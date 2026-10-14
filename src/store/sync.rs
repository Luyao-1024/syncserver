//! Read-only fixed-watermark projections and indexed change feeds.
use super::records::{self, Outcome};
use crate::{
    Error, Result,
    domain::{
        Namespace,
        records::{CommitSeq, Failure, MAX_RESULT_BYTES, Record, RecordName, Revision, ZoneId},
        sync::{Claims, Cursor, DatabaseRequest, QueryRequest, Read, ZoneRequest, keys},
    },
    tokens::TokenCodec,
};
use rusqlite::{Connection, OptionalExtension, Row, Transaction, params};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use uuid::Uuid;
// Leave room for all tokens/control/error fields, also in a 20-zone response.
const RECORD_BUDGET: usize = MAX_RESULT_BYTES - 1_048_576;
pub(crate) const SNAPSHOT_SQL: &str = "SELECT v.record_name,v.record_type,v.revision,v.seq,v.fields_json,v.deleted,v.events_json FROM records r JOIN record_versions v ON v.namespace_id=r.namespace_id AND v.zone_name=r.zone_name AND v.record_name=r.record_name AND v.seq=(SELECT max(h.seq) FROM record_versions h WHERE h.namespace_id=r.namespace_id AND h.zone_name=r.zone_name AND h.record_name=r.record_name AND h.seq<=?3) WHERE r.namespace_id=1 AND r.zone_name=?1 AND r.record_name>?2 AND v.deleted=0 AND v.record_type=?4 ORDER BY r.record_name LIMIT ?5";
pub(crate) const ZONE_SQL: &str = "SELECT v.record_name,v.record_type,v.revision,v.seq,v.fields_json,v.deleted,v.events_json FROM changes c JOIN record_versions v ON v.namespace_id=c.namespace_id AND v.seq=c.seq WHERE c.namespace_id=1 AND c.zone_name=?1 AND c.seq>?2 AND c.seq<=?3 ORDER BY c.seq LIMIT ?4";
const LOOKUP_SQL: &str = "SELECT record_name,record_type,revision,seq,fields_json,deleted,events_json FROM record_versions WHERE namespace_id=1 AND zone_name=?1 AND record_name=?2 AND seq<=?3 ORDER BY seq DESC LIMIT 1";
struct Context<'a> {
    tx: Transaction<'a>,
    namespace: &'a Namespace,
    generation: Uuid,
    head: i64,
    codec: &'a TokenCodec,
    budget: usize,
    guard: &'a super::leases::Guard<'a>,
}
fn row_record(row: &Row<'_>, zone: &ZoneId) -> rusqlite::Result<Record> {
    let fields: String = row.get(4)?;
    let events: String = row.get(6)?;
    let parse = |error| {
        rusqlite::Error::FromSqlConversionFailure(4, rusqlite::types::Type::Text, Box::new(error))
    };
    Ok(Record {
        name: RecordName(row.get(0)?),
        record_type: row.get(1)?,
        revision: Revision(row.get(2)?),
        seq: CommitSeq(row.get(3)?),
        fields: serde_json::from_str(&fields).map_err(parse)?,
        deleted: row.get(5)?,
        events: serde_json::from_str(&events).map_err(parse)?,
        zone: zone.clone(),
    })
}
impl Context<'_> {
    fn zone(&self, zone: &ZoneId) -> Result<()> {
        if !self.tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM zones WHERE namespace_id=1 AND zone_name=?1 AND owner=?2)",
            params![zone.zone_name, zone.owner_record_name],
            |r| r.get::<_, bool>(0),
        )? {
            return Err(Failure::unknown().into());
        }
        Ok(())
    }
    fn decode(&self, token: &str, kind: &str, zone: Option<&ZoneId>) -> Result<Claims> {
        let claims = self.codec.decode(token, kind)?;
        if claims.namespace != *self.namespace || claims.zone.as_ref() != zone {
            return Err(Failure::bad().into());
        }
        let (position, watermark) = claims.cursor.bounds();
        if position < 0 || watermark < position {
            return Err(Failure::bad().into());
        }
        if claims.generation != self.generation || watermark > self.head {
            return Err(Failure::new(
                400,
                "EXPIRED_SYNC_TOKEN",
                "Token does not belong to the retained database generation or range",
            )
            .into());
        }
        Ok(claims)
    }
    fn token(&self, zone: Option<&ZoneId>, cursor: Cursor) -> Result<String> {
        self.codec.encode(&Claims {
            version: 1,
            namespace: self.namespace.clone(),
            generation: self.generation,
            zone: zone.cloned(),
            cursor,
        })
    }
    fn sync_token(&self, zone: &ZoneId, head: i64) -> Result<String> {
        self.token(
            Some(zone),
            Cursor::Zone {
                position: head,
                watermark: None,
            },
        )
    }
    fn projection(&self, zone: &ZoneId, name: &RecordName, head: i64) -> Result<Option<Record>> {
        Ok(self
            .tx
            .query_row(LOOKUP_SQL, params![zone.zone_name, name.0, head], |row| {
                row_record(row, zone)
            })
            .optional()?)
    }
    fn wire(&self, record: &mut Record, keys: &Option<BTreeSet<String>>) -> Result<Value> {
        if let Some(keys) = keys {
            record.fields.retain(|key, _| keys.contains(key));
        }
        let mut value = record.wire();
        super::assets::render_bounded(&self.tx, &mut value, self.guard)?;
        Ok(value)
    }
    fn snapshot(&self, zone: ZoneId) -> Result<Value> {
        self.zone(&zone)?;
        Ok(
            json!({"zoneID":zone,"snapshotToken":self.token(Some(&zone),Cursor::Snapshot{watermark:self.head})?,"syncToken":self.sync_token(&zone,self.head)?}),
        )
    }
    fn query(&mut self, request: QueryRequest) -> Result<Value> {
        self.zone(&request.zone)?;
        let desired = keys(&request.desired_keys)?;
        let mut head = self.head;
        let mut last = String::new();
        if let Some(token) = &request.snapshot_token {
            let claims = self.decode(token, "snapshot", Some(&request.zone))?;
            if let Cursor::Snapshot { watermark } = claims.cursor {
                head = watermark;
            }
        }
        if let Some(token) = &request.continuation_marker {
            let claims = self.decode(token, "query", Some(&request.zone))?;
            let Cursor::Query {
                watermark,
                record_type,
                desired_keys,
                last_record_name,
            } = claims.cursor
            else {
                return Err(Failure::bad().into());
            };
            if record_type != request.query.record_type
                || desired_keys != desired
                || (request.snapshot_token.is_some() && watermark != head)
                || !crate::domain::records::identifier(&last_record_name)
            {
                return Err(Failure::bad().into());
            }
            head = watermark;
            last = last_record_name;
        }
        let mut records = Vec::new();
        let mut more = false;
        {
            let mut statement = self.tx.prepare(SNAPSHOT_SQL)?;
            let mut rows = statement.query(params![
                request.zone.zone_name,
                last,
                head,
                request.query.record_type,
                (request.results_limit + 1) as i64
            ])?;
            while let Some(row) = rows.next()? {
                if records.len() == request.results_limit {
                    more = true;
                    break;
                }
                let mut record = row_record(row, &request.zone)?;
                let value = self.wire(&mut record, &desired)?;
                let size = serde_json::to_vec(&value)
                    .map_err(|_| Error::RecoveryFailed)?
                    .len();
                if size > self.budget {
                    if records.is_empty() {
                        return Err(Failure::limit().into());
                    }
                    more = true;
                    break;
                }
                self.budget -= size;
                last = record.name.0;
                records.push(value);
            }
        }
        let mut result =
            json!({"records":records,"syncToken":self.sync_token(&request.zone,head)?});
        if more {
            result["continuationMarker"] = json!(self.token(
                Some(&request.zone),
                Cursor::Query {
                    watermark: head,
                    record_type: request.query.record_type,
                    desired_keys: desired,
                    last_record_name: last
                }
            )?);
        }
        Ok(result)
    }
    fn lookup(&mut self, request: crate::domain::records::Lookup) -> Result<Value> {
        let Some(token) = &request.snapshot_token else {
            return Err(Failure::bad().into());
        };
        let zone = &request.records[0].0;
        self.zone(zone)?;
        let claims = self.decode(token, "snapshot", Some(zone))?;
        if request.records.iter().any(|(other, _)| other != zone) {
            return Err(Failure::bad().into());
        }
        let Cursor::Snapshot { watermark } = claims.cursor else {
            return Err(Failure::bad().into());
        };
        let mut results = Vec::new();
        for (zone, name) in &request.records {
            let value = match self
                .projection(zone, name, watermark)?
                .filter(|r| !r.deleted)
            {
                Some(mut record) => self.wire(&mut record, &request.desired_keys)?,
                None => Failure::unknown().item(&name.0, zone),
            };
            let size = serde_json::to_vec(&value)
                .map_err(|_| Error::RecoveryFailed)?
                .len();
            if size > self.budget {
                return Err(Failure::limit().into());
            }
            self.budget -= size;
            results.push(value);
        }
        Ok(json!({"records":results,"syncToken":self.sync_token(zone,watermark)?}))
    }
    fn zone_changes(&mut self, request: ZoneRequest) -> Result<Value> {
        self.zone(&request.zone)?;
        let desired = keys(&request.desired_keys)?;
        let mut position = 0;
        let mut head = self.head;
        if let Some(token) = &request.sync_token {
            let claims = self.decode(token, "zone", Some(&request.zone))?;
            let Cursor::Zone {
                position: p,
                watermark,
            } = claims.cursor
            else {
                return Err(Failure::bad().into());
            };
            position = p;
            head = watermark.unwrap_or(self.head);
        }
        let mut records = Vec::new();
        let mut more = false;
        {
            let mut stmt = self.tx.prepare(ZONE_SQL)?;
            let mut rows = stmt.query(params![
                request.zone.zone_name,
                position,
                head,
                (request.results_limit + 1) as i64
            ])?;
            while let Some(row) = rows.next()? {
                if records.len() == request.results_limit {
                    more = true;
                    break;
                }
                let mut record = row_record(row, &request.zone)?;
                let value = self.wire(&mut record, &desired)?;
                let size = serde_json::to_vec(&value)
                    .map_err(|_| Error::RecoveryFailed)?
                    .len();
                if size > self.budget {
                    if records.is_empty() {
                        return Err(Failure::limit().into());
                    }
                    more = true;
                    break;
                }
                self.budget -= size;
                position = record.seq.0;
                records.push(value);
            }
        }
        let cursor = if more {
            Cursor::Zone {
                position,
                watermark: Some(head),
            }
        } else {
            Cursor::Zone {
                position: head,
                watermark: None,
            }
        };
        Ok(
            json!({"zoneID":request.zone,"records":records,"moreComing":more,"syncToken":self.token(Some(&request.zone),cursor)?}),
        )
    }
    fn database(&self, request: DatabaseRequest) -> Result<Value> {
        if request.sync_token.is_none() {
            let mut stmt = self
                .tx
                .prepare("SELECT zone_name FROM zones WHERE namespace_id=1 ORDER BY zone_name")?;
            let zones=stmt.query_map([],|r|r.get::<_,String>(0))?.map(|zone|zone.map(|name|json!({"zoneID":ZoneId {zone_name:name,..Default::default()},"deleted":false}))).collect::<std::result::Result<Vec<_>,_>>()?;
            return Ok(
                json!({"zones":zones,"moreComing":false,"syncToken":self.token(None,Cursor::Database{position:self.head,watermark:None})?}),
            );
        }
        let claims = self.decode(
            request.sync_token.as_deref().unwrap_or_default(),
            "database",
            None,
        )?;
        let Cursor::Database {
            mut position,
            watermark,
        } = claims.cursor
        else {
            return Err(Failure::bad().into());
        };
        let head = watermark.unwrap_or(self.head);
        let mut stmt=self.tx.prepare("SELECT seq,zone_name FROM changes WHERE namespace_id=1 AND seq>?1 AND seq<=?2 ORDER BY seq LIMIT ?3")?;
        let mut rows = stmt.query(params![position, head, (request.results_limit + 1) as i64])?;
        let mut names = BTreeSet::new();
        let mut zones = Vec::new();
        let mut more = false;
        let mut scanned = 0;
        while let Some(row) = rows.next()? {
            if scanned == request.results_limit {
                more = true;
                break;
            }
            scanned += 1;
            position = row.get(0)?;
            let name: String = row.get(1)?;
            if names.insert(name.clone()) {
                zones.push(
                    json!({"zoneID":ZoneId{zone_name:name,..Default::default()},"deleted":false}),
                );
            }
        }
        let cursor = if more {
            Cursor::Database {
                position,
                watermark: Some(head),
            }
        } else {
            Cursor::Database {
                position: head,
                watermark: None,
            }
        };
        Ok(json!({"zones":zones,"moreComing":more,"syncToken":self.token(None,cursor)?}))
    }
}
pub(crate) fn read(
    connection: &mut Connection,
    request: Read,
    codec: &TokenCodec,
    namespace: &Namespace,
    generation: Uuid,
    guard: &super::leases::Guard<'_>,
) -> Result<Outcome> {
    request.validate()?;
    // Current lookup keeps its M2 response, and cannot nest transactions.
    if let Read::Lookup(lookup) = request {
        if lookup.snapshot_token.is_none() {
            return records::lookup(connection, lookup);
        }
        return run(
            connection,
            Read::Lookup(lookup),
            codec,
            namespace,
            generation,
            guard,
        );
    }
    run(connection, request, codec, namespace, generation, guard)
}
fn run(
    connection: &mut Connection,
    request: Read,
    codec: &TokenCodec,
    namespace: &Namespace,
    generation: Uuid,
    guard: &super::leases::Guard<'_>,
) -> Result<Outcome> {
    let tx = connection.transaction()?;
    let head = tx.query_row(
        "SELECT last_committed_seq FROM namespace_counters WHERE namespace_id=1",
        [],
        |r| r.get(0),
    )?;
    let mut context = Context {
        tx,
        namespace,
        generation,
        head,
        codec,
        budget: RECORD_BUDGET,
        guard,
    };
    let body = match request {
        Read::Batch(r) => {
            let mut results = Vec::new();
            for query in r.batch {
                match context.query(query) {
                    Ok(value) => results.push(value),
                    Err(Error::Protocol(e)) => results.push(e.wire()),
                    Err(e) => return Err(e),
                }
            }
            json!({"batch":results})
        }
        Read::Snapshot(r) => context.snapshot(r.zone)?,
        Read::Query(r) => context.query(r)?,
        Read::Lookup(r) => context.lookup(r)?,
        Read::Database(r) => context.database(r)?,
        Read::Zones(r) => {
            let mut results = Vec::new();
            for request in r.zones {
                let zone = request.zone.clone();
                match context.zone_changes(request) {
                    Ok(result) => results.push(result),
                    Err(Error::Protocol(error)) => {
                        let mut value = error.wire();
                        value["zoneID"] = json!(zone);
                        results.push(value);
                    }
                    Err(error) => return Err(error),
                }
            }
            json!({"zones":results})
        }
    };
    if serde_json::to_vec(&body)
        .map_err(|_| Error::RecoveryFailed)?
        .len()
        > MAX_RESULT_BYTES
    {
        return Err(Failure::limit().into());
    }
    context.tx.commit()?;
    Ok(Outcome::success(body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Config, store::migrations};
    fn database() -> (tempfile::TempDir, Connection, Namespace, Uuid) {
        let root = tempfile::tempdir().unwrap();
        let config = Config::from_pairs([
            ("SYNC_DATA_DIR", "/unused/data"),
            ("SYNC_API_KEY_FILE", "/unused/api"),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing"),
        ])
        .unwrap();
        let mut db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
        let generation = migrations::initialize(&mut db, &config, &[1; 32]).unwrap();
        (root, db, config.namespace, generation)
    }
    #[test]
    fn signed_wrong_scope_generation_future_and_partial_positions_are_distinct() {
        let (_root, mut db, namespace, generation) = database();
        let config = Config::from_pairs([
            ("SYNC_DATA_DIR", "/unused/data"),
            ("SYNC_API_KEY_FILE", "/unused/api"),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing"),
        ])
        .unwrap();
        let clock = super::super::leases::Clock::new();
        let codec = TokenCodec::new([1; 32]);
        let guard = super::super::leases::Guard {
            clock: &clock,
            config: &config,
            generation,
            codec: &codec,
        };
        let tx = db.transaction().unwrap();
        let context = Context {
            tx,
            namespace: &namespace,
            generation,
            head: 10,
            codec: &codec,
            budget: RECORD_BUDGET,
            guard: &guard,
        };
        let zone = ZoneId::default();
        let base = Claims {
            version: 1,
            namespace: namespace.clone(),
            generation,
            zone: Some(zone.clone()),
            cursor: Cursor::Zone {
                position: 10,
                watermark: None,
            },
        };
        let mut claims = base.clone();
        claims.namespace.container = "other".into();
        assert!(
            matches!(context.decode(&codec.encode(&claims).unwrap(),"zone",Some(&zone)),Err(Error::Protocol(e)) if e.code=="BAD_REQUEST")
        );
        for changed in [
            Claims {
                generation: Uuid::new_v4(),
                ..base.clone()
            },
            Claims {
                cursor: Cursor::Zone {
                    position: 11,
                    watermark: None,
                },
                ..base.clone()
            },
        ] {
            assert!(
                matches!(context.decode(&codec.encode(&changed).unwrap(),"zone",Some(&zone)),Err(Error::Protocol(e)) if e.code=="EXPIRED_SYNC_TOKEN")
            );
        }
        claims = base;
        claims.cursor = Cursor::Zone {
            position: 9,
            watermark: Some(8),
        };
        assert!(
            matches!(context.decode(&codec.encode(&claims).unwrap(),"zone",Some(&zone)),Err(Error::Protocol(e)) if e.code=="BAD_REQUEST")
        );
    }
    #[test]
    fn actual_snapshot_and_delta_statements_use_scope_and_range_indexes() {
        let (_root, db, _, _) = database();
        let query_plan = |sql: &str, values: &[&dyn rusqlite::ToSql]| {
            let mut stmt = db.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
            stmt.query_map(values, |r| r.get::<_, String>(3))
                .unwrap()
                .collect::<std::result::Result<Vec<_>, _>>()
                .unwrap()
                .join("\n")
        };
        let snapshot = query_plan(SNAPSHOT_SQL, &[&"PrimarySync", &"", &100, &"Album", &201]);
        assert!(snapshot.contains("record_name>"), "{snapshot}");
        assert!(snapshot.contains("record_versions_snapshot"), "{snapshot}");
        let delta = query_plan(ZONE_SQL, &[&"PrimarySync", &0, &100, &201]);
        assert!(
            delta.contains("changes_zone") && delta.contains("seq>"),
            "{delta}"
        );
        assert!(!delta.contains("SCAN "), "{delta}");
    }
}
