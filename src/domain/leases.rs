//! Upload ownership inputs; lease identity is independent from sync generation.
use super::{Namespace, records::ZoneId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Debug)]
pub struct LeaseProof {
    pub id: String,
    pub token: String,
    pub fence: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct LeaseClaims {
    pub namespace: Namespace,
    pub generation: Uuid,
    pub lease_id: Uuid,
    pub device_id: String,
    pub fence: String,
    pub zones: Vec<ZoneId>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Acquire {
    pub device_id: String,
    #[serde(rename = "zoneIDs")]
    pub zones: Vec<ZoneId>,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct ZoneToken {
    #[serde(rename = "zoneID")]
    pub zone: ZoneId,
    pub sync_token: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Check {
    pub zone_tokens: Vec<ZoneToken>,
    pub local_ready: bool,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Phase {
    pub phase: String,
}
pub(crate) enum Control {
    Acquire(Acquire),
    Renew(LeaseProof),
    Check(LeaseProof, Check),
    Phase(LeaseProof, Phase),
    Release(LeaseProof),
    Status(String),
}
