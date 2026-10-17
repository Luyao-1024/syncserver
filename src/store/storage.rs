//! Account-local resource accounting; never touches object bodies or sync counters.
use super::leases::Guard;
use crate::{Error, Result, domain::records::Failure};
use rusqlite::{Connection, params};
use serde::Serialize;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Usage {
    pub quota_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub available_bytes: u64,
}

pub(super) fn usage(db: &Connection, guard: &Guard<'_>) -> Result<Usage> {
    usage_at(
        db,
        guard.config.limits.user_storage_quota_bytes,
        crate::jobs::now_ms()?,
        guard.clock.tick()?,
    )
}

fn usage_at(db: &Connection, quota_bytes: u64, now_ms: i64, tick_ms: i64) -> Result<Usage> {
    let used: i64 = db.query_row(
        "SELECT COALESCE(SUM(a.size),0) FROM records r
         JOIN json_each(r.fields_json) f
         JOIN assets a ON a.namespace_id=r.namespace_id
             AND a.asset_id=json_extract(f.value,'$.value.assetId')
         WHERE r.namespace_id=1 AND r.deleted=0 AND json_extract(f.value,'$.type')='ASSETID'",
        [],
        |row| row.get(0),
    )?;
    // A replaced/expired lease cannot publish. Do not let abandoned reservations
    // block the new holder, even before periodic session maintenance runs.
    let reserved: i64 = db.query_row(
        "SELECT COALESCE(SUM(MAX(s.expected_size-COALESCE(a.size,0),0)),0)
         FROM upload_sessions s
         JOIN upload_lease l ON l.namespace_id=s.namespace_id
             AND l.lease_id=s.lease_id AND l.fence=s.fence
         LEFT JOIN records r ON r.namespace_id=s.namespace_id AND r.zone_name=s.zone_name
             AND r.record_name=s.record_name AND r.deleted=0 AND s.intent='update'
         LEFT JOIN json_each(r.fields_json) f ON f.key=s.field_name
             AND json_extract(f.value,'$.type')='ASSETID'
         LEFT JOIN assets a ON a.namespace_id=r.namespace_id
             AND a.asset_id=json_extract(f.value,'$.value.assetId')
         WHERE s.namespace_id=1 AND s.state IN('Reserved','Receiving','PublishPrepared')
             AND s.expires_at_ms>?1 AND l.deadline_tick_ms>?2",
        params![now_ms, tick_ms],
        |row| row.get(0),
    )?;
    let used_bytes = u64::try_from(used).map_err(|_| Error::RecoveryFailed)?;
    let reserved_bytes = u64::try_from(reserved).map_err(|_| Error::RecoveryFailed)?;
    Ok(Usage {
        quota_bytes,
        used_bytes,
        reserved_bytes,
        available_bytes: quota_bytes
            .saturating_sub(used_bytes)
            .saturating_sub(reserved_bytes),
    })
}

/// Call inside the transaction that allocates or converts the reservation.
pub(super) fn ensure_capacity(db: &Connection, guard: &Guard<'_>, additional: u64) -> Result<()> {
    let current = usage(db, guard)?;
    let total = current
        .used_bytes
        .checked_add(current.reserved_bytes)
        .and_then(|bytes| bytes.checked_add(additional));
    if additional > 0 && total.is_none_or(|bytes| bytes > current.quota_bytes) {
        return Err(Failure::new(
            409,
            "STORAGE_QUOTA_EXCEEDED",
            "Account storage quota exceeded",
        )
        .into());
    }
    Ok(())
}

/// Reserve only positive logical growth when replacing an existing field.
pub(super) fn reservation_bytes(
    db: &Connection,
    zone: &crate::domain::records::ZoneId,
    token: &crate::domain::assets::UploadToken,
) -> Result<u64> {
    let current: i64 = if token.options.intent == "update" {
        db.query_row(
            "SELECT COALESCE(SUM(a.size),0) FROM records r
             JOIN json_each(r.fields_json) f
             JOIN assets a ON a.namespace_id=r.namespace_id
                 AND a.asset_id=json_extract(f.value,'$.value.assetId')
             WHERE r.namespace_id=1 AND r.zone_name=?1 AND r.record_name=?2
                 AND r.deleted=0 AND f.key=?3 AND json_extract(f.value,'$.type')='ASSETID'",
            params![zone.zone_name, token.record_name, token.field_name],
            |r| r.get(0),
        )?
    } else {
        0
    };
    Ok(token
        .options
        .expected_size
        .saturating_sub(u64::try_from(current).map_err(|_| Error::RecoveryFailed)?))
}

/// All record mutations, including reuse without an upload, pass this final gate.
pub(super) fn check_record_growth(
    db: &Connection,
    guard: &Guard<'_>,
    before: u64,
) -> Result<crate::domain::records::DomainResult<()>> {
    let after = usage(db, guard)?;
    if after.used_bytes > before
        && after
            .used_bytes
            .checked_add(after.reserved_bytes)
            .is_none_or(|bytes| bytes > after.quota_bytes)
    {
        return Ok(Err(Failure::new(
            409,
            "STORAGE_QUOTA_EXCEEDED",
            "Account storage quota exceeded",
        )));
    }
    Ok(Ok(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Config,
        domain::{
            assets::{AssetCommand, AssetReply, Reserve},
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
    use serde_json::json;

    #[test]
    fn lease_and_session_expiration_release_reservations_before_maintenance() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::from_pairs([
            ("SYNC_DATA_DIR", "/unused/data"),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing"),
            ("SYNC_USER_STORAGE_QUOTA_BYTES", "10"),
        ])
        .unwrap();
        let mut db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
        let generation = migrations::initialize(&mut db, &config, &[1; 32]).unwrap();
        let codec = TokenCodec::new([1; 32]);
        let clock = Clock::new();
        let guard = Guard {
            clock: &clock,
            config: &config,
            generation,
            codec: &codec,
        };
        let acquired = guard
            .control(
                &mut db,
                Control::Acquire(Acquire {
                    device_id: "test".into(),
                    zones: vec![ZoneId::default()],
                }),
            )
            .unwrap()
            .body;
        let proof = LeaseProof {
            id: acquired["leaseId"].as_str().unwrap().into(),
            token: acquired["leaseToken"].as_str().unwrap().into(),
            fence: acquired["fencingToken"].as_str().unwrap().into(),
        };
        let sync_token = codec
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
                            sync_token,
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
        let request: Reserve = serde_json::from_value(json!({"tokens":[{
            "recordName":"master", "recordType":"CPLMaster", "fieldName":"resOriginalRes",
            "_syncserver":{"role":"original","intent":"create","expectedSize":10}
        }]}))
        .unwrap();
        let AssetReply::Json(reserved) = assets::execute(
            &mut db,
            AssetCommand::Reserve(request, proof.clone(), None),
            &guard,
        )
        .unwrap() else {
            panic!()
        };
        let id = uuid::Uuid::parse_str(
            reserved["tokens"][0]["_syncserver"]["sessionId"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
        let session = assets::session(&db, id).unwrap();
        let deadline: i64 = db
            .query_row(
                "SELECT deadline_tick_ms FROM upload_lease WHERE namespace_id=1",
                [],
                |r| r.get(0),
            )
            .unwrap();
        for receiving in [false, true] {
            if receiving {
                assets::execute(
                    &mut db,
                    AssetCommand::Begin(id, proof.clone(), "staging-owner".into()),
                    &guard,
                )
                .unwrap();
            }
            assert_eq!(usage(&db, &guard).unwrap().reserved_bytes, 10);
            assert_eq!(
                usage_at(&db, 10, crate::jobs::now_ms().unwrap(), deadline)
                    .unwrap()
                    .reserved_bytes,
                0
            );
            assert_eq!(
                usage_at(&db, 10, session.expires_at_ms, clock.tick().unwrap())
                    .unwrap()
                    .reserved_bytes,
                0
            );
        }
    }
}
