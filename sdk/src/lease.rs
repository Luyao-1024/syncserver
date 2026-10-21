//! The exclusive upload lease.
//!
//! A namespace has one effective uplink device. The lease is what makes two
//! installations agree, and the fencing token is what proves the holder is
//! still the holder when a write commits.
//!
//! Authority is measured from **when the renewal request started**, on a
//! monotonic clock, with a safety margin. It is never derived from the
//! server's wall-clock `expiresAtMs`: a clock jump would otherwise end the
//! lease early or extend authority the client no longer has. A renewal that
//! arrives after the current deadline does not revive the lease.
//!
//! [`UploadLease::acquire`] starts a heartbeat task and tracks the current
//! phase. [`UploadLease::release`] stops it and releases the lease; dropping
//! the value only stops the heartbeat, because a release is a network call.

use crate::Error;
use crate::client::{Connection, decode};
use crate::model::{ZoneId, identifier};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use url::Url;

pub const UPLOAD_LEASE_LOST: &str = "UPLOAD_LEASE_LOST";
pub const UPLOAD_LOCK_BUSY: &str = "UPLOAD_LOCK_BUSY";
pub const UPLOAD_PHASE_MISMATCH: &str = "UPLOAD_PHASE_MISMATCH";
pub const SYNC_NOT_CURRENT: &str = "SYNC_NOT_CURRENT";

/// The share of a granted TTL withheld from authority. The renewal round trip
/// and scheduling jitter are the uncertainty; a tenth is far larger than both.
const DEADLINE_SAFETY: f64 = 0.1;

/// The proof carried on every lease-mutating call.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseProof {
    pub lease_id: String,
    pub lease_token: String,
    /// Opaque decimal text; reformatting it would change its meaning.
    pub fencing_token: String,
}

impl LeaseProof {
    pub(crate) fn headers(&self) -> [(&'static str, &str); 3] {
        [
            ("X-Sync-Lease-ID", self.lease_id.as_str()),
            ("X-Sync-Lease-Token", self.lease_token.as_str()),
            ("X-Sync-Lease-Fence", self.fencing_token.as_str()),
        ]
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseGrant {
    #[serde(rename = "leaseId")]
    pub lease_id: String,
    #[serde(rename = "leaseToken")]
    pub lease_token: String,
    #[serde(rename = "fencingToken")]
    pub fencing_token: String,
    #[serde(rename = "expiresAtMs", default)]
    pub expires_at_ms: i64,
    #[serde(rename = "ttlSeconds")]
    pub ttl_seconds: u64,
    #[serde(rename = "heartbeatSeconds")]
    pub heartbeat_seconds: u64,
    #[serde(default)]
    pub phase: String,
    #[serde(rename = "zoneIDs", default)]
    pub zone_ids: Vec<ZoneId>,
}

impl LeaseGrant {
    pub fn proof(&self) -> LeaseProof {
        LeaseProof {
            lease_id: self.lease_id.clone(),
            lease_token: self.lease_token.clone(),
            fencing_token: self.fencing_token.clone(),
        }
    }
}

/// The three phases a cycle walks, in order. A phase is never skipped by this
/// SDK: settling the previous phase is not optional even when it is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadPhase {
    Delete,
    Modify,
    Create,
}

impl UploadPhase {
    pub const ORDER: [UploadPhase; 3] = [Self::Delete, Self::Modify, Self::Create];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "delete",
            Self::Modify => "modify",
            Self::Create => "create",
        }
    }

    pub fn from_wire(value: &str) -> Option<Self> {
        match value {
            "delete" => Some(Self::Delete),
            "modify" => Some(Self::Modify),
            "create" => Some(Self::Create),
            _ => None,
        }
    }

    pub fn next(self) -> Option<Self> {
        match self {
            Self::Delete => Some(Self::Modify),
            Self::Modify => Some(Self::Create),
            Self::Create => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneSyncToken {
    #[serde(rename = "zoneID")]
    pub zone: ZoneId,
    #[serde(rename = "syncToken")]
    pub sync_token: String,
}

/// The answer to the progress gate. `isCurrent` false means the client's
/// applied token is behind the cloud; catch up and check again. `mayUpload`
/// false leaves the lease in `checking`.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LeaseCheck {
    #[serde(rename = "isCurrent")]
    pub is_current: bool,
    #[serde(rename = "mayUpload")]
    pub may_upload: bool,
    pub phase: String,
    #[serde(rename = "zoneTokens", default)]
    pub zone_tokens: Vec<ZoneSyncToken>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseRenewal {
    pub ttl_seconds: u64,
    /// When the renewal request was sent, before the round trip.
    pub started: Instant,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LeaseState {
    Held { deadline: Instant },
    Lost,
}

/// A held lease with a heartbeat and a conservative authority window.
pub struct UploadLease {
    connection: Connection,
    zone: ZoneId,
    proof: LeaseProof,
    ttl: Duration,
    heartbeat: Duration,
    state: Arc<Mutex<LeaseState>>,
    phase: Arc<Mutex<Option<UploadPhase>>>,
    heartbeat_task: Option<tokio::task::JoinHandle<()>>,
}

impl std::fmt::Debug for UploadLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UploadLease")
            .field("lease_id", &self.proof.lease_id)
            .field("fencing_token", &self.proof.fencing_token)
            .field("phase", &self.phase())
            .field("held", &self.is_held())
            .finish()
    }
}

impl UploadLease {
    /// Acquire the namespace's uplink lease for one zone and start its
    /// heartbeat. `device_id` is a stable, non-secret installation identity.
    pub async fn acquire(
        connection: &Connection,
        device_id: &str,
        zone: &ZoneId,
    ) -> Result<Self, Error> {
        if !identifier(device_id) {
            return Err(Error::Protocol("invalid device id".into()));
        }
        let started = Instant::now();
        let grant = connection.acquire_upload_lease(device_id, zone).await?;
        let mut lease = Self::from_grant(connection.clone(), zone.clone(), &grant, started);
        lease.start_heartbeat();
        Ok(lease)
    }

    /// Build a lease from a grant the caller obtained itself. Heartbeat is not
    /// started until [`UploadLease::start_heartbeat`] is called.
    pub fn from_grant(
        connection: Connection,
        zone: ZoneId,
        grant: &LeaseGrant,
        started: Instant,
    ) -> Self {
        let phase = UploadPhase::from_wire(&grant.phase);
        Self {
            connection,
            zone,
            proof: grant.proof(),
            ttl: Duration::from_secs(grant.ttl_seconds),
            heartbeat: Duration::from_secs(grant.heartbeat_seconds).max(Duration::from_millis(10)),
            state: Arc::new(Mutex::new(LeaseState::Held {
                deadline: started
                    .checked_add(conservative(grant.ttl_seconds))
                    .unwrap_or(started),
            })),
            phase: Arc::new(Mutex::new(phase)),
            heartbeat_task: None,
        }
    }

    /// Start the renewal task. Must run inside a Tokio runtime. Calling it more
    /// than once keeps the existing task.
    pub fn start_heartbeat(&mut self) {
        if self.heartbeat_task.is_some() || !self.is_held() {
            return;
        }
        let connection = self.connection.clone();
        let proof = self.proof.clone();
        let state = Arc::clone(&self.state);
        let interval = self.heartbeat;
        self.heartbeat_task = Some(tokio::spawn(async move {
            heartbeat(connection, proof, state, interval).await;
        }));
    }

    pub fn proof(&self) -> &LeaseProof {
        &self.proof
    }

    /// The zone this lease was acquired for. Every mutation issued under it
    /// must target this zone.
    pub fn zone(&self) -> &ZoneId {
        &self.zone
    }

    /// The granted TTL, before the safety margin.
    pub fn granted_ttl(&self) -> Duration {
        self.ttl
    }

    pub fn heartbeat_interval(&self) -> Duration {
        self.heartbeat
    }

    /// `None` while the service is still in `checking`.
    pub fn phase(&self) -> Option<UploadPhase> {
        *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn deadline(&self) -> Option<Instant> {
        match *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            LeaseState::Held { deadline } => Some(deadline),
            LeaseState::Lost => None,
        }
    }

    /// Whether authority still stands. Checked immediately before every
    /// mutation so a cycle cannot keep issuing after losing the lease.
    pub fn is_held(&self) -> bool {
        match *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            LeaseState::Lost => false,
            LeaseState::Held { deadline } => Instant::now() < deadline,
        }
    }

    pub fn require_held(&self) -> Result<(), Error> {
        if self.is_held() {
            Ok(())
        } else {
            Err(Error::LeaseLost)
        }
    }

    /// Stop assuming authority immediately. Call after any `UPLOAD_LEASE_LOST`.
    pub fn mark_lost(&self) {
        *self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = LeaseState::Lost;
    }

    /// Prove this client's position and move the lease out of `checking`.
    /// Inspect [`LeaseCheck::may_upload`]; a false result keeps the lease in
    /// `checking` and must be retried after catching up.
    pub async fn check(
        &self,
        applied_sync_token: &str,
        local_ready: bool,
    ) -> Result<LeaseCheck, Error> {
        self.require_held()?;
        let check = self
            .connection
            .check_upload_lease(&self.proof, &self.zone, applied_sync_token, local_ready)
            .await?;
        let next = match check.phase.as_str() {
            "checking" => None,
            other => Some(UploadPhase::from_wire(other).ok_or_else(|| {
                Error::Protocol(format!("the service reported unknown phase {other:?}"))
            })?),
        };
        *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
        Ok(check)
    }

    /// Move to exactly one phase. The service refuses skips and backwards
    /// moves; a local phase of `None` is still `checking`, not `Delete`.
    pub async fn set_phase(&self, phase: UploadPhase) -> Result<UploadPhase, Error> {
        self.require_held()?;
        let returned = self.connection.set_upload_phase(&self.proof, phase).await?;
        if returned != phase {
            return Err(Error::Protocol(format!(
                "the service acknowledged phase {:?} for a {:?} request",
                returned, phase
            )));
        }
        *self
            .phase
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(returned);
        Ok(returned)
    }

    /// Advance by one phase. Returns `None` when already at `Create`.
    pub async fn advance_phase(&self) -> Result<Option<UploadPhase>, Error> {
        let Some(current) = self.phase() else {
            return Err(Error::PhaseMismatch(
                "the progress check has not passed; the lease is still checking".into(),
            ));
        };
        match current.next() {
            Some(next) => self.set_phase(next).await.map(Some),
            None => Ok(None),
        }
    }

    /// Stop the heartbeat and release the lease. Returns whether the service
    /// considered this holder current. Dropping a lease never releases it.
    pub async fn release(mut self) -> Result<bool, Error> {
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        let was_held = self.is_held();
        self.mark_lost();
        if !was_held {
            return Ok(false);
        }
        match self.connection.release_upload_lease(&self.proof).await {
            Ok(released) => Ok(released),
            // Already gone: releasing it again is not an error.
            Err(Error::LeaseLost) => Ok(false),
            Err(error) => Err(error),
        }
    }
}

impl Drop for UploadLease {
    fn drop(&mut self) {
        if let Some(task) = self.heartbeat_task.take() {
            task.abort();
        }
        self.mark_lost();
    }
}

async fn heartbeat(
    connection: Connection,
    proof: LeaseProof,
    state: Arc<Mutex<LeaseState>>,
    interval: Duration,
) {
    let mut delay = interval;
    loop {
        let deadline = match *state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            LeaseState::Held { deadline } => deadline,
            LeaseState::Lost => return,
        };
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            mark_state_lost(&state);
            return;
        }
        tokio::time::sleep(delay.min(remaining)).await;
        if !state_held(&state) {
            mark_state_lost(&state);
            return;
        }
        let remaining = match *state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
        {
            LeaseState::Held { deadline } => deadline.saturating_duration_since(Instant::now()),
            LeaseState::Lost => return,
        };
        match tokio::time::timeout(remaining, connection.renew_upload_lease(&proof)).await {
            Ok(Ok(renewal)) => {
                let mut slot = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                *slot = renewed(*slot, Instant::now(), renewal.started, renewal.ttl_seconds);
                if !matches!(*slot, LeaseState::Held { .. }) {
                    return;
                }
                delay = interval;
            }
            // A temporary network problem is worth retrying while the current
            // window still stands; any other answer ends the lease.
            Ok(Err(error)) if error.is_retryable() && state_held(&state) => {
                delay = Duration::from_millis(500);
            }
            _ => {
                mark_state_lost(&state);
                return;
            }
        }
    }
}

fn state_held(state: &Mutex<LeaseState>) -> bool {
    match *state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        LeaseState::Lost => false,
        LeaseState::Held { deadline } => Instant::now() < deadline,
    }
}

fn mark_state_lost(state: &Mutex<LeaseState>) {
    *state
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = LeaseState::Lost;
}

/// The state a renewal leaves behind. Pulled out so the rule can be tested
/// without sleeping through a real lease.
fn renewed(current: LeaseState, now: Instant, started: Instant, ttl_seconds: u64) -> LeaseState {
    match current {
        LeaseState::Lost => LeaseState::Lost,
        LeaseState::Held { deadline } => {
            if now >= deadline {
                // Authority ended while the renewal was in flight. A late
                // answer cannot take it back from another device.
                LeaseState::Lost
            } else {
                let offered = started
                    .checked_add(conservative(ttl_seconds))
                    .unwrap_or(started);
                if offered > deadline {
                    LeaseState::Held { deadline: offered }
                } else {
                    LeaseState::Held { deadline }
                }
            }
        }
    }
}

fn conservative(ttl_seconds: u64) -> Duration {
    Duration::from_secs_f64((ttl_seconds as f64 * (1.0 - DEADLINE_SAFETY)).max(0.0))
}

#[derive(Deserialize)]
struct PhaseResponse {
    phase: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RenewResponse {
    #[serde(rename = "ttlSeconds")]
    ttl_seconds: u64,
}

#[derive(Deserialize)]
struct ReleaseResponse {
    released: bool,
}

impl Connection {
    /// Acquire the uplink lease for one zone. `UPLOAD_LOCK_BUSY` means another
    /// device holds it; skip this round instead of retrying.
    pub async fn acquire_upload_lease(
        &self,
        device_id: &str,
        zone: &ZoneId,
    ) -> Result<LeaseGrant, Error> {
        self.check_zone(zone)?;
        if !identifier(device_id) {
            return Err(Error::Protocol("invalid device id".into()));
        }
        let url = self.url(&["v1", "upload-leases"])?;
        let body = json!({ "deviceId": device_id, "zoneIDs": [zone] });
        let bytes = self
            .client()
            .post_json_bytes(&url, &body, None, None)
            .await?;
        decode(&bytes)
    }

    /// Prove the client's applied token and pass the progress gate.
    pub async fn check_upload_lease(
        &self,
        proof: &LeaseProof,
        zone: &ZoneId,
        applied_sync_token: &str,
        local_ready: bool,
    ) -> Result<LeaseCheck, Error> {
        self.check_zone(zone)?;
        let url = self.lease_url(proof, "check")?;
        let body = json!({
            "zoneTokens": [{ "zoneID": zone, "syncToken": applied_sync_token }],
            "localReady": local_ready,
        });
        let bytes = self
            .client()
            .post_json_bytes(&url, &body, Some(proof), None)
            .await?;
        decode(&bytes)
    }

    pub async fn set_upload_phase(
        &self,
        proof: &LeaseProof,
        phase: UploadPhase,
    ) -> Result<UploadPhase, Error> {
        let url = self.lease_url(proof, "phase")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &json!({ "phase": phase.as_str() }), Some(proof), None)
            .await?;
        let response: PhaseResponse = decode(&bytes)?;
        UploadPhase::from_wire(&response.phase).ok_or_else(|| {
            Error::Protocol(format!(
                "the service reported an unknown phase {:?}",
                response.phase
            ))
        })
    }

    pub async fn renew_upload_lease(&self, proof: &LeaseProof) -> Result<LeaseRenewal, Error> {
        let started = Instant::now();
        let url = self.lease_url(proof, "renew")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &json!({}), Some(proof), None)
            .await?;
        let response: RenewResponse = decode(&bytes)?;
        Ok(LeaseRenewal {
            ttl_seconds: response.ttl_seconds,
            started,
        })
    }

    /// Release the lease. A late release cannot release a newer holder: the
    /// service binds the proof to this lease id and fence.
    pub async fn release_upload_lease(&self, proof: &LeaseProof) -> Result<bool, Error> {
        let url = self.lease_url(proof, "release")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &json!({}), Some(proof), None)
            .await?;
        let response: ReleaseResponse = decode(&bytes)?;
        Ok(response.released)
    }

    fn lease_url(&self, proof: &LeaseProof, operation: &str) -> Result<Url, Error> {
        if !identifier(&proof.lease_id) || !identifier(operation) {
            return Err(Error::Protocol("invalid lease path".into()));
        }
        let mut url = self.url(&["v1", "upload-leases"])?;
        {
            let mut segments = url
                .path_segments_mut()
                .map_err(|_| Error::Protocol("lease URL cannot be extended".into()))?;
            segments.pop_if_empty();
            segments.push(&proof.lease_id);
            segments.push(operation);
        }
        Ok(url)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> LeaseGrant {
        LeaseGrant {
            lease_id: "lease".into(),
            lease_token: "token".into(),
            fencing_token: "1".into(),
            expires_at_ms: 0,
            ttl_seconds: 90,
            heartbeat_seconds: 30,
            phase: "checking".into(),
            zone_ids: vec![ZoneId::primary()],
        }
    }

    #[test]
    fn a_grant_starts_with_nine_tenths_of_its_ttl() {
        let connection = Connection::new(
            crate::Client::builder("https://sync.example.com")
                .unwrap()
                .build()
                .unwrap(),
            crate::scope::PinnedScope {
                authentication: None,
                public_base_url: "https://sync.example.com/".into(),
                namespace: crate::scope::Namespace {
                    tenant: "local".into(),
                    container: "com.apple.photos.cloud".into(),
                    environment: "production".into(),
                    database: "private".into(),
                },
                database_generation: "501855bb-18d3-4008-abc3-37e4948bf6f0".into(),
                protocol_version: 1,
                service_version: "0".into(),
                stage: "test".into(),
                checksum_algorithm: "SHA256".into(),
                zones: vec!["PrimarySync".into()],
                capabilities: crate::scope::CapabilityBits::default(),
                limits: crate::scope::Limits {
                    max_asset_bytes: 0,
                    user_storage_quota_bytes: 0,
                    thumbnail_max_bytes: 0,
                    upload_concurrency: 1,
                    download_concurrency: 1,
                    upload_idle_timeout_secs: 1,
                    upload_total_timeout_secs: 1,
                    db_queue_capacity: 1,
                    json_body_limit: 1,
                    lease_ttl_secs: 90,
                    lease_heartbeat_secs: 30,
                    control_queue_capacity: 1,
                },
                sync_limits: crate::scope::SyncLimits::default(),
                record_limits: crate::scope::RecordLimits::default(),
            },
        );
        let lease =
            UploadLease::from_grant(connection, ZoneId::primary(), &grant(), Instant::now());
        assert_eq!(lease.granted_ttl(), Duration::from_secs(90));
        assert_eq!(lease.heartbeat_interval(), Duration::from_secs(30));
        assert!(lease.is_held());
        assert_eq!(lease.phase(), None);
        assert_eq!(lease.zone(), &ZoneId::primary());
    }

    #[test]
    fn late_renewals_cannot_resurrect_authority() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(10);
        let held = LeaseState::Held { deadline };

        let late = renewed(
            held,
            deadline + Duration::from_secs(1),
            deadline - Duration::from_secs(1),
            90,
        );
        assert_eq!(late, LeaseState::Lost);

        let offered = renewed(held, now, now, 90);
        assert_eq!(
            offered,
            LeaseState::Held {
                deadline: now + conservative(90)
            }
        );

        let shorter = renewed(held, now, now, 5);
        assert_eq!(shorter, held);

        assert_eq!(renewed(LeaseState::Lost, now, now, 90), LeaseState::Lost);
    }

    #[test]
    fn phase_order_is_one_step_forward_only() {
        assert_eq!(UploadPhase::Delete.next(), Some(UploadPhase::Modify));
        assert_eq!(UploadPhase::Modify.next(), Some(UploadPhase::Create));
        assert_eq!(UploadPhase::Create.next(), None);
        assert_eq!(UploadPhase::from_wire("checking"), None);
    }
}
