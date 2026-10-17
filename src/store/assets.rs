//! Asset/session metadata. This module never reads or hashes object bodies.
use super::{
    leases::{Guard, lost, mismatch},
    storage,
};
use crate::{
    Error, Result,
    domain::{
        assets::{Asset, AssetCommand, AssetReply, Capability, Receipt, Reserve, Role, Session},
        leases::LeaseProof,
        records::{Failure, Fields, identifier},
    },
};
use base64::{Engine, engine::general_purpose::STANDARD};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use uuid::Uuid;
const SESSION_TTL_MS: i64 = 86_400_000;
const URL_TTL_SECS: i64 = 900;
fn time(ms: i64) -> Result<String> {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .map_err(|_| Error::RecoveryFailed)?
        .format(&time::format_description::well_known::Rfc3339)
        .map_err(|_| Error::RecoveryFailed)
}
pub(crate) fn session(db: &Connection, id: Uuid) -> Result<Session> {
    let row=db.query_row("SELECT state,role,expected_size,expected_sha256,lease_id,fence,intent,expires_at_ms,asset_id,error,staging_name FROM upload_sessions WHERE namespace_id=1 AND session_id=?1",[id.to_string()],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)? as u64,r.get::<_,Option<String>>(3)?,r.get::<_,String>(4)?,r.get::<_,i64>(5)?,r.get::<_,String>(6)?,r.get::<_,i64>(7)?,r.get::<_,Option<String>>(8)?,r.get::<_,Option<String>>(9)?,r.get::<_,Option<String>>(10)?))).optional()?.ok_or_else(Failure::unknown)?;
    let (state, role, size, sha, id_lease, fence, intent, expires, asset, error, staging) = row;
    Ok(Session {
        id,
        state,
        role: match role.as_str() {
            "original" => Role::Original,
            "thumbnail" => Role::Thumbnail,
            _ => return Err(Error::RecoveryFailed),
        },
        expected_size: size,
        expected_sha256: sha,
        lease_id: id_lease,
        fence: fence.to_string(),
        intent,
        expires_at_ms: expires,
        asset_id: asset
            .map(|s| Uuid::parse_str(&s).map_err(|_| Error::RecoveryFailed))
            .transpose()?,
        error,
        staging_name: staging,
    })
}
pub(crate) fn load(db: &Connection, id: Uuid) -> Result<Asset> {
    db.query_row("SELECT sha256,size,mime_type,width,height,thumbnail_validated FROM assets WHERE namespace_id=1 AND asset_id=?1",[id.to_string()],|r|Ok(Asset{asset_id:id,sha256:r.get(0)?,size:r.get::<_,i64>(1)? as u64,mime_type:r.get(2)?,width:r.get(3)?,height:r.get(4)?,thumbnail_validated:r.get(5)?})).optional()?.ok_or_else(||Failure::unknown().into())
}
fn valid_role(asset: &Asset, role: &Role) -> Result<()> {
    if *role == Role::Thumbnail && !asset.thumbnail_validated {
        return Err(Failure::unknown().into());
    }
    Ok(())
}
pub(crate) fn descriptor(asset: &Asset, role: Role, guard: &Guard<'_>) -> Result<Value> {
    valid_role(asset, &role)?;
    let expires = crate::jobs::now_ms()? / 1000 + URL_TTL_SECS;
    let receipt = guard.codec.seal(
        "asset-receipt",
        &Receipt {
            namespace: guard.config.namespace.clone(),
            generation: guard.generation,
            asset_id: asset.asset_id,
            sha256: asset.sha256.clone(),
            size: asset.size,
        },
    )?;
    let capability = guard.codec.seal(
        "download",
        &Capability {
            namespace: guard.config.namespace.clone(),
            generation: guard.generation,
            id: asset.asset_id,
            role: role.clone(),
            expires,
            lease_id: None,
            fence: None,
            intent: None,
        },
    )?;
    let mut url = guard
        .config
        .public_base_url
        .join(&format!("v1/assets/{}/content", asset.asset_id))
        .map_err(|_| Error::RecoveryFailed)?;
    url.query_pairs_mut()
        .append_pair("role", role.as_str())
        .append_pair("expires", &expires.to_string())
        .append_pair("token", &capability);
    let checksum = STANDARD.encode(hex_bytes(&asset.sha256)?);
    let mut value = json!({"assetId":asset.asset_id,"size":asset.size,"fileChecksum":checksum,"checksumAlgorithm":"SHA256","mimeType":asset.mime_type,"downloadURL":url.as_str(),"receipt":receipt});
    if let Some(width) = asset.width {
        value["width"] = json!(width);
    }
    if let Some(height) = asset.height {
        value["height"] = json!(height);
    }
    Ok(value)
}
fn hex_bytes(value: &str) -> Result<Vec<u8>> {
    if !crate::domain::assets::digest(value) {
        return Err(Error::RecoveryFailed);
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            u8::from_str_radix(
                std::str::from_utf8(pair).map_err(|_| Error::RecoveryFailed)?,
                16,
            )
            .map_err(|_| Error::RecoveryFailed)
        })
        .collect()
}
fn upload_url(session: &Session, proof: &LeaseProof, guard: &Guard<'_>) -> Result<Value> {
    let expires = (crate::jobs::now_ms()? / 1000 + URL_TTL_SECS).min(session.expires_at_ms / 1000);
    let token = guard.codec.seal(
        "upload",
        &Capability {
            namespace: guard.config.namespace.clone(),
            generation: guard.generation,
            id: session.id,
            role: session.role.clone(),
            expires,
            lease_id: Some(proof.id.clone()),
            fence: Some(proof.fence.clone()),
            intent: Some(session.intent.clone()),
        },
    )?;
    let mut url = guard
        .config
        .public_base_url
        .join(&format!("v1/uploads/{}/content", session.id))
        .map_err(|_| Error::RecoveryFailed)?;
    url.query_pairs_mut()
        .append_pair("expires", &expires.to_string())
        .append_pair("token", &token);
    Ok(
        json!({"url":url.as_str(),"_syncserver":{"sessionId":session.id,"state":session.state,"expiresAt":time(session.expires_at_ms)?,"urlExpiresAt":time(expires*1000)?}}),
    )
}
fn authorize(
    db: &Connection,
    session: &Session,
    proof: &LeaseProof,
    guard: &Guard<'_>,
) -> Result<()> {
    let active = guard.active(db, proof)?;
    if session.lease_id != active.id || session.fence != active.fence.to_string() {
        return Err(lost().into());
    }
    if active.verified.is_none() {
        return Err(Failure::new(
            409,
            "SYNC_NOT_CURRENT",
            "Complete current progress required",
        )
        .into());
    }
    let phase = if session.intent == "create" {
        "create"
    } else {
        "modify"
    };
    if phase != active.phase {
        return Err(mismatch().into());
    }
    if session.expires_at_ms <= crate::jobs::now_ms()? {
        return Err(Failure::new(409, "UPLOAD_EXPIRED", "Upload session expired").into());
    }
    Ok(())
}
fn reserve(
    db: &mut Connection,
    request: Reserve,
    proof: LeaseProof,
    key: Option<String>,
    guard: &Guard<'_>,
) -> Result<Value> {
    request.validate(
        guard.config.limits.max_asset_bytes,
        guard.config.limits.thumbnail_max_bytes,
    )?;
    if key
        .as_ref()
        .is_some_and(|s| !identifier(s) || s.len() > 128)
    {
        return Err(Failure::bad().into());
    }
    let mut hash = Sha256::new();
    hash.update(b"syncserver/upload-reservation/v1\0");
    hash.update(serde_json::to_vec(&request).map_err(|_| Error::RecoveryFailed)?);
    let fingerprint: [u8; 32] = hash.finalize().into();
    let tx = db.transaction()?;
    let active = guard.active(&tx, &proof)?;
    if active.verified.is_none() {
        return Err(Failure::new(
            409,
            "SYNC_NOT_CURRENT",
            "Complete current progress required",
        )
        .into());
    }
    if !active.zones.contains(&request.zone) {
        return Err(Failure::bad().into());
    }
    for token in &request.tokens {
        if active.phase
            != if token.options.intent == "create" {
                "create"
            } else {
                "modify"
            }
        {
            return Err(mismatch().into());
        }
    }
    let cached=key.as_ref().map(|key|tx.query_row("SELECT fingerprint,session_ids FROM upload_reservations WHERE namespace_id=1 AND operation_id=?1",[key],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,String>(1)?))).optional()).transpose()?.flatten();
    let ids: Vec<Uuid> = if let Some((saved, ids)) = cached {
        if saved != fingerprint {
            return Err(Failure::new(
                409,
                "IDEMPOTENCY_CONFLICT",
                "Reservation key belongs to another request",
            )
            .into());
        }
        serde_json::from_str(&ids).map_err(|_| Error::RecoveryFailed)?
    } else {
        let mut additional = 0u64;
        for token in &request.tokens {
            additional = additional
                .checked_add(storage::reservation_bytes(&tx, &request.zone, token)?)
                .ok_or_else(|| {
                    Failure::new(
                        409,
                        "STORAGE_QUOTA_EXCEEDED",
                        "Account storage quota exceeded",
                    )
                })?;
        }
        storage::ensure_capacity(&tx, guard, additional)?;
        let mut ids = Vec::new();
        let expires = crate::jobs::now_ms()? + SESSION_TTL_MS;
        for token in &request.tokens {
            let id = Uuid::new_v4();
            tx.execute("INSERT INTO upload_sessions(namespace_id,session_id,state,zone_name,record_name,field_name,role,intent,expected_size,expected_sha256,lease_id,fence,expires_at_ms) VALUES(1,?1,'Reserved',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)",params![id.to_string(),request.zone.zone_name,token.record_name,token.field_name,token.options.role.as_str(),token.options.intent,token.options.expected_size as i64,token.options.expected_sha256,proof.id,active.fence,expires])?;
            ids.push(id);
        }
        if let Some(key) = &key {
            tx.execute(
                "INSERT INTO upload_reservations VALUES(1,?1,?2,?3)",
                params![
                    key,
                    fingerprint.as_slice(),
                    serde_json::to_string(&ids).map_err(|_| Error::RecoveryFailed)?
                ],
            )?;
        }
        ids
    };
    let mut tokens = Vec::new();
    for (id, token) in ids.into_iter().zip(&request.tokens) {
        let session = session(&tx, id)?;
        let mut value = if session.state == "Published" {
            json!({"_syncserver":{"sessionId":id,"state":"Published"},"asset":descriptor(&load(&tx,session.asset_id.ok_or(Error::RecoveryFailed)?)?,session.role,guard)?})
        } else {
            authorize(&tx, &session, &proof, guard)?;
            upload_url(&session, &proof, guard)?
        };
        value["recordName"] = json!(token.record_name);
        value["fieldName"] = json!(token.field_name);
        tokens.push(value);
    }
    guard.active(&tx, &proof)?;
    tx.commit()?;
    Ok(json!({"tokens":tokens}))
}
pub(crate) fn execute(
    db: &mut Connection,
    command: AssetCommand,
    guard: &Guard<'_>,
) -> Result<AssetReply> {
    let value = match command {
        AssetCommand::Storage => {
            let tx = db.transaction()?;
            let value = serde_json::to_value(storage::usage(&tx, guard)?)
                .map_err(|_| Error::RecoveryFailed)?;
            tx.commit()?;
            value
        }
        AssetCommand::Reserve(request, proof, key) => reserve(db, request, proof, key, guard)?,
        AssetCommand::Status(id) => {
            let s = session(db, id)?;
            let asset = if s.state == "Published" {
                Some(descriptor(
                    &load(db, s.asset_id.ok_or(Error::RecoveryFailed)?)?,
                    s.role,
                    guard,
                )?)
            } else {
                None
            };
            json!({"sessionId":id,"state":s.state,"asset":asset,"serverErrorCode":s.error})
        }
        AssetCommand::Renew(id, proof) => {
            let tx = db.transaction()?;
            let s = session(&tx, id)?;
            authorize(&tx, &s, &proof, guard)?;
            if s.state != "Reserved" {
                return Err(Failure::new(
                    409,
                    "UPLOAD_STATE_CONFLICT",
                    "Only Reserved sessions can renew",
                )
                .into());
            }
            let result = upload_url(&s, &proof, guard)?;
            tx.commit()?;
            result
        }
        AssetCommand::Begin(id, proof, staging) => {
            let tx = db.transaction()?;
            let mut s = session(&tx, id)?;
            authorize(&tx, &s, &proof, guard)?;
            let error = match s.state.as_str() {
                "Reserved" => None,
                "Receiving" | "PublishPrepared" => Some("UPLOAD_BUSY"),
                "Published" => Some("UPLOAD_ALREADY_COMPLETED"),
                _ => Some("UPLOAD_STATE_CONFLICT"),
            };
            if let Some(code) = error {
                return Err(
                    Failure::new(409, code, "Upload session cannot accept another body").into(),
                );
            }
            tx.execute("UPDATE upload_sessions SET state='Receiving',staging_name=?1 WHERE namespace_id=1 AND session_id=?2",params![staging,id.to_string()])?;
            tx.commit()?;
            s.state = "Receiving".into();
            s.staging_name = Some(staging);
            return Ok(AssetReply::Session(s));
        }
        AssetCommand::Validate(id, proof) => {
            let s = session(db, id)?;
            authorize(db, &s, &proof, guard)?;
            if !matches!(s.state.as_str(), "Receiving" | "PublishPrepared") {
                return Err(Failure::new(
                    409,
                    "UPLOAD_STATE_CONFLICT",
                    "Upload is no longer active",
                )
                .into());
            }
            return Ok(AssetReply::Session(s));
        }
        AssetCommand::Fail(id, code) => {
            db.execute("UPDATE upload_sessions SET state='Failed',error=?1 WHERE namespace_id=1 AND session_id=?2 AND state IN('Reserved','Receiving','PublishPrepared')",params![code,id.to_string()])?;
            db.execute("UPDATE publish_intents SET state='Quarantined' WHERE namespace_id=1 AND session_id=?1 AND state='Prepared'",[id.to_string()])?;
            json!({"failed":true})
        }
        AssetCommand::Prepare(verified, proof) => {
            let tx = db.transaction()?;
            let s = session(&tx, verified.session_id)?;
            authorize(&tx, &s, &proof, guard)?;
            if s.state != "Receiving" || s.staging_name.as_ref() != Some(&verified.staging_name) {
                return Err(
                    Failure::new(409, "UPLOAD_STATE_CONFLICT", "Receiving owner mismatch").into(),
                );
            }
            if verified.asset.size != s.expected_size {
                return Err(Failure::bad().into());
            }
            tx.execute(
                "INSERT INTO publish_intents VALUES(1,?1,?2,?3,'Prepared')",
                params![
                    s.id.to_string(),
                    verified.staging_name,
                    serde_json::to_string(&verified.asset).map_err(|_| Error::RecoveryFailed)?
                ],
            )?;
            tx.execute("UPDATE upload_sessions SET state='PublishPrepared' WHERE namespace_id=1 AND session_id=?1",[s.id.to_string()])?;
            guard.active(&tx, &proof)?;
            tx.commit()?;
            return Ok(AssetReply::Verified(verified));
        }
        AssetCommand::Complete(id, proof) => {
            let tx = db.transaction()?;
            let s = session(&tx, id)?;
            authorize(&tx, &s, &proof, guard)?;
            if s.state != "PublishPrepared" {
                return Err(
                    Failure::new(409, "UPLOAD_STATE_CONFLICT", "Publish intent required").into(),
                );
            }
            let data:String=tx.query_row("SELECT asset_json FROM publish_intents WHERE namespace_id=1 AND session_id=?1 AND state='Prepared'",[id.to_string()],|r|r.get(0))?;
            let mut asset: Asset =
                serde_json::from_str(&data).map_err(|_| Error::RecoveryFailed)?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT asset_id FROM assets WHERE namespace_id=1 AND sha256=?1 AND size=?2",
                    params![asset.sha256, asset.size as i64],
                    |r| r.get(0),
                )
                .optional()?;
            if let Some(existing) = existing {
                asset.asset_id = Uuid::parse_str(&existing).map_err(|_| Error::RecoveryFailed)?;
                if asset.thumbnail_validated {
                    tx.execute("UPDATE assets SET thumbnail_validated=1,mime_type=?1,width=?2,height=?3 WHERE namespace_id=1 AND asset_id=?4",params![asset.mime_type,asset.width,asset.height,existing])?;
                }
            } else {
                tx.execute(
                    "INSERT INTO assets VALUES(1,?1,?2,?3,?4,?5,?6,?7)",
                    params![
                        asset.asset_id.to_string(),
                        asset.sha256,
                        asset.size as i64,
                        asset.mime_type,
                        asset.width,
                        asset.height,
                        asset.thumbnail_validated
                    ],
                )?;
            }
            tx.execute("UPDATE upload_sessions SET state='Published',asset_id=?1,error=NULL WHERE namespace_id=1 AND session_id=?2",params![asset.asset_id.to_string(),id.to_string()])?;
            tx.execute("UPDATE publish_intents SET state='Published' WHERE namespace_id=1 AND session_id=?1",[id.to_string()])?;
            guard.active(&tx, &proof)?;
            let result = json!({"singleFile":descriptor(&load(&tx,asset.asset_id)?,s.role,guard)?});
            tx.commit()?;
            result
        }
        AssetCommand::Describe(id, role) => descriptor(&load(db, id)?, role, guard)?,
        AssetCommand::Metadata(id, role) => {
            let asset = load(db, id)?;
            valid_role(&asset, &role)?;
            return Ok(AssetReply::Asset(asset));
        }
    };
    Ok(AssetReply::Json(value))
}
// New descriptor inputs are authenticated and reduced before fingerprints,
// projections or history are written. Retained references never store URL/receipt.
pub(crate) fn normalize(db: &Connection, fields: &mut Fields, guard: &Guard<'_>) -> Result<()> {
    for (name, field) in fields {
        if field.kind == "ASSETID_LIST" {
            return Err(Failure::unsupported().into());
        }
        if field.kind != "ASSETID" {
            continue;
        }
        let value = field.value.as_object().ok_or_else(Failure::bad)?;
        let allowed = [
            "assetId",
            "size",
            "fileChecksum",
            "checksumAlgorithm",
            "mimeType",
            "width",
            "height",
            "downloadURL",
            "receipt",
        ];
        if value.keys().any(|key| !allowed.contains(&key.as_str())) {
            return Err(Failure::bad().into());
        }
        let id = Uuid::parse_str(
            value
                .get("assetId")
                .and_then(Value::as_str)
                .ok_or_else(Failure::bad)?,
        )
        .map_err(|_| Failure::bad())?;
        let asset = load(db, id)?;
        let role = if name == "resJPEGThumbRes" {
            Role::Thumbnail
        } else {
            Role::Original
        };
        valid_role(&asset, &role)?;
        let receipt: Receipt = guard.codec.unseal(
            value
                .get("receipt")
                .and_then(Value::as_str)
                .ok_or_else(Failure::bad)?,
            "asset-receipt",
        )?;
        if receipt.namespace != guard.config.namespace
            || receipt.generation != guard.generation
            || receipt.asset_id != id
            || receipt.sha256 != asset.sha256
            || receipt.size != asset.size
            || value.get("size").and_then(Value::as_u64) != Some(asset.size)
            || value.get("checksumAlgorithm").and_then(Value::as_str) != Some("SHA256")
            || value.get("fileChecksum").and_then(Value::as_str)
                != Some(STANDARD.encode(hex_bytes(&asset.sha256)?).as_str())
        {
            return Err(Failure::bad().into());
        }
        for (key, expected) in [
            ("mimeType", Some(json!(asset.mime_type))),
            ("width", asset.width.map(|v| json!(v))),
            ("height", asset.height.map(|v| json!(v))),
        ] {
            if let Some(provided) = value.get(key)
                && expected.as_ref() != Some(provided)
            {
                return Err(Failure::bad().into());
            }
        }
        if value
            .get("downloadURL")
            .is_some_and(|value| !value.is_string())
        {
            return Err(Failure::bad().into());
        }
        let mut stable = descriptor(&asset, role, guard)?;
        stable
            .as_object_mut()
            .ok_or(Error::RecoveryFailed)?
            .retain(|key, _| !matches!(key.as_str(), "receipt" | "downloadURL"));
        field.value = stable;
    }
    Ok(())
}
pub(crate) fn render(
    db: &Connection,
    value: &mut Value,
    guard: &Guard<'_>,
    remaining: &mut usize,
) -> Result<()> {
    match value {
        Value::Array(items) => {
            for item in items {
                render(db, item, guard, remaining)?;
            }
        }
        Value::Object(map) => {
            if let Some(fields) = map.get_mut("fields").and_then(Value::as_object_mut) {
                for (name, field) in fields {
                    if field.get("type").and_then(Value::as_str) == Some("ASSETID") {
                        let id = field["value"]["assetId"]
                            .as_str()
                            .and_then(|s| Uuid::parse_str(s).ok())
                            .ok_or(Error::RecoveryFailed)?;
                        let fresh = descriptor(
                            &load(db, id).map_err(|error| match error {
                                Error::Protocol(_) => Error::RecoveryFailed,
                                other => other,
                            })?,
                            if name == "resJPEGThumbRes" {
                                Role::Thumbnail
                            } else {
                                Role::Original
                            },
                            guard,
                        )?;
                        let added = serde_json::to_vec(
                            &json!({"downloadURL":fresh["downloadURL"],"receipt":fresh["receipt"]}),
                        )
                        .map_err(|_| Error::RecoveryFailed)?
                        .len();
                        let mut old_transport = serde_json::Map::new();
                        for key in ["downloadURL", "receipt"] {
                            if let Some(value) = field["value"].get(key) {
                                old_transport.insert(key.to_owned(), value.clone());
                            }
                        }
                        let old_size = serde_json::to_vec(&old_transport)
                            .map_err(|_| Error::RecoveryFailed)?
                            .len();
                        let added = added.saturating_sub(old_size) + 2;
                        if added > *remaining {
                            return Err(Failure::limit().into());
                        }
                        *remaining -= added;
                        let stored = field["value"]
                            .as_object_mut()
                            .ok_or(Error::RecoveryFailed)?;
                        stored.insert("downloadURL".into(), fresh["downloadURL"].clone());
                        stored.insert("receipt".into(), fresh["receipt"].clone());
                    }
                }
            }
            for (key, nested) in map {
                if key != "fields" {
                    render(db, nested, guard, remaining)?;
                }
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn render_bounded(
    db: &Connection,
    value: &mut Value,
    guard: &Guard<'_>,
) -> Result<usize> {
    let initial = serde_json::to_vec(value)
        .map_err(|_| Error::RecoveryFailed)?
        .len();
    let mut remaining = crate::domain::records::MAX_RESULT_BYTES
        .checked_sub(initial)
        .ok_or_else(Failure::limit)?;
    render(db, value, guard, &mut remaining)?;
    Ok(crate::domain::records::MAX_RESULT_BYTES - remaining)
}

#[cfg(test)]
mod tests {
    use crate::assets::*;
    use crate::domain::assets::Role;
    use crate::{
        Config,
        domain::{
            assets::{AssetCommand, AssetReply, Reserve, UploadOptions, UploadToken},
            leases::{Acquire, Check, Control, LeaseProof, Phase, ZoneToken},
            records::ZoneId,
            sync::{Claims, Cursor},
        },
        store::{
            assets,
            leases::{Clock, Guard},
            migrations,
        },
        tokens::TokenCodec,
    };
    use axum::body::Bytes;
    use rusqlite::Connection;
    use std::fs::File;
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use uuid::Uuid;
    #[test]
    fn recovery_settles_each_durable_publication_boundary_without_new_old_fence_success() {
        for boundary in [
            "receiving",
            "prepared",
            "linked",
            "object",
            "committed",
            "two_intents",
            "failed_duplicate",
        ] {
            let root = tempfile::tempdir().unwrap();
            let data = root.path().join("data");
            let config = Config::from_pairs([
                ("SYNC_DATA_DIR", data.display().to_string()),
                ("SYNC_API_KEY_FILE", "/unused/api".into()),
                ("SYNC_SIGNING_KEY_FILE", "/unused/signing".into()),
            ])
            .unwrap();
            let _directory = crate::store::files::DataDirectory::acquire(&data).unwrap();
            let mut db = Connection::open(data.join("metadata.sqlite")).unwrap();
            let generation = migrations::initialize(&mut db, &config, &[1; 32]).unwrap();
            let codec = TokenCodec::new([1; 32]);
            let clock = Clock::new();
            let guard = Guard {
                clock: &clock,
                config: &config,
                generation,
                codec: &codec,
            };
            let lease = guard
                .control(
                    &mut db,
                    Control::Acquire(Acquire {
                        device_id: "a".into(),
                        zones: vec![ZoneId::default()],
                    }),
                )
                .unwrap()
                .body;
            let proof = LeaseProof {
                id: lease["leaseId"].as_str().unwrap().into(),
                token: lease["leaseToken"].as_str().unwrap().into(),
                fence: lease["fencingToken"].as_str().unwrap().into(),
            };
            let token = codec
                .encode(&Claims {
                    version: 1,
                    namespace: config.namespace.clone(),
                    generation,
                    zone: Some(ZoneId::default()),
                    cursor: Cursor::Zone {
                        position: 0,
                        watermark: None,
                    },
                })
                .unwrap();
            guard
                .control(
                    &mut db,
                    Control::Check(
                        proof.clone(),
                        Check {
                            zone_tokens: vec![ZoneToken {
                                zone: ZoneId::default(),
                                sync_token: token,
                            }],
                            local_ready: true,
                        },
                    ),
                )
                .unwrap();
            for phase in ["modify", "create"] {
                guard
                    .control(
                        &mut db,
                        Control::Phase(
                            proof.clone(),
                            Phase {
                                phase: phase.into(),
                            },
                        ),
                    )
                    .unwrap();
            }
            let reserve = Reserve {
                zone: ZoneId::default(),
                tokens: vec![UploadToken {
                    record_name: "master".into(),
                    record_type: "CPLMaster".into(),
                    field_name: "resOriginalRes".into(),
                    options: UploadOptions {
                        role: Role::Original,
                        intent: "create".into(),
                        expected_size: 3,
                        expected_sha256: None,
                    },
                }],
            };
            let AssetReply::Json(reserved) = assets::execute(
                &mut db,
                AssetCommand::Reserve(reserve.clone(), proof.clone(), None),
                &guard,
            )
            .unwrap() else {
                panic!()
            };
            let id = Uuid::parse_str(
                reserved["tokens"][0]["_syncserver"]["sessionId"]
                    .as_str()
                    .unwrap(),
            )
            .unwrap();
            let name = format!("{id}-{}", Uuid::new_v4());
            let AssetReply::Session(session) = assets::execute(
                &mut db,
                AssetCommand::Begin(id, proof.clone(), name),
                &guard,
            )
            .unwrap() else {
                panic!()
            };
            let (sender, receiver) = mpsc::channel(1);
            sender.blocking_send(Bytes::from_static(b"abc")).unwrap();
            drop(sender);
            let verified = Sink {
                root: data.clone(),
                session,
                receiver,
                metrics: Arc::new(Metrics::default()),
            }
            .receive()
            .unwrap();
            if boundary != "receiving" {
                assets::execute(
                    &mut db,
                    AssetCommand::Prepare(verified.clone(), proof.clone()),
                    &guard,
                )
                .unwrap();
            }
            if boundary == "linked" {
                std::fs::hard_link(
                    temporary(&data, &verified.staging_name),
                    object(&data, &verified.asset.sha256),
                )
                .unwrap();
                File::open(data.join("objects"))
                    .unwrap()
                    .sync_all()
                    .unwrap();
            }
            if matches!(boundary, "object" | "committed") {
                publish(&data, &verified).unwrap();
            }
            if boundary == "committed" {
                assets::execute(&mut db, AssetCommand::Complete(id, proof.clone()), &guard)
                    .unwrap();
            }
            if matches!(boundary, "two_intents" | "failed_duplicate") {
                let mut duplicate = reserve.clone();
                duplicate.tokens[0].record_name = "second-master".into();
                let AssetReply::Json(reserved) = assets::execute(
                    &mut db,
                    AssetCommand::Reserve(duplicate, proof.clone(), None),
                    &guard,
                )
                .unwrap() else {
                    panic!()
                };
                let second_id = Uuid::parse_str(
                    reserved["tokens"][0]["_syncserver"]["sessionId"]
                        .as_str()
                        .unwrap(),
                )
                .unwrap();
                let second_name = format!("{second_id}-{}", Uuid::new_v4());
                let AssetReply::Session(second) = assets::execute(
                    &mut db,
                    AssetCommand::Begin(second_id, proof.clone(), second_name),
                    &guard,
                )
                .unwrap() else {
                    panic!()
                };
                let (sender, receiver) = mpsc::channel(1);
                sender.blocking_send(Bytes::from_static(b"abc")).unwrap();
                drop(sender);
                let second = Sink {
                    root: data.clone(),
                    session: second,
                    receiver,
                    metrics: Arc::new(Metrics::default()),
                }
                .receive()
                .unwrap();
                assets::execute(
                    &mut db,
                    AssetCommand::Prepare(second, proof.clone()),
                    &guard,
                )
                .unwrap();
                std::fs::hard_link(
                    temporary(&data, &verified.staging_name),
                    object(&data, &verified.asset.sha256),
                )
                .unwrap();
                if boundary == "failed_duplicate" {
                    assets::execute(
                        &mut db,
                        AssetCommand::Fail(id, "UPLOAD_INTERRUPTED".into()),
                        &guard,
                    )
                    .unwrap();
                }
            }
            drop(db);
            let mut db = Connection::open(data.join("metadata.sqlite")).unwrap();
            let report = crate::jobs::recover(&mut db, &data).unwrap();
            let state = assets::session(&db, id).unwrap();
            if boundary == "committed" {
                assert_eq!(state.state, "Published");
                assert_eq!(report.asset_bytes_hashed, 0);
                assert!(assets::load(&db, state.asset_id.unwrap()).is_ok());
            } else {
                assert_eq!(state.state, "Failed");
                assert!(state.asset_id.is_none());
                assert_eq!(
                    report.asset_bytes_hashed,
                    if boundary == "receiving" { 0 } else { 3 }
                );
            }
            assert_eq!(std::fs::read_dir(data.join("staging")).unwrap().count(), 0);
            assert_eq!(
                std::fs::read_dir(data.join("objects")).unwrap().count(),
                if boundary == "receiving" { 0 } else { 1 }
            );
            crate::jobs::finish(&mut db, report.run_id).unwrap();
        }
    }
}
