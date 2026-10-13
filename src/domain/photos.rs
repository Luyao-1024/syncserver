//! Validation uses only affected record components and metadata, never image bytes.
use super::records::{DomainResult, Failure, Record, ResolvedOperation};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PhotoBinding {
    pub photo_id: String,
    pub master_name: String,
    pub relative_path: String,
    pub generation: Uuid,
    pub original_asset_id: Uuid,
    pub thumbnail_asset_id: Uuid,
}
fn value<'a>(record: &'a Record, name: &str, kind: &str) -> DomainResult<&'a Value> {
    record
        .fields
        .get(name)
        .filter(|field| field.kind == kind)
        .map(|field| &field.value)
        .ok_or_else(Failure::pair)
}
fn text<'a>(record: &'a Record, name: &str) -> DomainResult<&'a str> {
    value(record, name, "STRING")?
        .as_str()
        .ok_or_else(Failure::pair)
}
fn generation(record: &Record) -> DomainResult<Uuid> {
    Uuid::parse_str(text(record, "contentGeneration")?).map_err(|_| Failure::pair())
}
pub fn neighbor(record: &Record) -> Option<String> {
    if record.deleted {
        return None;
    }
    match record.record_type.as_str() {
        "CPLAsset" => record
            .fields
            .get("masterRef")?
            .value
            .get("recordName")?
            .as_str()
            .map(str::to_owned),
        "CPLMaster" => record
            .fields
            .get("photoId")?
            .value
            .as_str()
            .map(str::to_owned),
        _ => None,
    }
}
// Only stable identity participates in the graph. Receipt verification belongs
// to the trusted publication boundary; persisted references have no receipt/URL.
fn asset(record: &Record, name: &str) -> DomainResult<(Uuid, String)> {
    let descriptor = value(record, name, "ASSETID")?;
    let id = Uuid::parse_str(
        descriptor
            .get("assetId")
            .and_then(Value::as_str)
            .ok_or_else(Failure::pair)?,
    )
    .map_err(|_| Failure::pair())?;
    let checksum = descriptor
        .get("fileChecksum")
        .and_then(Value::as_str)
        .ok_or_else(Failure::pair)?;
    let digest = STANDARD.decode(checksum).map_err(|_| Failure::pair())?;
    if id.is_nil()
        || digest.len() != 32
        || STANDARD.encode(&digest) != checksum
        || descriptor.get("checksumAlgorithm").and_then(Value::as_str) != Some("SHA256")
        || descriptor
            .get("size")
            .and_then(Value::as_u64)
            .is_none_or(|size| size > i64::MAX as u64)
    {
        return Err(Failure::pair());
    }
    Ok((id, digest.iter().map(|b| format!("{b:02x}")).collect()))
}
pub fn safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 4096 || path.contains(['\\', '\0']) {
        return false;
    }
    let parts: Vec<_> = path.split('/').collect();
    parts
        .iter()
        .all(|part| !part.is_empty() && !matches!(*part, "." | ".."))
        && !matches!(
            parts[0],
            ".syncserver" | ".photoviewer-sync" | ".sync-staging"
        )
}

pub fn validate_graph(
    before: &[Record],
    after: &[Record],
    operations: &[ResolvedOperation],
    atomic: bool,
) -> DomainResult<Vec<PhotoBinding>> {
    let old: BTreeMap<_, _> = before
        .iter()
        .filter(|r| r.is_photo() && !r.deleted)
        .map(|r| (&r.name.0, r))
        .collect();
    let active: BTreeMap<_, _> = after
        .iter()
        .filter(|r| r.is_photo() && !r.deleted)
        .map(|r| (&r.name.0, r))
        .collect();
    let actions: BTreeMap<_, _> = operations
        .iter()
        .map(|op| (&op.record_name.0, op.operation_type.as_str()))
        .collect();
    let paired = |asset: &str, master: &str, action: &str| {
        atomic
            && actions.get(&asset.to_owned()) == Some(&action)
            && actions.get(&master.to_owned()) == Some(&action)
    };
    for record in old.values() {
        if !active.contains_key(&record.name.0) {
            let other = neighbor(record).ok_or_else(Failure::pair)?;
            if !paired(&record.name.0, &other, "delete") {
                return Err(Failure::pair());
            }
        }
    }
    let mut bindings = Vec::new();
    let mut paths = BTreeSet::new();
    let mut masters = BTreeSet::new();
    for asset_record in active.values().filter(|r| r.record_type == "CPLAsset") {
        let reference = value(asset_record, "masterRef", "REFERENCE")?;
        if reference.get("action").and_then(Value::as_str) != Some("NONE") {
            return Err(Failure::pair());
        }
        let master_name = reference
            .get("recordName")
            .and_then(Value::as_str)
            .ok_or_else(Failure::pair)?;
        let master = active
            .get(&master_name.to_owned())
            .filter(|r| r.record_type == "CPLMaster" && r.zone == asset_record.zone)
            .ok_or_else(Failure::pair)?;
        if let Some(zone) = reference.get("zoneID") {
            let zone: super::records::ZoneId =
                serde_json::from_value(zone.clone()).map_err(|_| Failure::pair())?;
            if zone != asset_record.zone {
                return Err(Failure::pair());
            }
        }
        if text(master, "photoId")? != asset_record.name.0
            || !masters.insert(master_name.to_owned())
        {
            return Err(Failure::pair());
        }
        let content_generation = generation(asset_record)?;
        if content_generation.is_nil() || content_generation != generation(master)? {
            return Err(Failure::pair());
        }
        let relative_path = text(asset_record, "relativePath")?;
        if !safe_relative_path(relative_path) {
            return Err(Failure::pair());
        }
        if !paths.insert(relative_path.to_owned()) {
            return Err(Failure::new(
                409,
                "PATH_CONFLICT",
                "An active photo already uses this relative path",
            ));
        }
        for name in ["isFavorite", "isHidden"] {
            if asset_record.fields.get(name).is_some_and(|field| {
                field.kind != "INT64" || !matches!(field.value.as_i64(), Some(0 | 1))
            }) {
                return Err(Failure::pair());
            }
        }
        if asset_record
            .fields
            .get("assetDate")
            .is_some_and(|field| field.kind != "TIMESTAMP" || field.value.as_i64().is_none())
        {
            return Err(Failure::pair());
        }
        let (original, digest) = asset(master, "resOriginalRes")?;
        let (thumbnail, _) = asset(master, "resJPEGThumbRes")?;
        if text(master, "thumbnailSourceOriginalSHA256")? != digest {
            return Err(Failure::pair());
        }
        if let Some(old_asset) = old.get(&asset_record.name.0) {
            let old_master_name = neighbor(old_asset).ok_or_else(Failure::pair)?;
            let old_master = old.get(&old_master_name).ok_or_else(Failure::pair)?;
            if old_master_name != master_name
                || text(old_master, "photoId")? != text(master, "photoId")?
            {
                return Err(Failure::pair());
            }
            let resources_changed = asset(old_master, "resOriginalRes")?.0 != original
                || asset(old_master, "resJPEGThumbRes")?.0 != thumbnail;
            let generation_changed = generation(old_asset)? != content_generation;
            if (resources_changed && !generation_changed)
                || ((resources_changed || generation_changed)
                    && !paired(&asset_record.name.0, master_name, "update"))
            {
                return Err(Failure::pair());
            }
        } else if !paired(&asset_record.name.0, master_name, "create") {
            return Err(Failure::pair());
        }
        bindings.push(PhotoBinding {
            photo_id: asset_record.name.0.clone(),
            master_name: master_name.into(),
            relative_path: relative_path.into(),
            generation: content_generation,
            original_asset_id: original,
            thumbnail_asset_id: thumbnail,
        });
    }
    if masters.len()
        != active
            .values()
            .filter(|r| r.record_type == "CPLMaster")
            .count()
    {
        return Err(Failure::pair());
    }
    Ok(bindings)
}
