//! Account-local resource accounting; never touches object bodies or sync counters.
use super::leases::Guard;
use crate::{Error, Result, domain::records::Failure};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub quota_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub available_bytes: u64,
    /// False when the figure came from a foreign account, where the lease deadline
    /// could not be compared against the owning store's monotonic tick.
    ///
    /// This stays in process. `GET /v1/storage` is a published contract and must not
    /// grow a field; the administration console reports the flag explicitly instead.
    #[serde(skip_serializing, default = "own_store_is_exact")]
    pub reserved_exact: bool,
    #[serde(flatten)]
    pub media_usage: MediaUsage,
}

/// A usage report produced by the account's own store is exact by construction.
fn own_store_is_exact() -> bool {
    true
}

#[derive(Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaUsage {
    pub image_bytes: u64,
    pub video_bytes: u64,
    pub other_bytes: u64,
}

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum MediaKind {
    Image,
    Video,
    Other,
}

impl MediaKind {
    pub(crate) fn as_i64(self) -> i64 {
        match self {
            MediaKind::Image => 0,
            MediaKind::Video => 1,
            MediaKind::Other => 2,
        }
    }
}

/// The directory part of a photo's relative path, or the empty string for a photo
/// that sits at the root. A rename moves the photo between albums because the index
/// stores the same derived value the gallery groups by.
pub(crate) fn album_of(relative_path: &str) -> String {
    match relative_path.rfind('/') {
        Some(at) => relative_path[..at].to_owned(),
        None => String::new(),
    }
}

pub(crate) fn media_kind(mime: &str, path: Option<&str>) -> MediaKind {
    if mime.starts_with("image/") {
        return MediaKind::Image;
    }
    if mime.starts_with("video/") {
        return MediaKind::Video;
    }
    let extension = path
        .and_then(|path| std::path::Path::new(path).extension())
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "jpg" | "jpeg" | "jpe" | "png" | "apng" | "gif" | "webp" | "bmp" | "dib" | "tif"
        | "tiff" | "heic" | "heif" | "avif" | "jxl" | "ico" | "svg" | "dng" | "cr2" | "cr3"
        | "nef" | "nrw" | "arw" | "srf" | "sr2" | "orf" | "rw2" | "raf" | "pef" | "srw" => {
            MediaKind::Image
        }
        "mp4" | "m4v" | "mov" | "mkv" | "webm" | "avi" | "mpeg" | "mpg" | "m2v" | "m2ts"
        | "mts" | "ts" | "3gp" | "3g2" | "ogv" | "wmv" | "asf" | "flv" | "vob" => MediaKind::Video,
        _ => MediaKind::Other,
    }
}

fn logical_usage(db: &Connection) -> Result<MediaUsage> {
    let mut statement = db.prepare(
        "SELECT a.size, COALESCE(original.mime_type,a.mime_type),
             COALESCE(b.relative_path,CASE WHEN json_extract(r.fields_json,'$.relativePath.type')='STRING'
                 THEN json_extract(r.fields_json,'$.relativePath.value') END)
         FROM records r
         JOIN json_each(r.fields_json) f
         JOIN assets a ON a.namespace_id=r.namespace_id
             AND a.asset_id=json_extract(f.value,'$.value.assetId')
         LEFT JOIN assets original ON original.namespace_id=r.namespace_id
             AND json_extract(r.fields_json,'$.resOriginalRes.type')='ASSETID'
             AND original.asset_id=json_extract(r.fields_json,'$.resOriginalRes.value.assetId')
         LEFT JOIN photo_bindings b ON b.namespace_id=r.namespace_id AND b.zone_name=r.zone_name
             AND b.master_record_name=r.record_name
         WHERE r.namespace_id=1 AND r.deleted=0 AND json_extract(f.value,'$.type')='ASSETID'"
    )?;
    let mut rows = statement.query([])?;
    let mut usage = MediaUsage::default();
    while let Some(row) = rows.next()? {
        let size = u64::try_from(row.get::<_, i64>(0)?).map_err(|_| Error::RecoveryFailed)?;
        let mime: String = row.get(1)?;
        let path: Option<String> = row.get(2)?;
        let bytes = match media_kind(&mime, path.as_deref()) {
            MediaKind::Image => &mut usage.image_bytes,
            MediaKind::Video => &mut usage.video_bytes,
            MediaKind::Other => &mut usage.other_bytes,
        };
        *bytes = bytes.checked_add(size).ok_or(Error::RecoveryFailed)?;
    }
    Ok(usage)
}

impl Usage {
    /// A ceiling with nothing consumed and nothing in flight. Used when an account's
    /// database cannot be read, so a single failure does not blank the deployment.
    pub(crate) fn zero(quota_bytes: u64) -> Self {
        Self {
            quota_bytes,
            used_bytes: 0,
            reserved_bytes: 0,
            available_bytes: quota_bytes,
            reserved_exact: false,
            media_usage: MediaUsage::default(),
        }
    }
}

pub(super) fn usage(db: &Connection, guard: &Guard<'_>) -> Result<Usage> {
    usage_at(
        db,
        guard.quota_bytes.load(std::sync::atomic::Ordering::Relaxed),
        crate::jobs::now_ms()?,
        Some(guard.clock.tick()?),
    )
}

/// Usage for an account whose store this process does not hold open. The lease
/// deadline is a per-store monotonic tick, so a foreign read can only judge
/// reservations by wall clock; the resulting figure is an upper bound and the
/// caller must report it as such.
pub(crate) fn usage_foreign(db: &Connection, quota_bytes: u64) -> Result<Usage> {
    usage_at(db, quota_bytes, crate::jobs::now_ms()?, None)
}

fn usage_at(db: &Connection, quota_bytes: u64, now_ms: i64, tick_ms: Option<i64>) -> Result<Usage> {
    let media_usage = logical_usage(db)?;
    let used_bytes = media_usage
        .image_bytes
        .checked_add(media_usage.video_bytes)
        .and_then(|bytes| bytes.checked_add(media_usage.other_bytes))
        .ok_or(Error::RecoveryFailed)?;
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
             AND s.expires_at_ms>?1 AND (?2 IS NULL OR l.deadline_tick_ms>?2)",
        params![now_ms, tick_ms],
        |row| row.get(0),
    )?;
    let reserved_bytes = u64::try_from(reserved).map_err(|_| Error::RecoveryFailed)?;
    Ok(Usage {
        quota_bytes,
        used_bytes,
        reserved_bytes,
        reserved_exact: tick_ms.is_some(),
        available_bytes: quota_bytes
            .saturating_sub(used_bytes)
            .saturating_sub(reserved_bytes),
        media_usage,
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

/// The derived display fields of one photo, computed once from the trusted record
/// and asset rows so the console index and the quota classification cannot drift.
pub(crate) struct IndexRow {
    pub photo_id: String,
    pub relative_path: String,
    pub asset_date_ms: Option<i64>,
    pub favorite: bool,
    pub hidden: bool,
    pub album: String,
    pub media_kind: MediaKind,
}

/// Reads the display fields for one photo from the rows that are already trusted.
/// `fields_json` is the committed `CPLAsset` projection; the media classification
/// uses the server's own asset MIME, never anything the client asserted.
pub(crate) fn index_row(tx: &Connection, zone: &str, photo_id: &str) -> Result<IndexRow> {
    let row: (String, Option<i64>, i64, i64) = tx.query_row(
        "SELECT r.fields_json,
                CASE WHEN json_extract(r.fields_json,'$.assetDate.type')='TIMESTAMP'
                    THEN json_extract(r.fields_json,'$.assetDate.value') END,
                COALESCE(json_extract(r.fields_json,'$.isFavorite.value'),0),
                COALESCE(json_extract(r.fields_json,'$.isHidden.value'),0)
         FROM records r JOIN photo_bindings b
           ON b.namespace_id=r.namespace_id AND b.zone_name=r.zone_name AND b.photo_id=r.record_name
         WHERE r.namespace_id=1 AND r.zone_name=?1 AND r.record_name=?2",
        params![zone, photo_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    let relative_path: String = tx.query_row(
        "SELECT relative_path FROM photo_bindings
         WHERE namespace_id=1 AND zone_name=?1 AND photo_id=?2",
        params![zone, photo_id],
        |row| row.get(0),
    )?;
    let mime: Option<String> = tx
        .query_row(
            "SELECT a.mime_type FROM photo_bindings b
             JOIN assets a ON a.namespace_id=b.namespace_id AND a.asset_id=b.original_asset_id
             WHERE b.namespace_id=1 AND b.zone_name=?1 AND b.photo_id=?2",
            params![zone, photo_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(IndexRow {
        photo_id: photo_id.to_owned(),
        album: album_of(&relative_path),
        media_kind: media_kind(mime.as_deref().unwrap_or_default(), Some(&relative_path)),
        relative_path,
        asset_date_ms: row.1,
        favorite: row.2 != 0,
        hidden: row.3 != 0,
    })
}

/// Fills the console index columns for one photo. Called from the same transaction
/// that wrote the binding, so the index cannot disagree with the photo graph.
pub(crate) fn write_index_row(tx: &Connection, row: &IndexRow) -> Result<()> {
    tx.execute(
        "UPDATE photo_bindings SET asset_date_ms=?3, favorite=?4, hidden=?5, album=?6, media_kind=?7
         WHERE namespace_id=1 AND photo_id=?1 AND relative_path=?2",
        params![
            row.photo_id,
            row.relative_path,
            row.asset_date_ms,
            i64::from(row.favorite),
            i64::from(row.hidden),
            row.album,
            row.media_kind.as_i64(),
        ],
    )?;
    Ok(())
}

/// Backfills the console index after the v6 migration, using the same derivation as
/// the write path rather than a second copy of the rules in SQL.
pub(crate) fn backfill_photo_index(tx: &Connection) -> Result<()> {
    let photos: Vec<(String, String)> = {
        let mut statement = tx.prepare(
            "SELECT zone_name, photo_id FROM photo_bindings
             WHERE namespace_id=1 ORDER BY zone_name, photo_id",
        )?;
        statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (zone, photo_id) in photos {
        write_index_row(tx, &index_row(tx, &zone, &photo_id)?)?;
    }
    Ok(())
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
    fn media_kind_uses_trusted_original_mime_then_case_insensitive_path() {
        for (mime, path, expected) in [
            ("image/png", Some("movie.mp4"), MediaKind::Image),
            ("video/mp4", Some("photo.png"), MediaKind::Video),
            (
                "application/octet-stream",
                Some("clips/视频.MOV"),
                MediaKind::Video,
            ),
            (
                "application/octet-stream",
                Some("pictures/raw.HEIC"),
                MediaKind::Image,
            ),
            (
                "application/octet-stream",
                Some("data.bin"),
                MediaKind::Other,
            ),
            ("application/octet-stream", None, MediaKind::Other),
        ] {
            assert_eq!(media_kind(mime, path), expected);
        }
    }

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
        let quota = std::sync::atomic::AtomicU64::new(config.limits.user_storage_quota_bytes);
        let clock = Clock::new();
        let guard = Guard {
            clock: &clock,
            config: &config,
            generation,
            codec: &codec,
            quota_bytes: &quota,
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
                usage_at(&db, 10, crate::jobs::now_ms().unwrap(), Some(deadline))
                    .unwrap()
                    .reserved_bytes,
                0
            );
            assert_eq!(
                usage_at(&db, 10, session.expires_at_ms, Some(clock.tick().unwrap()))
                    .unwrap()
                    .reserved_bytes,
                0
            );
        }
    }
}
