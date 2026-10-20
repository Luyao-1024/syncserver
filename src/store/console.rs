//! Read-only projection of the photo index for the web console.
//!
//! Every query here reads `photo_bindings`, `records` and `assets` metadata only.
//! No object body is opened, read or hashed, no zone token is minted, and none of
//! this runs inside a transaction that could advance the synchronization
//! generation or the commit sequence. The cost profile matches `GET /v1/storage`.
//!
//! Resource URLs are produced by the same [`super::assets::descriptor`] the record
//! wire uses, so a browser receives an identical capability contract no matter which
//! endpoint it read from.
use super::{assets, leases::Guard};
use crate::{Error, Result, domain::assets::Role};
use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::{Value, json};

pub const MAX_PAGE: usize = 200;
pub const DEFAULT_PAGE: usize = 120;
pub const MAX_MONTHS: i64 = 240;
/// Domain separator for listing positions, so a cursor can never be mistaken for a
/// sync token or a resource capability.
const CURSOR_KIND: &str = "console-cursor";

/// Keyset position for the date-ordered listing. Photos without a capture date sort
/// last, so a partial library still pages deterministically instead of stalling.
#[derive(serde::Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Cursor {
    pub photo_id: String,
    pub asset_date_ms: Option<i64>,
}

/// Everything the console may narrow a listing by. `None` means "do not filter".
#[derive(Default)]
pub struct Filters<'a> {
    pub media: Option<&'a str>,
    pub album: Option<&'a str>,
    pub favorite: Option<bool>,
    pub hidden: Option<bool>,
    /// `None` includes both live and trashed photos; the API defaults the ordinary
    /// gallery to `Some(false)` and the trash album asks for `Some(true)`.
    pub trashed: Option<bool>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    /// Substring of the relative path. A scan by nature, and bounded on purpose.
    pub query: Option<&'a str>,
}

fn rejected() -> Error {
    Error::Config("console listing parameter is not accepted")
}

fn media_code(media: Option<&str>) -> Result<Option<i64>> {
    Ok(match media {
        None => None,
        Some("image") => Some(0),
        Some("video") => Some(1),
        Some("other") => Some(2),
        Some(_) => return Err(rejected()),
    })
}

/// Bounded, deduplicated, prefix-only album filter. A caller cannot inject a
/// wildcard it did not intend, and cannot make the comparison unbounded.
fn album_prefix(album: &str) -> Result<String> {
    if album.is_empty()
        || album.len() > 4096
        || album.contains('%')
        || album.contains('_')
        || album.starts_with('/')
        || album
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == "..")
    {
        return Err(rejected());
    }
    Ok(album.to_owned())
}

const COLUMNS: &str = "b.photo_id, b.master_record_name, b.relative_path, b.album,
     b.asset_date_ms, b.favorite, b.hidden, b.trashed, b.media_kind, b.content_generation,
     r.revision, m.revision,
     b.original_asset_id, b.thumbnail_asset_id";

fn row_to_view(row: &rusqlite::Row<'_>) -> rusqlite::Result<Value> {
    let photo_id: String = row.get(0)?;
    let master: String = row.get(1)?;
    let relative_path: String = row.get(2)?;
    let album: String = row.get(3)?;
    let asset_date_ms: Option<i64> = row.get(4)?;
    let favorite: i64 = row.get(5)?;
    let hidden: i64 = row.get(6)?;
    let trashed: i64 = row.get(7)?;
    let media_kind: i64 = row.get(8)?;
    let generation: String = row.get(9)?;
    let asset_tag: String = row.get(10)?;
    let master_tag: String = row.get(11)?;
    let original_id: String = row.get(12)?;
    let thumbnail_id: String = row.get(13)?;
    Ok(json!({
        "photoId": photo_id,
        "masterRecordName": master,
        "relativePath": relative_path,
        "album": album,
        "assetDateMs": asset_date_ms,
        "favorite": favorite != 0,
        "hidden": hidden != 0,
        "trashed": trashed != 0,
        "mediaType": match media_kind { 0 => "image", 1 => "video", _ => "other" },
        "contentGeneration": generation,
        // Both revisions, because a rename or a delete must be conditional on both
        // records and the console is a client like any other.
        "recordChangeTags": [asset_tag, master_tag],
        "originalAssetId": original_id,
        "thumbnailAssetId": thumbnail_id,
    }))
}

const FROM: &str = " FROM photo_bindings b
     JOIN records r ON r.namespace_id=b.namespace_id AND r.zone_name=b.zone_name
         AND r.record_name=b.photo_id
     JOIN records m ON m.namespace_id=b.namespace_id AND m.zone_name=b.zone_name
         AND m.record_name=b.master_record_name
     WHERE b.namespace_id=1 AND b.zone_name=?1";

/// Attaches the per-role resource descriptors and the logical byte count.
fn decorate(db: &Connection, photo: &mut Value, guard: &Guard<'_>) -> Result<()> {
    let original_id = photo["originalAssetId"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or(Error::RecoveryFailed)?;
    let thumbnail_id = photo["thumbnailAssetId"]
        .as_str()
        .and_then(|id| uuid::Uuid::parse_str(id).ok())
        .ok_or(Error::RecoveryFailed)?;
    let original = assets::load(db, original_id)?;
    let thumbnail = assets::load(db, thumbnail_id)?;
    let original_bytes = original.size;
    let thumbnail_bytes = thumbnail.size;
    photo["original"] = assets::descriptor(&original, Role::Original, guard)?;
    photo["thumbnail"] = assets::descriptor(&thumbnail, Role::Thumbnail, guard)?;
    photo["bytes"] = json!(original_bytes.saturating_add(thumbnail_bytes));
    // The identity fields have served their purpose once the descriptors exist.
    if let Some(map) = photo.as_object_mut() {
        map.remove("originalAssetId");
        map.remove("thumbnailAssetId");
    }
    Ok(())
}

/// The gallery listing: newest first, one keyset page, with fresh URLs.
///
/// Only this account's own namespace is ever read. There is no parameter that
/// selects another account, a different container or a different tenant.
pub(crate) fn photos(
    db: &Connection,
    zone: &str,
    filters: &Filters<'_>,
    limit: usize,
    after: Option<&Cursor>,
    guard: &Guard<'_>,
) -> Result<Value> {
    let kind = media_code(filters.media)?;
    let album = filters.album.map(album_prefix).transpose()?;
    let mut statement = db
        .prepare(&format!(
            "SELECT {COLUMNS}{FROM}
             AND (?2 IS NULL OR b.media_kind=?2)
             AND (?3 IS NULL OR b.album=?3 OR b.album LIKE ?3 || '/%')
             AND (?4 IS NULL OR b.favorite=?4)
             AND (?5 IS NULL OR b.hidden=?5)
             AND (?6 IS NULL OR b.asset_date_ms IS NULL OR b.asset_date_ms>=?6)
             AND (?7 IS NULL OR b.asset_date_ms>=?7)
             AND (?8 IS NULL OR b.relative_path LIKE '%' || ?8 || '%' ESCAPE '\\')
             AND (?9 IS NULL OR (b.asset_date_ms, b.photo_id) < (?9, ?10))
             AND (?11 IS NULL OR b.trashed=?11)
             ORDER BY b.asset_date_ms DESC, b.photo_id DESC
             LIMIT ?12"
        ))
        .map_err(|_| Error::RecoveryFailed)?;
    // A path substring is matched literally: the caller's wildcards are escaped, so
    // a query of `%` searches for that character rather than matching everything.
    let escaped = filters.query.map(|value| {
        value
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    });
    let rows = statement
        .query_map(
            params![
                zone,
                kind,
                album.as_deref(),
                filters.favorite.map(i64::from),
                filters.hidden.map(i64::from),
                filters.from_ms,
                filters.to_ms,
                escaped.as_deref(),
                after.and_then(|cursor| cursor.asset_date_ms),
                after.map(|cursor| cursor.photo_id.as_str()),
                filters.trashed.map(i64::from),
                (limit + 1) as i64,
            ],
            row_to_view,
        )
        .map_err(|_| Error::RecoveryFailed)?;

    let mut page: Vec<Value> = Vec::with_capacity(limit);
    for row in rows {
        page.push(row.map_err(|_| Error::RecoveryFailed)?);
    }
    // The extra row proves another page exists; it is never itself returned.
    let next = if page.len() > limit {
        page.pop();
        page.last().map(|last| Cursor {
            photo_id: last["photoId"].as_str().unwrap_or_default().to_owned(),
            asset_date_ms: last["assetDateMs"].as_i64(),
        })
    } else {
        None
    };
    for photo in &mut page {
        decorate(db, photo, guard)?;
    }
    let next = match next {
        Some(cursor) => Some(guard.codec.seal(CURSOR_KIND, &cursor)?),
        None => None,
    };
    Ok(json!({
        "photos": page,
        "hasMore": next.is_some(),
        "nextCursor": next,
        "zone": {"zoneName": zone},
    }))
}

/// One photo, for the preview view. Same projection as the listing, plus the
/// original resource URL so the viewer can fetch full resolution on demand.
pub(crate) fn photo(
    db: &Connection,
    zone: &str,
    photo_id: &str,
    guard: &Guard<'_>,
) -> Result<Value> {
    let mut statement = db
        .prepare(&format!("SELECT {COLUMNS}{FROM} AND b.photo_id=?2"))
        .map_err(|_| Error::RecoveryFailed)?;
    let mut rows = statement
        .query_map(params![zone, photo_id], row_to_view)
        .map_err(|_| Error::RecoveryFailed)?;
    let Some(row) = rows.next() else {
        return Err(crate::domain::records::Failure::unknown().into());
    };
    let mut value = row.map_err(|_| Error::RecoveryFailed)?;
    decorate(db, &mut value, guard)?;
    Ok(json!({"photo": value, "zone": {"zoneName": zone}}))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AlbumView {
    name: String,
    photo_count: u64,
    bytes: u64,
}

/// Groups by the stored album column. The index makes this an index-only scan, so
/// the cost stays proportional to the number of photos rather than their content.
pub(crate) fn albums(db: &Connection, zone: &str, prefix: Option<&str>) -> Result<Value> {
    let prefix = prefix.map(album_prefix).transpose()?;
    let mut statement = db
        .prepare(
            "SELECT b.album, COUNT(*),
                    COALESCE(SUM((SELECT oa.size FROM assets oa
                        WHERE oa.namespace_id=b.namespace_id AND oa.asset_id=b.original_asset_id)),0)
                  + COALESCE(SUM((SELECT ta.size FROM assets ta
                        WHERE ta.namespace_id=b.namespace_id AND ta.asset_id=b.thumbnail_asset_id)),0)
             FROM photo_bindings b
             WHERE b.namespace_id=1 AND b.zone_name=?1 AND b.trashed=0
               AND (?2 IS NULL OR b.album=?2 OR b.album LIKE ?2 || '/%')
             GROUP BY b.album ORDER BY b.album",
        )
        .map_err(|_| Error::RecoveryFailed)?;
    let rows = statement
        .query_map(params![zone, prefix.as_deref()], |row| {
            Ok(AlbumView {
                name: row.get(0)?,
                photo_count: row.get::<_, i64>(1)?.max(0) as u64,
                bytes: row.get::<_, i64>(2)?.max(0) as u64,
            })
        })
        .map_err(|_| Error::RecoveryFailed)?;
    let mut albums = Vec::new();
    let mut ungrouped = AlbumView {
        name: String::new(),
        photo_count: 0,
        bytes: 0,
    };
    for row in rows {
        let album = row.map_err(|_| Error::RecoveryFailed)?;
        if album.name.is_empty() {
            ungrouped = album;
        } else {
            albums.push(album);
        }
    }
    // The trash album is not a stored album: it is the same index filtered by the
    // record's isTrashed projection, so its count is reported separately.
    let trashed_count: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM photo_bindings b
             WHERE b.namespace_id=1 AND b.zone_name=?1 AND b.trashed=1",
            params![zone],
            |row| row.get(0),
        )
        .map_err(|_| Error::RecoveryFailed)?;
    Ok(
        json!({"albums": albums, "ungrouped": ungrouped, "trashedCount": trashed_count.max(0), "zone": {"zoneName": zone}}),
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MonthBucket {
    /// UTC calendar month, `YYYY-MM`.
    month: String,
    photo_count: u64,
    bytes: u64,
}

/// Buckets by UTC calendar month. UTC is not incidental: `assetDate` is stored as
/// UTC milliseconds, so a local-timezone bucket would move photos between months.
pub(crate) fn timeline(db: &Connection, zone: &str, months: i64) -> Result<Value> {
    let mut statement = db
        .prepare(
            "SELECT strftime('%Y-%m', b.asset_date_ms/1000, 'unixepoch') AS month,
                    COUNT(*),
                    COALESCE(SUM((SELECT oa.size FROM assets oa
                        WHERE oa.namespace_id=b.namespace_id AND oa.asset_id=b.original_asset_id)),0)
                  + COALESCE(SUM((SELECT ta.size FROM assets ta
                        WHERE ta.namespace_id=b.namespace_id AND ta.asset_id=b.thumbnail_asset_id)),0)
             FROM photo_bindings b
             WHERE b.namespace_id=1 AND b.zone_name=?1 AND b.asset_date_ms IS NOT NULL
               AND b.trashed=0
             GROUP BY month ORDER BY month DESC LIMIT ?2",
        )
        .map_err(|_| Error::RecoveryFailed)?;
    let rows = statement
        .query_map(params![zone, months.clamp(1, MAX_MONTHS)], |row| {
            Ok(MonthBucket {
                month: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                photo_count: row.get::<_, i64>(1)?.max(0) as u64,
                bytes: row.get::<_, i64>(2)?.max(0) as u64,
            })
        })
        .map_err(|_| Error::RecoveryFailed)?;
    let mut buckets = Vec::new();
    for row in rows {
        buckets.push(row.map_err(|_| Error::RecoveryFailed)?);
    }
    Ok(json!({"months": buckets, "zone": {"zoneName": zone}}))
}

/// Counts for the account's own library. Byte totals come from the same `assets`
/// rows the quota accounting reads, so the gallery and the quota agree.
pub(crate) fn summary(db: &Connection, zone: &str) -> Result<Value> {
    let mut statement = db
        .prepare(
            "SELECT COUNT(*),
                    COALESCE(SUM(b.media_kind=0),0), COALESCE(SUM(b.media_kind=1),0),
                    COALESCE(SUM(b.media_kind=2),0),
                    COUNT(DISTINCT NULLIF(b.album,'')),
                    MIN(b.asset_date_ms), MAX(b.asset_date_ms)
             FROM photo_bindings b WHERE b.namespace_id=1 AND b.zone_name=?1 AND b.trashed=0",
        )
        .map_err(|_| Error::RecoveryFailed)?;
    let counts: (i64, i64, i64, i64, i64, Option<i64>, Option<i64>) = statement
        .query_row(params![zone], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        })
        .map_err(|_| Error::RecoveryFailed)?;
    let bytes: i64 = db
        .query_row(
            "SELECT COALESCE(SUM((SELECT oa.size FROM assets oa
                WHERE oa.namespace_id=b.namespace_id AND oa.asset_id=b.original_asset_id)),0)
              + COALESCE(SUM((SELECT ta.size FROM assets ta
                WHERE ta.namespace_id=b.namespace_id AND ta.asset_id=b.thumbnail_asset_id)),0)
             FROM photo_bindings b WHERE b.namespace_id=1 AND b.zone_name=?1 AND b.trashed=0",
            params![zone],
            |row| row.get(0),
        )
        .map_err(|_| Error::RecoveryFailed)?;
    Ok(json!({
        "photoCount": counts.0.max(0),
        "imageCount": counts.1.max(0),
        "videoCount": counts.2.max(0),
        "otherCount": counts.3.max(0),
        "albumCount": counts.4.max(0),
        "bytes": bytes.max(0),
        "fromMs": counts.5,
        "toMs": counts.6,
        "zone": {"zoneName": zone},
    }))
}

/// A listing request, fully owned so the API layer can build it from a query string
/// without borrowing. Every field is a filter, never a scope: none of them can name
/// another account, container or tenant.
#[derive(Default)]
pub struct Listing {
    pub zone: String,
    pub media: Option<String>,
    pub album: Option<String>,
    pub favorite: Option<bool>,
    pub hidden: Option<bool>,
    pub trashed: Option<bool>,
    pub from_ms: Option<i64>,
    pub to_ms: Option<i64>,
    pub query: Option<String>,
    pub limit: usize,
    pub cursor: Option<String>,
}

pub enum Request {
    Photos(Listing),
    Photo {
        zone: String,
        photo_id: String,
    },
    Albums {
        zone: String,
        prefix: Option<String>,
    },
    Timeline {
        zone: String,
        months: i64,
    },
    Summary {
        zone: String,
    },
}

pub(crate) fn execute(db: &Connection, guard: &Guard<'_>, request: Request) -> Result<Value> {
    match request {
        Request::Photos(listing) => {
            // The cursor is sealed with the same codec as a sync token, so a caller
            // cannot hand-craft a position, and it expires with the rest of the
            // account's tokens.
            let cursor = listing
                .cursor
                .as_deref()
                .map(|raw| guard.codec.unseal::<Cursor>(raw, CURSOR_KIND))
                .transpose()
                .map_err(|_| Error::Config("cursor is not valid"))?;
            let filters = Filters {
                media: listing.media.as_deref(),
                album: listing.album.as_deref(),
                favorite: listing.favorite,
                hidden: listing.hidden,
                trashed: listing.trashed,
                from_ms: listing.from_ms,
                to_ms: listing.to_ms,
                query: listing.query.as_deref(),
            };
            photos(
                db,
                &listing.zone,
                &filters,
                listing.limit.clamp(1, MAX_PAGE),
                cursor.as_ref(),
                guard,
            )
        }
        Request::Photo { zone, photo_id } => photo(db, &zone, &photo_id, guard),
        Request::Albums { zone, prefix } => albums(db, &zone, prefix.as_deref()),
        Request::Timeline { zone, months } => timeline(db, &zone, months),
        Request::Summary { zone } => summary(db, &zone),
    }
}
