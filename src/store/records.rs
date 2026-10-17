use crate::{
    Error, Result,
    domain::{
        photos,
        records::{
            self, CommitSeq, DomainResult, Failure, Lookup, MAX_RESULT_BYTES, Modify, Record,
            RecordName, ResolvedOperation, Revision, ZoneId,
        },
    },
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct Outcome {
    pub status: u16,
    pub body: Value,
}
impl Outcome {
    pub(crate) fn failure(failure: Failure) -> Self {
        Self {
            status: failure.status,
            body: failure.wire(),
        }
    }
    pub(crate) fn success(body: Value) -> Self {
        Self { status: 200, body }
    }
}

pub(crate) fn load(
    connection: &Connection,
    zone: &ZoneId,
    name: &RecordName,
) -> Result<Option<Record>> {
    let row=connection.query_row("SELECT record_type, revision, seq, fields_json, deleted, events_json FROM records WHERE namespace_id=1 AND zone_name=?1 AND record_name=?2",
        params![zone.zone_name,name.0], |row|Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,String>(3)?,row.get::<_,bool>(4)?,row.get::<_,String>(5)?))).optional()?;
    row.map(|(record_type, revision, seq, fields, deleted, events)| {
        Ok(Record {
            events: serde_json::from_str(&events).map_err(|_| Error::RecoveryFailed)?,
            name: name.clone(),
            record_type,
            zone: zone.clone(),
            revision: Revision(revision),
            seq: CommitSeq(seq),
            fields: serde_json::from_str(&fields).map_err(|_| Error::RecoveryFailed)?,
            deleted,
        })
    })
    .transpose()
}
fn zone_exists(connection: &Connection, zone: &ZoneId) -> Result<bool> {
    Ok(connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM zones WHERE namespace_id=1 AND zone_name=?1 AND owner=?2)",
        params![zone.zone_name, zone.owner_record_name],
        |row| row.get(0),
    )?)
}
fn bounded(value: &Value) -> Result<bool> {
    Ok(serde_json::to_vec(value)
        .map_err(|_| Error::RecoveryFailed)?
        .len()
        <= MAX_RESULT_BYTES)
}

pub(crate) fn lookup(connection: &mut Connection, lookup: Lookup) -> Result<Outcome> {
    if lookup.snapshot_token.is_some() {
        return Ok(Outcome::failure(Failure::unsupported()));
    }
    let tx = connection.transaction()?;
    let mut results = Vec::new();
    let mut bytes = 0usize;
    for (zone, name) in lookup.records {
        let record = load(&tx, &zone, &name)?;
        let value = if let Some(mut record) = record.filter(|record| !record.deleted) {
            if let Some(keys) = &lookup.desired_keys {
                record.fields.retain(|name, _| keys.contains(name));
            }
            record.wire()
        } else {
            Failure::unknown().item(&name.0, &zone)
        };
        bytes = bytes.saturating_add(
            serde_json::to_vec(&value)
                .map_err(|_| Error::RecoveryFailed)?
                .len(),
        );
        if bytes > MAX_RESULT_BYTES {
            return Ok(Outcome::failure(Failure::limit()));
        }
        results.push(value);
    }
    tx.commit()?;
    Ok(Outcome::success(json!({"records":results})))
}

fn fingerprint(request: &Modify) -> Result<[u8; 32]> {
    let mut logical = serde_json::to_value(request).map_err(|_| Error::RecoveryFailed)?;
    if let Some(operations) = logical["operations"].as_array_mut() {
        for operation in operations {
            if let Some(fields) = operation["fields"].as_object_mut() {
                for field in fields.values_mut() {
                    if field.get("type").and_then(Value::as_str) == Some("ASSETID")
                        && let Some(value) = field["value"].as_object_mut()
                    {
                        value.retain(|key, _| {
                            matches!(
                                key.as_str(),
                                "assetId" | "size" | "fileChecksum" | "checksumAlgorithm"
                            )
                        });
                    }
                }
            }
        }
    }
    let bytes = serde_json::to_vec(&logical).map_err(|_| Error::RecoveryFailed)?;
    let mut hasher = Sha256::new();
    hasher.update(b"syncserver/record-operation/v1\0");
    hasher.update(bytes);
    Ok(hasher.finalize().into())
}
fn receipt(
    connection: &Connection,
    key: &str,
    index: i64,
    hash: &[u8; 32],
) -> Result<DomainResult<Option<Value>>> {
    let row=connection.query_row("SELECT request_fingerprint,result_json FROM operation_receipts WHERE namespace_id=1 AND operation_id=?1 AND item_index=?2",params![key,index],
        |row|Ok((row.get::<_,Vec<u8>>(0)?,row.get::<_,Option<String>>(1)?))).optional()?;
    match row {
        Some((saved, _)) if saved != hash => Ok(Err(Failure::new(
            409,
            "IDEMPOTENCY_CONFLICT",
            "Idempotency key is bound to another request",
        ))),
        Some((_, Some(value))) => Ok(Ok(Some(
            serde_json::from_str(&value).map_err(|_| Error::RecoveryFailed)?,
        ))),
        _ => Ok(Ok(None)),
    }
}
fn save_receipt(
    tx: &Transaction<'_>,
    key: &str,
    index: i64,
    hash: &[u8; 32],
    value: Option<&Value>,
) -> Result<()> {
    let value = value
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| Error::RecoveryFailed)?;
    tx.execute("INSERT INTO operation_receipts(namespace_id,operation_id,item_index,request_fingerprint,result_json) VALUES(1,?1,?2,?3,?4) ON CONFLICT(namespace_id,operation_id,item_index) DO UPDATE SET result_json=excluded.result_json",
        params![key,index,hash.as_slice(),value])?;
    Ok(())
}

pub(crate) fn modify(
    connection: &mut Connection,
    mut request: Modify,
    guard: &super::leases::Guard<'_>,
) -> Result<Outcome> {
    guard.mutation(connection, &request)?;
    let mut input_errors = Vec::new();
    for op in &mut request.operations {
        let mut fields = op
            .fields
            .iter()
            .filter_map(|(name, field)| field.clone().map(|field| (name.clone(), field)))
            .collect();
        let error = match super::assets::normalize(connection, &mut fields, guard) {
            Ok(()) => None,
            Err(Error::Protocol(error)) => Some(error),
            Err(error) => return Err(error),
        };
        if request.atomic {
            if let Some(error) = error {
                return Ok(Outcome::failure(error));
            }
            input_errors.push(None);
        } else {
            input_errors.push(error);
        }
        // Temporary transport credentials do not identify a logical operation,
        // including a child rejected before it can become a record version.
        for field in fields.values_mut() {
            if field.kind == "ASSETID"
                && let Some(value) = field.value.as_object_mut()
            {
                value.remove("receipt");
                value.remove("downloadURL");
            }
        }
        for (name, field) in fields {
            op.fields.insert(name, Some(field));
        }
    }
    let hash = fingerprint(&request)?;
    let key = request.idempotency_key.as_deref();
    if let Some(key) = key {
        match receipt(connection, key, -1, &hash)? {
            Err(error) => return Ok(Outcome::failure(error)),
            Ok(Some(value)) => return Ok(Outcome::success(value)),
            Ok(None) => {}
        }
    }
    let mut budget = 0usize;
    for (index, op) in request.operations.iter().enumerate() {
        // Count cached items when resuming, and read indexed JSON lengths for
        // unfinished operations before allocating full records or committing.
        let cached = key
            .map(|key| receipt(connection, key, index as i64, &hash))
            .transpose()?;
        let value = match cached {
            Some(Ok(Some(value))) => value,
            Some(Err(error)) => return Ok(Outcome::failure(error)),
            _ => {
                let bytes:Option<i64>=connection.query_row("SELECT length(CAST(fields_json AS BLOB)) FROM records WHERE namespace_id=1 AND zone_name=?1 AND record_name=?2",params![op.zone.zone_name,op.record_name.0],|row|row.get(0)).optional()?;
                if budget.saturating_add(bytes.unwrap_or(0) as usize) > MAX_RESULT_BYTES {
                    return Ok(Outcome::failure(Failure::limit()));
                }
                if let Some(error) = &input_errors[index] {
                    error.item(&op.record_name.0, &op.zone)
                } else {
                    let current = load(connection, &op.zone, &op.record_name)?;
                    match records::apply(op, current.as_ref()) {
                        Ok(record) => record.wire(),
                        Err(error) => error.item(&op.record_name.0, &op.zone),
                    }
                }
            }
        };
        let mut preview = value;
        let bytes = match super::assets::render_bounded(connection, &mut preview, guard) {
            Ok(size) => size + 4096,
            Err(Error::Protocol(f)) => return Ok(Outcome::failure(f)),
            Err(e) => return Err(e),
        };
        budget = budget.saturating_add(bytes);
        if budget > MAX_RESULT_BYTES {
            return Ok(Outcome::failure(Failure::limit()));
        }
    }
    if request.atomic {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let records = match apply(&tx, &request.operations, true, guard)? {
            Ok(records) => records,
            Err(error) => return Ok(Outcome::failure(error)),
        };
        let result = json!({"records":records.iter().map(Record::wire).collect::<Vec<_>>()});
        if !bounded(&result)? {
            return Ok(Outcome::failure(Failure::limit()));
        }
        if let Some(key) = key {
            save_receipt(&tx, key, -1, &hash, Some(&result))?;
        }
        guard.mutation(&tx, &request)?;
        tx.commit()?;
        return Ok(Outcome::success(result));
    }
    if let Some(key) = key {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        guard.mutation(&tx, &request)?;
        save_receipt(&tx, key, -1, &hash, None)?;
        tx.commit()?;
    }
    let mut results = Vec::new();
    // Each transaction also records its result. A retry of a Pending batch resumes
    // only items lacking a receipt, even after a process crash or response loss.
    for (index, op) in request.operations.iter().enumerate() {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        guard.mutation(&tx, &request)?;
        if let Some(key) = key {
            match receipt(&tx, key, index as i64, &hash)? {
                Err(error) => return Ok(Outcome::failure(error)),
                Ok(Some(value)) => {
                    results.push(value);
                    tx.commit()?;
                    continue;
                }
                Ok(None) => {}
            }
        }
        tx.execute_batch("SAVEPOINT record_child")?;
        let result = if let Some(error) = &input_errors[index] {
            tx.execute_batch("ROLLBACK TO record_child")?;
            error.item(&op.record_name.0, &op.zone)
        } else {
            match apply(&tx, std::slice::from_ref(op), false, guard)? {
                Ok(records) => records[0].wire(),
                Err(error) => {
                    tx.execute_batch("ROLLBACK TO record_child")?;
                    error.item(&op.record_name.0, &op.zone)
                }
            }
        };
        tx.execute_batch("RELEASE record_child")?;
        if let Some(key) = key {
            save_receipt(&tx, key, index as i64, &hash, Some(&result))?;
        }
        guard.mutation(&tx, &request)?;
        tx.commit()?;
        results.push(result);
    }
    let result = json!({"records":results});
    if let Some(key) = key {
        let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        guard.mutation(&tx, &request)?;
        save_receipt(&tx, key, -1, &hash, Some(&result))?;
        guard.mutation(&tx, &request)?;
        tx.commit()?;
    }
    Ok(Outcome::success(result))
}

fn apply(
    tx: &Transaction<'_>,
    operations: &[ResolvedOperation],
    atomic: bool,
    guard: &super::leases::Guard<'_>,
) -> Result<DomainResult<Vec<Record>>> {
    let used_before = super::storage::usage(tx, guard)?.used_bytes;
    let zone = &operations[0].zone;
    if !zone_exists(tx, zone)? {
        return Ok(Err(Failure::unknown()));
    }
    let mut before = BTreeMap::new();
    let mut after = BTreeMap::new();
    let mut changed = Vec::new();
    let mut bytes = 0usize;
    for op in operations {
        let current = load(tx, &op.zone, &op.record_name)?;
        if let Some(current) = &current {
            bytes = bytes.saturating_add(
                serde_json::to_vec(&current.fields)
                    .map_err(|_| Error::RecoveryFailed)?
                    .len(),
            );
            if bytes > MAX_RESULT_BYTES {
                return Ok(Err(Failure::limit()));
            }
            before.insert(current.name.0.clone(), current.clone());
        }
        let record = match records::apply(op, current.as_ref()) {
            Ok(record) => record,
            Err(error) => return Ok(Err(error)),
        };
        after.insert(record.name.0.clone(), record.clone());
        changed.push(record);
    }
    // Fetch affected partners by primary key, never scan the zone's photo library.
    let mut pending: Vec<String> = before
        .values()
        .chain(after.values())
        .filter_map(photos::neighbor)
        .collect();
    let mut visited: BTreeSet<_> = after.keys().cloned().collect();
    while let Some(name) = pending.pop() {
        if !visited.insert(name.clone()) {
            continue;
        }
        if let Some(record) = load(tx, zone, &RecordName(name.clone()))? {
            bytes = bytes.saturating_add(
                serde_json::to_vec(&record.fields)
                    .map_err(|_| Error::RecoveryFailed)?
                    .len(),
            );
            if bytes > MAX_RESULT_BYTES {
                return Ok(Err(Failure::limit()));
            }
            if let Some(partner) = photos::neighbor(&record) {
                pending.push(partner);
            }
            before.insert(name.clone(), record.clone());
            after.insert(name, record);
        }
    }
    let old: Vec<_> = before.values().cloned().collect();
    let new: Vec<_> = after.values().cloned().collect();
    let bindings = match photos::validate_graph(&old, &new, operations, atomic) {
        Ok(bindings) => bindings,
        Err(error) => return Ok(Err(error)),
    };
    let affected: BTreeSet<_> = old
        .iter()
        .chain(new.iter())
        .filter(|record| record.record_type == "CPLAsset")
        .map(|record| record.name.0.clone())
        .collect();
    for binding in &bindings {
        let other:Option<String>=tx.query_row("SELECT photo_id FROM photo_bindings WHERE namespace_id=1 AND zone_name=?1 AND relative_path=?2",params![zone.zone_name,binding.relative_path],|row|row.get(0)).optional()?;
        if other.is_some_and(|id| id != binding.photo_id && !affected.contains(&id)) {
            return Ok(Err(Failure::new(
                409,
                "PATH_CONFLICT",
                "An active photo already uses this relative path",
            )));
        }
    }
    for id in affected {
        tx.execute(
            "DELETE FROM photo_bindings WHERE namespace_id=1 AND zone_name=?1 AND photo_id=?2",
            params![zone.zone_name, id],
        )?;
    }
    // Assign sequence numbers only in the same transaction as all projections,
    // immutable versions, change rows and the operation receipt.
    let committed_at_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::RecoveryFailed)?
        .as_millis()
        .try_into()
        .map_err(|_| Error::RecoveryFailed)?;
    for (record, operation) in changed.iter_mut().zip(operations) {
        let seq: i64 = tx.query_row(
            "SELECT last_committed_seq FROM namespace_counters WHERE namespace_id=1",
            [],
            |row| row.get(0),
        )?;
        record.seq = CommitSeq(seq.checked_add(1).ok_or(Error::RecoveryFailed)?);
        record.revision = Revision(Uuid::new_v4().to_string());
        if let Err(error) = crate::domain::events::assign(
            record,
            before.get(&record.name.0),
            operation,
            committed_at_ms,
        ) {
            return Ok(Err(error));
        }
        let events = serde_json::to_string(&record.events).map_err(|_| Error::RecoveryFailed)?;
        let fields = serde_json::to_string(&record.fields).map_err(|_| Error::RecoveryFailed)?;
        tx.execute(
            "UPDATE namespace_counters SET last_committed_seq=?1 WHERE namespace_id=1",
            [record.seq.0],
        )?;
        tx.execute("INSERT INTO records(namespace_id,zone_name,record_name,record_type,revision,seq,fields_json,deleted,events_json) VALUES(1,?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(namespace_id,zone_name,record_name) DO UPDATE SET revision=excluded.revision,seq=excluded.seq,fields_json=excluded.fields_json,deleted=excluded.deleted,events_json=excluded.events_json",
            params![record.zone.zone_name,record.name.0,record.record_type,record.revision.0,record.seq.0,fields,record.deleted,events])?;
        tx.execute(
            "INSERT INTO record_versions(namespace_id,zone_name,record_name,seq,record_type,revision,fields_json,deleted,events_json) VALUES(1,?1,?2,?3,?4,?5,?6,?7,?8)",
            params![
                record.zone.zone_name,
                record.name.0,
                record.seq.0,
                record.record_type,
                record.revision.0,
                fields,
                record.deleted,
                events
            ],
        )?;
        tx.execute(
            "INSERT INTO changes VALUES(1,?1,?2,?3)",
            params![record.seq.0, record.zone.zone_name, record.name.0],
        )?;
    }
    for binding in bindings {
        tx.execute(
            "INSERT INTO photo_bindings(namespace_id,zone_name,photo_id,master_record_name,
                relative_path,content_generation,original_asset_id,thumbnail_asset_id)
             VALUES(1,?1,?2,?3,?4,?5,?6,?7)",
            params![
                zone.zone_name,
                binding.photo_id,
                binding.master_name,
                binding.relative_path,
                binding.generation.to_string(),
                binding.original_asset_id.to_string(),
                binding.thumbnail_asset_id.to_string()
            ],
        )?;
        // The console index is derived in this same transaction, so a listing can
        // never describe a photo that the graph does not actually contain.
        let index = super::storage::index_row(tx, &zone.zone_name, &binding.photo_id)?;
        super::storage::write_index_row(tx, &index)?;
    }
    if let Err(error) = super::storage::check_record_growth(tx, guard, used_before)? {
        return Ok(Err(error));
    }
    Ok(Ok(changed))
}
