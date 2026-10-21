//! Wire-level metadata types shared by the client API and the photo helpers.

use crate::Error;
use crate::error::ServerFailure;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const DEFAULT_OWNER: &str = "_defaultOwner";
pub const PRIMARY_ZONE: &str = "PrimarySync";
pub const MASTER_RECORD_TYPE: &str = "CPLMaster";
pub const ASSET_RECORD_TYPE: &str = "CPLAsset";
pub const MAX_OPERATIONS: usize = 400;

/// A record zone. The server currently only serves `_defaultOwner`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ZoneId {
    pub zone_name: String,
    #[serde(default = "default_owner")]
    pub owner_record_name: String,
}

fn default_owner() -> String {
    DEFAULT_OWNER.into()
}

impl ZoneId {
    pub fn new(zone_name: impl Into<String>) -> Self {
        Self {
            zone_name: zone_name.into(),
            owner_record_name: default_owner(),
        }
    }

    pub fn primary() -> Self {
        Self::new(PRIMARY_ZONE)
    }

    pub fn validate(&self) -> Result<(), Error> {
        if !identifier(&self.zone_name) || self.owner_record_name != DEFAULT_OWNER {
            return Err(Error::Protocol(format!(
                "invalid zone {:?}/{:?}",
                self.zone_name, self.owner_record_name
            )));
        }
        Ok(())
    }
}

impl Default for ZoneId {
    fn default() -> Self {
        Self::primary()
    }
}

impl From<&str> for ZoneId {
    fn from(value: &str) -> Self {
        Self::new(value)
    }
}

impl From<String> for ZoneId {
    fn from(value: String) -> Self {
        Self::new(value)
    }
}

/// One typed record field. `kind` is kept as text so unknown future field types
/// survive a read/write cycle unchanged.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Field {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: Value,
}

impl Field {
    pub fn raw(kind: impl Into<String>, value: Value) -> Self {
        Self {
            kind: kind.into(),
            value,
        }
    }

    pub fn string(value: impl Into<String>) -> Self {
        Self::raw("STRING", Value::String(value.into()))
    }

    pub fn int64(value: i64) -> Self {
        Self::raw("INT64", json!(value))
    }

    pub fn double(value: f64) -> Self {
        Self::raw("DOUBLE", json!(value))
    }

    pub fn timestamp(value: i64) -> Self {
        Self::raw("TIMESTAMP", json!(value))
    }

    pub fn bytes(value: &[u8]) -> Self {
        Self::raw("BYTES", Value::String(STANDARD.encode(value)))
    }

    pub fn string_list(values: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self::raw(
            "STRING_LIST",
            Value::Array(values.into_iter().map(|v| json!(v.into())).collect()),
        )
    }

    pub fn reference(record_name: impl Into<String>) -> Self {
        Self::raw(
            "REFERENCE",
            json!({"recordName": record_name.into(), "action": "NONE"}),
        )
    }

    pub fn reference_in_zone(record_name: impl Into<String>, zone: &ZoneId) -> Self {
        Self::raw(
            "REFERENCE",
            json!({
                "recordName": record_name.into(),
                "action": "NONE",
                "zoneID": zone,
            }),
        )
    }

    /// An `ASSETID` field for a record write. The service verifies the sealed
    /// receipt, so a descriptor without one is rejected locally instead of
    /// failing at the server.
    pub fn asset(descriptor: &AssetDescriptor) -> Result<Self, Error> {
        if descriptor.receipt.is_none() {
            return Err(Error::Protocol(
                "an ASSETID field requires a fresh descriptor receipt".into(),
            ));
        }
        let value = serde_json::to_value(descriptor).map_err(|error| {
            Error::Protocol(format!("asset descriptor is not encodable: {error}"))
        })?;
        Ok(Self::raw("ASSETID", value))
    }

    pub fn as_str(&self) -> Option<&str> {
        self.value.as_str()
    }

    pub fn as_i64(&self) -> Option<i64> {
        self.value.as_i64()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self.value.as_i64() {
            Some(0) => Some(false),
            Some(1) => Some(true),
            _ => None,
        }
    }

    pub fn as_reference_name(&self) -> Option<&str> {
        self.value.get("recordName").and_then(Value::as_str)
    }

    pub fn as_bytes(&self) -> Option<Vec<u8>> {
        STANDARD.decode(self.value.as_str()?).ok()
    }

    pub fn as_asset(&self) -> Option<AssetDescriptor> {
        serde_json::from_value(self.value.clone()).ok()
    }
}

/// Identity of a stored resource. `download_url` and `receipt` are temporary
/// transport credentials; never persist them as record identity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDescriptor {
    pub asset_id: String,
    pub size: u64,
    /// Canonical padded Base64 SHA-256.
    pub file_checksum: String,
    #[serde(default = "sha256")]
    pub checksum_algorithm: String,
    #[serde(default)]
    pub mime_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(
        rename = "downloadURL",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub download_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub receipt: Option<String>,
}

fn sha256() -> String {
    "SHA256".into()
}

impl AssetDescriptor {
    /// Lowercase hex SHA-256, the form photo fields use.
    pub fn sha256_hex(&self) -> Result<String, Error> {
        let bytes = STANDARD
            .decode(&self.file_checksum)
            .map_err(|_| Error::Protocol("asset checksum is not valid Base64".into()))?;
        if bytes.len() != 32 || STANDARD.encode(&bytes) != self.file_checksum {
            return Err(Error::Protocol(
                "asset checksum is not a canonical SHA-256".into(),
            ));
        }
        Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
    }

    pub fn validate(&self) -> Result<(), Error> {
        if self.asset_id.parse::<uuid::Uuid>().is_err() || self.checksum_algorithm != "SHA256" {
            return Err(Error::Protocol("invalid asset descriptor".into()));
        }
        self.sha256_hex().map(|_| ())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditEvent {
    pub edited_at_ms: i64,
    pub source: String,
    pub event_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditEvents {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metadata_event: Option<EditEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_event: Option<EditEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletion_event: Option<EditEvent>,
}

/// One record exactly as the service returns it. Tombstones carry
/// `deleted: true`, no fields and a null record type.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRecord {
    pub record_name: String,
    #[serde(default)]
    pub record_type: Option<String>,
    #[serde(rename = "zoneID", default)]
    pub zone_id: Option<ZoneId>,
    #[serde(rename = "recordChangeTag", default)]
    pub record_change_tag: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Field>,
    #[serde(rename = "_syncserver", default)]
    pub events: EditEvents,
    #[serde(default)]
    pub deleted: bool,
}

impl RemoteRecord {
    pub fn is_tombstone(&self) -> bool {
        self.deleted
    }

    pub fn is_photo(&self) -> bool {
        matches!(
            self.record_type.as_deref(),
            Some(MASTER_RECORD_TYPE) | Some(ASSET_RECORD_TYPE)
        )
    }

    pub fn revision(&self) -> Option<&str> {
        self.record_change_tag.as_deref()
    }

    pub fn require_revision(&self) -> Result<&str, Error> {
        self.revision().ok_or_else(|| {
            Error::Protocol(format!(
                "record {:?} has no recordChangeTag",
                self.record_name
            ))
        })
    }

    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.get(name)
    }

    pub fn string(&self, name: &str) -> Option<&str> {
        self.field(name)?.as_str()
    }

    pub fn int64(&self, name: &str) -> Option<i64> {
        self.field(name)?.as_i64()
    }

    pub fn reference_name(&self, name: &str) -> Option<&str> {
        self.field(name)?.as_reference_name()
    }

    pub fn asset(&self, name: &str) -> Option<AssetDescriptor> {
        self.field(name)?.as_asset()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OperationType {
    Create,
    Update,
    Delete,
}

impl OperationType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
            Self::Delete => "delete",
        }
    }
}

/// One conditional record operation. A create omits the tag; an update or
/// delete must carry the tag it was read at, so the server can distinguish
/// "apply my change" from "overwrite whatever is there now".
#[derive(Clone, Debug)]
pub struct RecordOperation {
    pub operation_type: OperationType,
    pub record_name: String,
    pub record_type: Option<String>,
    pub record_change_tag: Option<String>,
    /// `None` removes a field on update; creates must only carry `Some`.
    pub fields: BTreeMap<String, Option<Field>>,
    pub events: Option<EditEvents>,
}

impl RecordOperation {
    pub fn create(
        record_name: impl Into<String>,
        record_type: impl Into<String>,
        fields: impl IntoIterator<Item = (String, Field)>,
    ) -> Self {
        Self {
            operation_type: OperationType::Create,
            record_name: record_name.into(),
            record_type: Some(record_type.into()),
            record_change_tag: None,
            fields: fields
                .into_iter()
                .map(|(key, field)| (key, Some(field)))
                .collect(),
            events: None,
        }
    }

    pub fn update(
        record_name: impl Into<String>,
        record_change_tag: impl Into<String>,
        fields: impl IntoIterator<Item = (String, Field)>,
    ) -> Self {
        Self::update_fields(
            record_name,
            record_change_tag,
            fields
                .into_iter()
                .map(|(key, field)| (key, Some(field)))
                .collect(),
        )
    }

    pub fn update_fields(
        record_name: impl Into<String>,
        record_change_tag: impl Into<String>,
        fields: BTreeMap<String, Option<Field>>,
    ) -> Self {
        Self {
            operation_type: OperationType::Update,
            record_name: record_name.into(),
            record_type: None,
            record_change_tag: Some(record_change_tag.into()),
            fields,
            events: None,
        }
    }

    pub fn delete(record_name: impl Into<String>, record_change_tag: impl Into<String>) -> Self {
        Self {
            operation_type: OperationType::Delete,
            record_name: record_name.into(),
            record_type: None,
            record_change_tag: Some(record_change_tag.into()),
            fields: BTreeMap::new(),
            events: None,
        }
    }

    pub fn with_events(mut self, events: EditEvents) -> Self {
        self.events = Some(events);
        self
    }

    fn validate(&self) -> Result<(), Error> {
        if !identifier(&self.record_name) {
            return Err(Error::Protocol(format!(
                "invalid record name {:?}",
                self.record_name
            )));
        }
        if self.fields.keys().any(|name| !identifier(name)) {
            return Err(Error::Protocol("invalid field name".into()));
        }
        match self.operation_type {
            OperationType::Create => {
                if self.record_type.as_deref().is_none_or(str::is_empty)
                    || self.record_change_tag.is_some()
                    || self.fields.values().any(Option::is_none)
                {
                    return Err(Error::Protocol(
                        "a create needs a record type, no change tag and concrete fields".into(),
                    ));
                }
            }
            OperationType::Update => {
                if self.record_change_tag.as_deref().is_none_or(str::is_empty) {
                    return Err(Error::Protocol(
                        "an update requires the recordChangeTag it was read at".into(),
                    ));
                }
            }
            OperationType::Delete => {
                if self.record_change_tag.as_deref().is_none_or(str::is_empty)
                    || !self.fields.is_empty()
                {
                    return Err(Error::Protocol(
                        "a delete requires a change tag and carries no fields".into(),
                    ));
                }
            }
        }
        Ok(())
    }

    pub(crate) fn wire(&self) -> Result<Value, Error> {
        self.validate()?;
        let mut record = Map::new();
        record.insert("recordName".into(), json!(self.record_name));
        if let Some(record_type) = &self.record_type {
            record.insert("recordType".into(), json!(record_type));
        }
        if let Some(tag) = &self.record_change_tag {
            record.insert("recordChangeTag".into(), json!(tag));
        }
        if !self.fields.is_empty() {
            let mut fields = Map::new();
            for (name, field) in &self.fields {
                fields.insert(
                    name.clone(),
                    match field {
                        Some(field) => serde_json::to_value(field).map_err(|error| {
                            Error::Protocol(format!("field is not encodable: {error}"))
                        })?,
                        None => Value::Null,
                    },
                );
            }
            record.insert("fields".into(), Value::Object(fields));
        }
        if let Some(events) = &self.events {
            record.insert(
                "_syncserver".into(),
                serde_json::to_value(events).map_err(|error| {
                    Error::Protocol(format!("events are not encodable: {error}"))
                })?,
            );
        }
        Ok(json!({
            "operationType": self.operation_type.as_str(),
            "record": Value::Object(record),
        }))
    }
}

/// A conditional, optionally atomic mutation. Atomic requests must target one
/// zone and commit or fail as a whole.
#[derive(Clone, Debug)]
pub struct ModifyRequest {
    pub zone: ZoneId,
    pub atomic: bool,
    pub operations: Vec<RecordOperation>,
    pub idempotency_key: Option<String>,
}

impl ModifyRequest {
    pub fn new(zone: ZoneId, operations: Vec<RecordOperation>) -> Self {
        Self {
            zone,
            atomic: true,
            operations,
            idempotency_key: None,
        }
    }

    pub fn atomic(mut self, atomic: bool) -> Self {
        self.atomic = atomic;
        self
    }

    pub fn idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    pub(crate) fn wire(&self) -> Result<Value, Error> {
        self.zone.validate()?;
        if self.operations.is_empty() || self.operations.len() > MAX_OPERATIONS {
            return Err(Error::Protocol(format!(
                "a mutation needs 1..={MAX_OPERATIONS} operations"
            )));
        }
        if let Some(key) = &self.idempotency_key
            && (key.is_empty()
                || key.len() > 128
                || !key.bytes().all(|byte| (33..=126).contains(&byte)))
        {
            return Err(Error::Protocol("invalid idempotency key".into()));
        }
        if self.atomic {
            let zones: BTreeSet<_> = self
                .operations
                .iter()
                .map(|_| self.zone.zone_name.clone())
                .collect();
            if zones.len() != 1 {
                return Err(Error::Protocol(
                    "an atomic mutation may target one zone".into(),
                ));
            }
        }
        let mut names = BTreeSet::new();
        for operation in &self.operations {
            if !names.insert(operation.record_name.clone()) {
                return Err(Error::Protocol(
                    "one mutation cannot mention the same record twice".into(),
                ));
            }
        }
        Ok(json!({
            "zoneID": self.zone,
            "atomic": self.atomic,
            "operations": self
                .operations
                .iter()
                .map(RecordOperation::wire)
                .collect::<Result<Vec<_>, _>>()?,
        }))
    }
}

/// One entry in a mutation response. Atomic batches report a top-level error,
/// but non-atomic batches report a failure per item.
#[derive(Clone, Debug)]
pub enum RecordResult {
    Confirmed(RemoteRecord),
    Failed(ServerFailure),
}

impl RecordResult {
    pub fn confirmed(&self) -> Option<&RemoteRecord> {
        match self {
            Self::Confirmed(record) => Some(record),
            Self::Failed(_) => None,
        }
    }

    pub(crate) fn from_value(value: &Value) -> Result<Self, Error> {
        if value.get("serverErrorCode").is_some() {
            let failure = serde_json::from_value(value.clone())
                .map_err(|error| Error::Protocol(format!("invalid record failure: {error}")))?;
            return Ok(Self::Failed(failure));
        }
        let record = serde_json::from_value(value.clone())
            .map_err(|error| Error::Protocol(format!("invalid record response: {error}")))?;
        Ok(Self::Confirmed(record))
    }
}

/// A record name, field name or other protocol identifier.
pub fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_helpers_use_the_wire_spellings() {
        let string = serde_json::to_value(Field::string("photo")).unwrap();
        assert_eq!(string, json!({"type": "STRING", "value": "photo"}));
        assert_eq!(Field::int64(1).as_bool(), Some(true));
        assert_eq!(Field::timestamp(42).as_i64(), Some(42));
        assert_eq!(
            Field::reference("master").as_reference_name(),
            Some("master")
        );
        assert_eq!(
            Field::bytes(b"abc").as_bytes().as_deref(),
            Some(b"abc".as_slice())
        );
    }

    #[test]
    fn create_and_update_shapes_match_the_service() {
        let create =
            RecordOperation::create("A", "CPLAsset", [("x".to_string(), Field::string("y"))]);
        assert_eq!(
            create.wire().unwrap(),
            json!({
                "operationType": "create",
                "record": {
                    "recordName": "A",
                    "recordType": "CPLAsset",
                    "fields": {"x": {"type": "STRING", "value": "y"}},
                },
            })
        );
        let update =
            RecordOperation::update_fields("A", "tag", BTreeMap::from([("x".to_string(), None)]));
        assert_eq!(
            update.wire().unwrap()["record"]["fields"],
            json!({"x": null})
        );
        assert!(RecordOperation::create("A", "T", []).wire().is_ok());
        assert!(RecordOperation::delete("A", "tag").wire().is_ok());
    }

    #[test]
    fn invalid_mutations_are_refused_before_network_use() {
        assert!(RecordOperation::delete("A", "").wire().is_err());
        let invalid = ModifyRequest::new(
            ZoneId::primary(),
            vec![
                RecordOperation::delete("A", "t"),
                RecordOperation::delete("A", "t"),
            ],
        );
        assert!(invalid.wire().is_err());
    }
}
