//! Bounded read requests and opaque cursor roles, independent of HTTP and SQL.
use super::{
    Namespace,
    records::{DomainResult, Failure, Lookup, ZoneId, identifier},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;
pub const MAX_PAGE: usize = 200;
pub const MAX_ZONES: usize = 20;
pub const MAX_TOKEN_BYTES: usize = 16_384;
fn limit() -> usize {
    MAX_PAGE
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Claims {
    pub version: u8,
    pub namespace: Namespace,
    pub generation: Uuid,
    pub zone: Option<ZoneId>,
    pub cursor: Cursor,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Cursor {
    Snapshot {
        watermark: i64,
    },
    Query {
        watermark: i64,
        record_type: String,
        desired_keys: Option<BTreeSet<String>>,
        last_record_name: String,
    },
    Zone {
        position: i64,
        watermark: Option<i64>,
    },
    Database {
        position: i64,
        watermark: Option<i64>,
    },
}
impl Cursor {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Snapshot { .. } => "snapshot",
            Self::Query { .. } => "query",
            Self::Zone { .. } => "zone",
            Self::Database { .. } => "database",
        }
    }
    pub fn bounds(&self) -> (i64, i64) {
        match self {
            Self::Snapshot { watermark } | Self::Query { watermark, .. } => {
                (*watermark, *watermark)
            }
            Self::Zone {
                position,
                watermark,
            }
            | Self::Database {
                position,
                watermark,
            } => (*position, watermark.unwrap_or(*position)),
        }
    }
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct SnapshotRequest {
    #[serde(rename = "zoneID", default)]
    pub zone: ZoneId,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Query {
    pub record_type: String,
    #[serde(default)]
    pub filter_by: Vec<serde_json::Value>,
    #[serde(default)]
    pub sort_by: Vec<serde_json::Value>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct QueryRequest {
    #[serde(rename = "zoneID", default)]
    pub zone: ZoneId,
    pub query: Query,
    #[serde(default = "limit")]
    pub results_limit: usize,
    pub desired_keys: Option<Vec<String>>,
    pub snapshot_token: Option<String>,
    pub continuation_marker: Option<String>,
    pub get_current_sync_token: Option<bool>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ZoneRequest {
    #[serde(rename = "desiredRecordTypes")]
    pub desired_record_types: Option<Vec<String>>,
    #[serde(rename = "zoneID", default)]
    pub zone: ZoneId,
    pub sync_token: Option<String>,
    #[serde(default = "limit")]
    pub results_limit: usize,
    pub desired_keys: Option<Vec<String>>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ZonesRequest {
    pub zones: Vec<ZoneRequest>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct DatabaseRequest {
    pub sync_token: Option<String>,
    #[serde(default = "limit")]
    pub results_limit: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct BatchRequest {
    pub batch: Vec<QueryRequest>,
}
pub(crate) enum Read {
    Batch(BatchRequest),
    Snapshot(SnapshotRequest),
    Query(QueryRequest),
    Lookup(Lookup),
    Zones(ZonesRequest),
    Database(DatabaseRequest),
}
pub(crate) fn check_limit(value: usize) -> DomainResult<()> {
    if !(1..=MAX_PAGE).contains(&value) {
        return Err(Failure::bad());
    }
    Ok(())
}
pub(crate) fn keys(value: &Option<Vec<String>>) -> DomainResult<Option<BTreeSet<String>>> {
    if value
        .as_ref()
        .is_some_and(|keys| keys.len() > 256 || keys.iter().any(|s| !identifier(s)))
    {
        return Err(Failure::bad());
    }
    if value
        .as_ref()
        .is_some_and(|keys| serde_json::to_vec(keys).map_or(true, |bytes| bytes.len() > 8192))
    {
        return Err(Failure::limit());
    }
    Ok(value.as_ref().map(|keys| keys.iter().cloned().collect()))
}
impl Read {
    pub fn validate(&self) -> DomainResult<()> {
        match self {
            Self::Batch(r) => {
                if r.batch.is_empty() || r.batch.len() > 20 {
                    return Err(Failure::bad());
                }
                for q in &r.batch {
                    q.zone.validate()?;
                    check_limit(q.results_limit)?;
                    keys(&q.desired_keys)?;
                    if !identifier(&q.query.record_type) {
                        return Err(Failure::bad());
                    }
                    if !q.query.filter_by.is_empty() || !q.query.sort_by.is_empty() {
                        return Err(Failure::unsupported());
                    }
                }
                Ok(())
            }
            Self::Snapshot(r) => r.zone.validate(),
            Self::Query(r) => {
                r.zone.validate()?;
                check_limit(r.results_limit)?;
                keys(&r.desired_keys)?;
                if !identifier(&r.query.record_type) {
                    return Err(Failure::bad());
                }
                if !r.query.filter_by.is_empty() || !r.query.sort_by.is_empty() {
                    return Err(Failure::unsupported());
                }
                // Token is always returned; false is a compatibility hint, never
                // permission to treat an incomplete query as applied progress.
                let _ = r.get_current_sync_token;
                Ok(())
            }
            Self::Lookup(_) => Ok(()),
            Self::Zones(r) => {
                if r.zones.is_empty() || r.zones.len() > MAX_ZONES {
                    return Err(Failure::bad());
                }
                let mut zones = BTreeSet::new();
                for zone in &r.zones {
                    zone.zone.validate()?;
                    check_limit(zone.results_limit)?;
                    keys(&zone.desired_keys)?;
                    keys(&zone.desired_record_types)?;
                    if !zones.insert(&zone.zone.zone_name) {
                        return Err(Failure::bad());
                    }
                }
                Ok(())
            }
            Self::Database(r) => check_limit(r.results_limit),
        }
    }
}
