//! Framework-independent record inputs, conditions and typed fields.
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};

pub const MAX_OPERATIONS: usize = 400;
pub const MAX_RECORD_BYTES: usize = 1_048_576;
pub const MAX_RESULT_BYTES: usize = 8_388_608;
pub type Fields = BTreeMap<String, Field>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RecordName(pub String);
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Revision(pub String);
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CommitSeq(pub i64);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ZoneId {
    pub zone_name: String,
    #[serde(default = "default_owner")]
    pub owner_record_name: String,
}
fn default_owner() -> String {
    "_defaultOwner".into()
}
impl Default for ZoneId {
    fn default() -> Self {
        Self {
            zone_name: "PrimarySync".into(),
            owner_record_name: default_owner(),
        }
    }
}
impl ZoneId {
    pub fn validate(&self) -> DomainResult<()> {
        if !identifier(&self.zone_name) || self.owner_record_name != "_defaultOwner" {
            return Err(Failure::bad());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Field {
    #[serde(rename = "type")]
    pub kind: String,
    pub value: Value,
}
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    pub name: RecordName,
    pub record_type: String,
    pub zone: ZoneId,
    pub revision: Revision,
    pub seq: CommitSeq,
    pub fields: Fields,
    pub deleted: bool,
}
impl Record {
    pub fn wire(&self) -> Value {
        if self.deleted {
            json!({"recordName":self.name,"recordType":null,"zoneID":self.zone,"recordChangeTag":self.revision,"deleted":true})
        } else {
            json!({"recordName":self.name,"recordType":self.record_type,"zoneID":self.zone,"recordChangeTag":self.revision,"fields":self.fields})
        }
    }
    pub fn is_photo(&self) -> bool {
        matches!(self.record_type.as_str(), "CPLAsset" | "CPLMaster")
    }
}

#[derive(Clone, Debug)]
pub struct Failure {
    pub status: u16,
    pub code: &'static str,
    pub reason: &'static str,
    pub record: Option<Box<Record>>,
}
pub type DomainResult<T> = std::result::Result<T, Failure>;
impl Failure {
    pub fn new(status: u16, code: &'static str, reason: &'static str) -> Self {
        Self {
            status,
            code,
            reason,
            record: None,
        }
    }
    pub fn bad() -> Self {
        Self::new(400, "BAD_REQUEST", "Invalid record request or field value")
    }
    pub fn unsupported() -> Self {
        Self::new(
            501,
            "UNSUPPORTED_CAPABILITY",
            "The requested operation or field capability is unavailable",
        )
    }
    pub fn pair() -> Self {
        Self::new(
            409,
            "PHOTO_PAIR_REQUIRED",
            "Photo records require a valid atomic pair",
        )
    }
    pub fn limit() -> Self {
        Self::new(
            413,
            "LIMIT_EXCEEDED",
            "Record, batch or response limit exceeded",
        )
    }
    pub fn unknown() -> Self {
        Self::new(404, "UNKNOWN_ITEM", "Record or zone does not exist")
    }
    pub fn conflict(record: &Record) -> Self {
        let mut result = Self::new(
            409,
            "CONFLICT",
            "recordChangeTag does not match current revision",
        );
        result.record = Some(Box::new(record.clone()));
        result
    }
    pub fn wire(&self) -> Value {
        let mut value = json!({"serverErrorCode":self.code,"reason":self.reason});
        if let Some(record) = &self.record {
            value["serverRecord"] = record.wire();
        }
        value
    }
    pub fn item(&self, name: &str, zone: &ZoneId) -> Value {
        let mut value = self.wire();
        value["recordName"] = json!(name);
        value["zoneID"] = json!(zone);
        value
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordInput {
    pub record_name: String,
    pub record_type: Option<String>,
    #[serde(rename = "zoneID")]
    pub zone_id: Option<ZoneId>,
    pub record_change_tag: Option<String>,
    #[serde(default)]
    pub fields: BTreeMap<String, Option<Field>>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Operation {
    pub operation_type: String,
    pub record: RecordInput,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModifyRequest {
    #[serde(rename = "zoneID")]
    pub zone_id: Option<ZoneId>,
    #[serde(default = "atomic_default")]
    pub(crate) atomic: bool,
    pub operations: Vec<Operation>,
}
fn atomic_default() -> bool {
    true
}
#[derive(Clone, Debug, Serialize)]
pub struct ResolvedOperation {
    pub operation_type: String,
    pub record_name: RecordName,
    pub record_type: Option<String>,
    pub zone: ZoneId,
    pub revision: Option<Revision>,
    pub fields: BTreeMap<String, Option<Field>>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Modify {
    pub(crate) atomic: bool,
    pub(crate) operations: Vec<ResolvedOperation>,
    #[serde(skip)]
    pub(crate) idempotency_key: Option<String>,
}
impl ModifyRequest {
    pub fn prepare(self, key: Option<String>) -> DomainResult<Modify> {
        if self.operations.is_empty() {
            return Err(Failure::bad());
        }
        if self.operations.len() > MAX_OPERATIONS {
            return Err(Failure::limit());
        }
        let default_zone = self.zone_id.unwrap_or_default();
        default_zone.validate()?;
        if key.as_ref().is_some_and(|s| {
            s.is_empty() || s.len() > 128 || !s.bytes().all(|b| (33..=126).contains(&b))
        }) {
            return Err(Failure::bad());
        }
        let mut names = BTreeSet::new();
        let mut zones = BTreeSet::new();
        let mut operations = Vec::new();
        for op in self.operations {
            if !identifier(&op.record.record_name) {
                return Err(Failure::bad());
            }
            let zone = op.record.zone_id.unwrap_or_else(|| default_zone.clone());
            zone.validate()?;
            if !names.insert((zone.zone_name.clone(), op.record.record_name.clone())) {
                return Err(Failure::bad());
            }
            zones.insert(zone.zone_name.clone());
            let mut fields = op.record.fields;
            for field in fields.values_mut().flatten() {
                normalize_reference(field, &zone);
            }
            operations.push(ResolvedOperation {
                operation_type: op.operation_type,
                record_name: RecordName(op.record.record_name),
                record_type: op.record.record_type,
                zone,
                revision: op.record.record_change_tag.map(Revision),
                fields,
            });
        }
        if self.atomic && zones.len() != 1 {
            return Err(Failure::bad());
        }
        Ok(Modify {
            atomic: self.atomic,
            operations,
            idempotency_key: key,
        })
    }
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupItem {
    pub record_name: String,
    #[serde(rename = "zoneID")]
    pub zone_id: Option<ZoneId>,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LookupRequest {
    #[serde(rename = "zoneID")]
    pub zone_id: Option<ZoneId>,
    pub records: Vec<LookupItem>,
    pub desired_keys: Option<Vec<String>>,
}
#[derive(Clone, Debug)]
pub struct Lookup {
    pub(crate) records: Vec<(ZoneId, RecordName)>,
    pub(crate) desired_keys: Option<BTreeSet<String>>,
}
impl LookupRequest {
    pub fn prepare(self) -> DomainResult<Lookup> {
        if self.records.is_empty() {
            return Err(Failure::bad());
        }
        if self.records.len() > MAX_OPERATIONS {
            return Err(Failure::limit());
        }
        let default_zone = self.zone_id.unwrap_or_default();
        default_zone.validate()?;
        let mut records = Vec::new();
        for item in self.records {
            if !identifier(&item.record_name) {
                return Err(Failure::bad());
            }
            let zone = item.zone_id.unwrap_or_else(|| default_zone.clone());
            zone.validate()?;
            records.push((zone, RecordName(item.record_name)));
        }
        if self
            .desired_keys
            .as_ref()
            .is_some_and(|keys| keys.len() > 256 || keys.iter().any(|key| !identifier(key)))
        {
            return Err(Failure::bad());
        }
        Ok(Lookup {
            records,
            desired_keys: self.desired_keys.map(|keys| keys.into_iter().collect()),
        })
    }
}

pub fn identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}

pub fn validate_fields(fields: &Fields, zone: &ZoneId) -> DomainResult<()> {
    if fields.len() > 256 {
        return Err(Failure::limit());
    }
    for (name, field) in fields {
        if !identifier(name) {
            return Err(Failure::bad());
        }
        validate_field(field, zone)?;
    }
    if serde_json::to_vec(fields)
        .map_err(|_| Failure::bad())?
        .len()
        > MAX_RECORD_BYTES
    {
        return Err(Failure::limit());
    }
    Ok(())
}
fn validate_field(field: &Field, zone: &ZoneId) -> DomainResult<()> {
    if let Some(kind) = field.kind.strip_suffix("_LIST") {
        if !matches!(
            kind,
            "STRING" | "INT64" | "DOUBLE" | "TIMESTAMP" | "BYTES" | "REFERENCE"
        ) {
            return Err(Failure::unsupported());
        }
        let values = field
            .value
            .as_array()
            .filter(|v| v.len() <= 4096)
            .ok_or_else(Failure::bad)?;
        for value in values {
            validate_field(
                &Field {
                    kind: kind.into(),
                    value: value.clone(),
                },
                zone,
            )?;
        }
        return Ok(());
    }
    let valid = match field.kind.as_str() {
        "STRING" => field.value.is_string(),
        "INT64" | "TIMESTAMP" => field.value.as_i64().is_some(),
        "DOUBLE" => field.value.as_f64().is_some_and(f64::is_finite),
        "BYTES" => field.value.as_str().is_some_and(|text| {
            STANDARD
                .decode(text)
                .is_ok_and(|bytes| STANDARD.encode(bytes) == text)
        }),
        "REFERENCE" => {
            let object = field.value.as_object().ok_or_else(Failure::bad)?;
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "recordName" | "action" | "zoneID"))
            {
                return Err(Failure::bad());
            }
            let reference_zone = object
                .get("zoneID")
                .map(|v| serde_json::from_value::<ZoneId>(v.clone()).map_err(|_| Failure::bad()))
                .transpose()?;
            reference_zone.as_ref().map(ZoneId::validate).transpose()?;
            object
                .get("recordName")
                .and_then(Value::as_str)
                .is_some_and(identifier)
                && object.get("action").and_then(Value::as_str) == Some("NONE")
                && reference_zone.as_ref().is_none_or(|other| other == zone)
        }
        "ASSETID" => field.value.is_object(), // Photo graph parses descriptors; all asset writes require M4's trusted verifier.
        _ => return Err(Failure::unsupported()),
    };
    if !valid {
        return Err(Failure::bad());
    }
    Ok(())
}

pub fn apply(op: &ResolvedOperation, current: Option<&Record>) -> DomainResult<Record> {
    if !matches!(op.operation_type.as_str(), "create" | "update" | "delete") {
        return Err(Failure::unsupported());
    }
    if op
        .record_type
        .as_ref()
        .is_some_and(|name| !identifier(name))
        || op.revision.as_ref().is_some_and(|tag| !identifier(&tag.0))
    {
        return Err(Failure::bad());
    }
    let mut record = match op.operation_type.as_str() {
        "create" => {
            if op.revision.is_some()
                || op.record_type.is_none()
                || op.fields.values().any(Option::is_none)
            {
                return Err(Failure::bad());
            }
            if let Some(current) = current {
                if current.deleted {
                    return Err(Failure::new(
                        409,
                        "RECORD_NAME_RETIRED",
                        "A tombstoned record name cannot be reused",
                    ));
                }
                return Err(Failure::conflict(current));
            }
            Record {
                name: op.record_name.clone(),
                record_type: op.record_type.clone().unwrap_or_default(),
                zone: op.zone.clone(),
                revision: Revision(String::new()),
                seq: CommitSeq(0),
                fields: Fields::new(),
                deleted: false,
            }
        }
        _ => {
            let tag = op.revision.as_ref().ok_or_else(Failure::bad)?;
            let current = current.ok_or_else(Failure::unknown)?;
            if tag != &current.revision {
                return Err(Failure::conflict(current));
            }
            if current.deleted {
                return Err(Failure::unknown());
            }
            if op
                .record_type
                .as_ref()
                .is_some_and(|kind| kind != &current.record_type)
            {
                return Err(Failure::new(
                    409,
                    "RECORD_TYPE_IMMUTABLE",
                    "recordType cannot change",
                ));
            }
            current.clone()
        }
    };
    if op.operation_type == "delete" {
        if !op.fields.is_empty() {
            return Err(Failure::bad());
        }
        record.deleted = true;
        record.fields.clear();
    } else {
        for (name, field) in &op.fields {
            if !identifier(name) {
                return Err(Failure::bad());
            }
            match field {
                Some(field) => {
                    record.fields.insert(name.clone(), field.clone());
                }
                None => {
                    record.fields.remove(name);
                }
            }
        }
        validate_fields(&record.fields, &record.zone)?;
    }
    Ok(record)
}

fn normalize_reference(field: &mut Field, zone: &ZoneId) {
    let reference = |value: &mut Value| {
        if let Some(object) = value.as_object_mut() {
            if !object.contains_key("zoneID") {
                object.insert(
                    "zoneID".into(),
                    serde_json::to_value(zone).unwrap_or(Value::Null),
                );
            } else if let Some(explicit) = object.get_mut("zoneID").and_then(Value::as_object_mut) {
                explicit
                    .entry("ownerRecordName")
                    .or_insert_with(|| Value::String("_defaultOwner".into()));
            }
        }
    };
    if field.kind == "REFERENCE" {
        reference(&mut field.value);
    } else if field.kind == "REFERENCE_LIST"
        && let Some(values) = field.value.as_array_mut()
    {
        for value in values {
            reference(value);
        }
    }
}
