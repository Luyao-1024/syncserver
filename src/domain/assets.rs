//! Trusted immutable identity and explicit upload intent/role.
use super::{
    Namespace,
    leases::LeaseProof,
    records::{DomainResult, Failure, ZoneId, identifier},
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    Original,
    Thumbnail,
}
impl Role {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Thumbnail => "thumbnail",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UploadOptions {
    pub role: Role,
    pub intent: String,
    pub expected_size: u64,
    pub expected_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct UploadToken {
    pub record_name: String,
    pub record_type: String,
    pub field_name: String,
    #[serde(rename = "_syncserver")]
    pub options: UploadOptions,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Reserve {
    #[serde(rename = "zoneID", default)]
    pub zone: ZoneId,
    pub tokens: Vec<UploadToken>,
}
impl Reserve {
    pub fn validate(&self, max: u64, thumb_max: u64) -> DomainResult<()> {
        self.zone.validate()?;
        if self.tokens.is_empty() || self.tokens.len() > 200 {
            return Err(Failure::bad());
        }
        let mut seen = std::collections::BTreeSet::new();
        for token in &self.tokens {
            let options = &token.options;
            if !identifier(&token.record_name)
                || token.record_type != "CPLMaster"
                || !seen.insert((&token.record_name, &token.field_name))
                || !matches!(options.intent.as_str(), "create" | "update")
                || !matches!(
                    (&options.role, token.field_name.as_str()),
                    (Role::Original, "resOriginalRes") | (Role::Thumbnail, "resJPEGThumbRes")
                )
            {
                return Err(Failure::bad());
            }
            if options.expected_size == 0
                || options.expected_size
                    > match options.role {
                        Role::Original => max,
                        Role::Thumbnail => thumb_max,
                    }
            {
                return Err(Failure::limit());
            }
            if options.expected_sha256.as_ref().is_some_and(|s| !digest(s)) {
                return Err(Failure::bad());
            }
        }
        Ok(())
    }
}
pub(crate) fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Asset {
    pub asset_id: Uuid,
    pub sha256: String,
    pub size: u64,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub thumbnail_validated: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Receipt {
    pub namespace: Namespace,
    pub generation: Uuid,
    pub asset_id: Uuid,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub(crate) struct Capability {
    pub namespace: Namespace,
    pub generation: Uuid,
    pub id: Uuid,
    pub role: Role,
    pub expires: i64,
    pub lease_id: Option<String>,
    pub fence: Option<String>,
    pub intent: Option<String>,
}
#[derive(Clone, Debug)]
pub(crate) struct Session {
    pub id: Uuid,
    pub state: String,
    pub role: Role,
    pub expected_size: u64,
    pub expected_sha256: Option<String>,
    pub lease_id: String,
    pub fence: String,
    pub intent: String,
    pub expires_at_ms: i64,
    pub asset_id: Option<Uuid>,
    pub error: Option<String>,
    pub staging_name: Option<String>,
}
#[derive(Clone, Debug)]
pub(crate) struct Verified {
    pub session_id: Uuid,
    pub staging_name: String,
    pub asset: Asset,
}
pub(crate) enum AssetCommand {
    Storage,
    Reserve(Reserve, LeaseProof, Option<String>),
    Status(Uuid),
    Renew(Uuid, LeaseProof),
    Begin(Uuid, LeaseProof, String),
    Validate(Uuid, LeaseProof),
    Fail(Uuid, String),
    Prepare(Verified, LeaseProof),
    Complete(Uuid, LeaseProof),
    Describe(Uuid, Role),
    Metadata(Uuid, Role),
}
pub(crate) enum AssetReply {
    Json(serde_json::Value),
    Session(Session),
    Asset(Asset),
    Verified(Verified),
}
