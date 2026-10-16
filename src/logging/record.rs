//! Record assembly plus the single redaction path shared by console and database.

use crate::config::LogLevel;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use tracing::field::{Field, Visit};

/// Stored and archived wire form of one log record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogRecord {
    pub ts_ms: i64,
    /// Uppercase severity: ERROR, WARN, INFO, DEBUG or TRACE.
    pub level: String,
    pub target: String,
    pub message: String,
    #[serde(default)]
    pub fields: Map<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Archive line: the record together with its original sequence number.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ArchiveLine {
    pub seq: i64,
    #[serde(flatten)]
    pub record: LogRecord,
}

/// A persisted record with its database sequence number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRecord {
    pub seq: i64,
    pub record: LogRecord,
}

/// Request correlation copied from the enclosing span.
#[derive(Clone, Debug, Default)]
pub(crate) struct Correlation {
    pub request_id: Option<String>,
}

/// Any single text value is clipped so one pathological field cannot dominate storage.
const MAX_TEXT_CHARS: usize = 512;
const SUPPRESSED: &str = "[detail-off]";

/// Name fragments that never leave the process, even with development detail logging.
const SECRET_FRAGMENTS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "credential",
    "authorization",
    "cookie",
    "apikey",
    "api_key",
    "signing",
    "private_key",
    "token",
    "proof",
    "signature",
    "receipt",
    "capability",
    "session",
    "bearer",
    "url",
];

/// Name fragments carrying library or user content: rendered only when detail logging is on.
const DETAIL_FRAGMENTS: &[&str] = &[
    "record_name",
    "recordname",
    "relative_path",
    "relativepath",
    "path",
    "filename",
    "username",
    "title",
    "album",
    "label",
    "zone",
    "desired_keys",
    "desiredkeys",
    "desired_key",
    "desiredkey",
    "record_type",
    "recordtype",
    "body",
    "payload",
    "value",
    "preview",
    "summary",
    "device",
    "device_id",
    "digest",
    "hash",
    "checksum",
    "etag",
];

fn is_secret(name: &str) -> bool {
    SECRET_FRAGMENTS.iter().any(|part| name.contains(part))
}

fn is_detail(name: &str) -> bool {
    DETAIL_FRAGMENTS.iter().any(|part| name.contains(part))
}

fn clip(text: &str) -> String {
    let count = text.chars().count();
    if count <= MAX_TEXT_CHARS {
        return text.to_owned();
    }
    let kept: String = text.chars().take(MAX_TEXT_CHARS).collect();
    format!("{kept}…[{} chars clipped]", count - MAX_TEXT_CHARS)
}

fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Collects tracing fields into a message plus a redacted JSON field map.
pub(crate) struct Collector {
    pub message: String,
    pub fields: Map<String, Value>,
    pub request_id: Option<String>,
    payload_detail: bool,
}

impl Collector {
    pub(crate) fn new(payload_detail: bool) -> Self {
        Self {
            message: String::new(),
            fields: Map::new(),
            request_id: None,
            payload_detail,
        }
    }

    fn put(&mut self, name: &str, value: Value) {
        let lowered = name.to_ascii_lowercase();
        if lowered == "message" {
            self.message = clip(&text_of(&value));
            return;
        }
        if lowered == "request_id" || lowered == "requestid" {
            let text = clip(&text_of(&value));
            self.request_id = Some(text.clone());
            self.fields.insert(name.to_owned(), Value::String(text));
            return;
        }
        let stored = if is_secret(&lowered) {
            Value::String(format!("[redacted:{}]", text_of(&value).chars().count()))
        } else if !self.payload_detail && is_detail(&lowered) {
            Value::String(SUPPRESSED.to_owned())
        } else {
            match value {
                Value::String(text) => Value::String(clip(&text)),
                other => other,
            }
        };
        self.fields.insert(name.to_owned(), stored);
    }
}

impl Visit for Collector {
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.put(field.name(), Value::from(value));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.put(field.name(), Value::from(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.put(field.name(), Value::from(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.put(field.name(), Value::from(value));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.put(field.name(), Value::String(value.to_owned()));
    }
    fn record_error(&mut self, field: &Field, value: &(dyn std::error::Error + 'static)) {
        self.put(field.name(), Value::String(value.to_string()));
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.put(field.name(), Value::String(format!("{value:?}")));
    }
}

pub(crate) fn level_of(level: &tracing::Level) -> LogLevel {
    match *level {
        tracing::Level::ERROR => LogLevel::Error,
        tracing::Level::WARN => LogLevel::Warn,
        tracing::Level::INFO => LogLevel::Info,
        tracing::Level::DEBUG => LogLevel::Debug,
        tracing::Level::TRACE => LogLevel::Trace,
    }
}

pub(crate) fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|value| i64::try_from(value.as_millis()).ok())
        .unwrap_or_default()
}
