//! Independent edit events; upload/commit time is not a client's edit time.
use super::records::{DomainResult, Failure, Record, ResolvedOperation, identifier};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditEvent {
    pub edited_at_ms: i64,
    pub source: String,
    pub event_id: String,
}
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EditEvents {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata_event: Option<EditEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_event: Option<EditEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deletion_event: Option<EditEvent>,
}
impl EditEvents {
    pub fn validate(&self) -> DomainResult<()> {
        for event in [
            &self.metadata_event,
            &self.content_event,
            &self.deletion_event,
        ]
        .into_iter()
        .flatten()
        {
            if event.edited_at_ms < 0 || !identifier(&event.source) || !identifier(&event.event_id)
            {
                return Err(Failure::bad());
            }
        }
        Ok(())
    }
}

const CONTENT_KEYS: &[&str] = &[
    "contentGeneration",
    "resOriginalRes",
    "resJPEGThumbRes",
    "thumbnailSourceOriginalSHA256",
    "masterRef",
];
/// Called only after conditions and final photo graph validation. New events
/// replace only the corresponding category; old events survive independent edits.
pub(crate) fn assign(
    record: &mut Record,
    previous: Option<&Record>,
    operation: &ResolvedOperation,
    committed_at_ms: i64,
) -> DomainResult<()> {
    let provided = operation.events.clone().unwrap_or_default();
    let creating = operation.operation_type == "create";
    let deleting = operation.operation_type == "delete";
    let content = !deleting
        && record.is_photo()
        && (creating
            || CONTENT_KEYS.iter().any(|key| {
                previous.map(|old| old.fields.get(*key)) != Some(record.fields.get(*key))
            }));
    let metadata = !deleting
        && (creating
            || operation
                .fields
                .keys()
                .any(|key| !record.is_photo() || !CONTENT_KEYS.contains(&key.as_str())));
    if (provided.metadata_event.is_some() && !metadata)
        || (provided.content_event.is_some() && !content)
        || (provided.deletion_event.is_some() && !deleting)
    {
        return Err(Failure::bad());
    }
    let default = || EditEvent {
        edited_at_ms: committed_at_ms,
        source: "server".into(),
        event_id: record.revision.0.clone(),
    };
    if metadata {
        record.events.metadata_event = Some(provided.metadata_event.unwrap_or_else(default));
    }
    if content {
        record.events.content_event = Some(provided.content_event.unwrap_or_else(default));
    }
    if deleting {
        record.events.deletion_event = Some(provided.deletion_event.unwrap_or_else(default));
    }
    Ok(())
}
