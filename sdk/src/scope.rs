//! Capability probing and the pinned server scope.
//!
//! A connection is usable only after the service identifies itself, announces
//! protocol version 1, SHA-256 digests and every capability this SDK relies on.
//! The validated result is pinned for the lifetime of the connection so a later
//! response cannot silently weaken the contract.

use crate::Error;
use crate::endpoint::Endpoint;
use crate::model::ZoneId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use url::Url;

pub const SERVICE_NAME: &str = "syncserver";
pub const PROTOCOL_VERSION: i64 = 1;
pub const CHECKSUM_ALGORITHM: &str = "SHA256";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Namespace {
    pub tenant: String,
    pub container: String,
    pub environment: String,
    pub database: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionUser {
    pub id: String,
    pub username: String,
    pub role: String,
}

impl SessionUser {
    pub fn is_admin(&self) -> bool {
        self.role == "admin"
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Authentication {
    pub mode: String,
    #[serde(default)]
    pub user: Option<SessionUser>,
    #[serde(default)]
    pub resource_bearer_required: bool,
    #[serde(default)]
    pub session_ttl_seconds: u64,
}

/// The capability bits this SDK requires. Optional optimizations are recorded
/// but never required, and features the protocol does not offer are ignored.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CapabilityBits {
    pub zone_listing: bool,
    pub record_crud: bool,
    pub snapshots: bool,
    pub incremental_changes: bool,
    pub upload_leases: bool,
    pub fencing: bool,
    pub token_current_gate: bool,
    pub upload_phases: bool,
    pub operation_status_read: bool,
    pub asset_upload: bool,
    pub asset_download: bool,
    pub photo_publication: bool,
    #[serde(default)]
    pub storage_quota: bool,
    #[serde(default)]
    pub storage_usage_by_media_type: bool,
    #[serde(default)]
    pub query_batch: bool,
    #[serde(default)]
    pub change_aliases: bool,
    #[serde(default)]
    pub upload_resume: bool,
    #[serde(default)]
    pub sharing: bool,
    #[serde(default)]
    pub apple_authentication: bool,
}

pub type CapabilityRequirement = (&'static str, fn(&CapabilityBits) -> bool);

pub const REQUIRED_CAPABILITIES: [CapabilityRequirement; 12] = [
    ("zoneListing", |bits| bits.zone_listing),
    ("recordCrud", |bits| bits.record_crud),
    ("snapshots", |bits| bits.snapshots),
    ("incrementalChanges", |bits| bits.incremental_changes),
    ("uploadLeases", |bits| bits.upload_leases),
    ("fencing", |bits| bits.fencing),
    ("tokenCurrentGate", |bits| bits.token_current_gate),
    ("uploadPhases", |bits| bits.upload_phases),
    ("operationStatusRead", |bits| bits.operation_status_read),
    ("assetUpload", |bits| bits.asset_upload),
    ("assetDownload", |bits| bits.asset_download),
    ("photoPublication", |bits| bits.photo_publication),
];

impl CapabilityBits {
    pub fn missing_required(&self) -> Vec<&'static str> {
        REQUIRED_CAPABILITIES
            .into_iter()
            .filter(|(_, enabled)| !enabled(self))
            .map(|(name, _)| name)
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_asset_bytes: u64,
    #[serde(default)]
    pub user_storage_quota_bytes: u64,
    pub thumbnail_max_bytes: u64,
    pub upload_concurrency: usize,
    pub download_concurrency: usize,
    pub upload_idle_timeout_secs: u64,
    pub upload_total_timeout_secs: u64,
    pub db_queue_capacity: usize,
    pub json_body_limit: usize,
    pub lease_ttl_secs: u64,
    pub lease_heartbeat_secs: u64,
    pub control_queue_capacity: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SyncLimits {
    #[serde(default)]
    pub max_page: usize,
    #[serde(default)]
    pub max_zones: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RecordLimits {
    #[serde(default)]
    pub max_operations: usize,
    #[serde(default)]
    pub max_record_bytes: u64,
    #[serde(default)]
    pub max_result_bytes: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    #[serde(default)]
    pub authentication: Option<Authentication>,
    pub service: String,
    pub version: String,
    pub stage: String,
    pub namespace: Namespace,
    /// Bare zone names, not `zoneID` objects.
    pub zones: Vec<String>,
    #[serde(rename = "publicBaseURL")]
    pub public_base_url: String,
    pub database_generation: String,
    #[serde(default)]
    pub schema_version: i64,
    #[serde(default)]
    pub administrator: bool,
    pub capabilities: CapabilityBits,
    pub limits: Limits,
    pub protocol_version: i64,
    pub checksum_algorithm: String,
    #[serde(default)]
    pub history_retention: String,
    #[serde(default)]
    pub record_field_types: Vec<String>,
    #[serde(default)]
    pub sync_limits: SyncLimits,
    #[serde(default)]
    pub record_limits: RecordLimits,
}

/// What a successful probe pins onto a connection.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PinnedScope {
    pub authentication: Option<Authentication>,
    pub public_base_url: String,
    pub namespace: Namespace,
    pub database_generation: String,
    pub protocol_version: i64,
    pub service_version: String,
    pub stage: String,
    pub checksum_algorithm: String,
    pub zones: Vec<String>,
    pub capabilities: CapabilityBits,
    pub limits: Limits,
    pub sync_limits: SyncLimits,
    pub record_limits: RecordLimits,
}

impl PinnedScope {
    pub fn has_zone(&self, zone_name: &str) -> bool {
        self.zones.iter().any(|zone| zone == zone_name)
    }

    /// A configured zone with the only owner the service currently accepts.
    pub fn zone(&self, zone_name: &str) -> Result<ZoneId, Error> {
        if !self.has_zone(zone_name) {
            return Err(Error::Protocol(format!(
                "zone {zone_name:?} is not configured on this server"
            )));
        }
        Ok(ZoneId::new(zone_name))
    }

    /// `PrimarySync` when present, otherwise the first configured zone.
    pub fn primary_zone(&self) -> Result<ZoneId, Error> {
        if self.has_zone(crate::model::PRIMARY_ZONE) {
            return Ok(ZoneId::primary());
        }
        self.zones
            .first()
            .map(|name| ZoneId::new(name.clone()))
            .ok_or_else(|| Error::Protocol("the server has no configured zones".into()))
    }

    /// A conservative page size that never exceeds the server's own ceiling.
    pub fn page_limit(&self) -> usize {
        match self.sync_limits.max_page {
            0 => 100,
            limit => limit.min(100),
        }
        .max(1)
    }

    /// An idempotency key is at most 128 printable ASCII bytes.
    pub fn max_idempotency_key_len(&self) -> usize {
        128
    }
}

/// A name that is safe to use as one path segment. The service already only
/// accepts these, so anything else means the peer is not the service we expect.
fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

/// Turn an announced capability document into the scope this connection uses.
pub fn validate_capabilities(
    endpoint: &Endpoint,
    response: &Capabilities,
) -> Result<PinnedScope, Error> {
    if response.service != SERVICE_NAME {
        return Err(Error::Protocol(format!(
            "expected service {SERVICE_NAME:?}, received {:?}",
            response.service
        )));
    }
    if response.protocol_version != PROTOCOL_VERSION {
        return Err(Error::Protocol(format!(
            "unsupported protocol version {} (this SDK speaks {PROTOCOL_VERSION})",
            response.protocol_version
        )));
    }
    if response.checksum_algorithm != CHECKSUM_ALGORITHM {
        return Err(Error::Protocol(format!(
            "unsupported checksum algorithm {:?}",
            response.checksum_algorithm
        )));
    }
    let missing = response.capabilities.missing_required();
    if !missing.is_empty() {
        return Err(Error::Protocol(format!(
            "server is missing required capabilities: {}",
            missing.join(", ")
        )));
    }
    if response.database_generation.parse::<uuid::Uuid>().is_err() {
        return Err(Error::Protocol(format!(
            "database generation {:?} is not a UUID",
            response.database_generation
        )));
    }
    if response.namespace.environment != "production"
        || response.namespace.database != "private"
        || !safe_component(&response.namespace.container)
    {
        return Err(Error::Protocol(
            "the server's namespace is not the supported production/private database".into(),
        ));
    }
    if response.zones.iter().any(|zone| !safe_component(zone)) {
        return Err(Error::Protocol(
            "the server announced an invalid zone name".into(),
        ));
    }
    let unique: BTreeSet<_> = response.zones.iter().collect();
    if unique.len() != response.zones.len() {
        return Err(Error::Protocol(
            "the server announced duplicate zones".into(),
        ));
    }

    let announced = Url::parse(&response.public_base_url).map_err(|error| {
        Error::Protocol(format!(
            "publicBaseURL {:?} is not a URL: {error}",
            response.public_base_url
        ))
    })?;
    let announced_origin = (
        announced.scheme().to_string(),
        announced.host_str().unwrap_or_default().to_string(),
        announced.port(),
    );
    if announced_origin != endpoint.origin() || announced.path() != "/" {
        return Err(Error::Protocol(format!(
            "publicBaseURL {:?} does not match the configured origin {:?}; \
             alternate asset origins are not supported",
            response.public_base_url,
            endpoint.as_str()
        )));
    }

    Ok(PinnedScope {
        authentication: response.authentication.clone(),
        public_base_url: response.public_base_url.clone(),
        namespace: response.namespace.clone(),
        database_generation: response.database_generation.clone(),
        protocol_version: response.protocol_version,
        service_version: response.version.clone(),
        stage: response.stage.clone(),
        checksum_algorithm: response.checksum_algorithm.clone(),
        zones: response.zones.clone(),
        capabilities: response.capabilities.clone(),
        limits: response.limits.clone(),
        sync_limits: response.sync_limits.clone(),
        record_limits: response.record_limits.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document() -> Capabilities {
        let value = json!({
            "service": "syncserver",
            "version": "0.1.0",
            "stage": "M4",
            "namespace": {
                "tenant": "local",
                "container": "com.apple.photos.cloud",
                "environment": "production",
                "database": "private",
            },
            "zones": ["PrimarySync"],
            "publicBaseURL": "https://sync.example.com/",
            "databaseGeneration": "501855bb-18d3-4008-abc3-37e4948bf6f0",
            "schemaVersion": 4,
            "capabilities": {
                "zoneListing": true, "recordCrud": true, "snapshots": true,
                "incrementalChanges": true, "uploadLeases": true, "fencing": true,
                "tokenCurrentGate": true, "uploadPhases": true,
                "operationStatusRead": true, "assetUpload": true,
                "assetDownload": true, "photoPublication": true,
                "uploadResume": false, "sharing": false, "appleAuthentication": false,
            },
            "limits": {
                "maxAssetBytes": 10, "userStorageQuotaBytes": 100,
                "thumbnailMaxBytes": 5, "uploadConcurrency": 2,
                "downloadConcurrency": 2, "uploadIdleTimeoutSecs": 30,
                "uploadTotalTimeoutSecs": 60, "dbQueueCapacity": 4,
                "jsonBodyLimit": 1000, "leaseTtlSecs": 90,
                "leaseHeartbeatSecs": 30, "controlQueueCapacity": 2,
            },
            "protocolVersion": 1,
            "checksumAlgorithm": "SHA256",
            "historyRetention": "all",
            "recordFieldTypes": ["STRING"],
            "syncLimits": {"maxPage": 200, "maxZones": 20},
            "recordLimits": {"maxOperations": 400, "maxRecordBytes": 1000, "maxResultBytes": 2000},
        });
        serde_json::from_value(value).unwrap()
    }

    #[test]
    fn a_complete_document_pins_the_scope() {
        let endpoint = Endpoint::parse("https://sync.example.com").unwrap();
        let scope = validate_capabilities(&endpoint, &document()).unwrap();
        assert_eq!(scope.primary_zone().unwrap(), ZoneId::primary());
        assert_eq!(scope.page_limit(), 100);
        assert_eq!(scope.namespace.container, "com.apple.photos.cloud");
    }

    #[test]
    fn missing_capabilities_and_foreign_origins_are_rejected() {
        let endpoint = Endpoint::parse("https://sync.example.com").unwrap();
        let mut missing = document();
        missing.capabilities.photo_publication = false;
        assert!(validate_capabilities(&endpoint, &missing).is_err());

        let mut origin = document();
        origin.public_base_url = "https://cdn.example.com/".into();
        assert!(validate_capabilities(&endpoint, &origin).is_err());

        let mut version = document();
        version.protocol_version = 2;
        assert!(validate_capabilities(&endpoint, &version).is_err());

        let mut namespace = document();
        namespace.namespace.environment = "development".into();
        assert!(validate_capabilities(&endpoint, &namespace).is_err());
    }
}
