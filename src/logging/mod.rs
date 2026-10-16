//! Structured logging pipeline: one redaction path, a console renderer and a private
//! SQLite log database that keeps recent records raw and older records gzip compressed.
//!
//! Records reach the database through a bounded queue so logging can never block or
//! slow a request: when the queue is full the newest record is discarded and counted.

mod archive;
mod cli;
mod layer;
mod record;
mod store;

pub use cli::run;
pub use layer::LogLayer;
pub use record::{LogRecord, StoredRecord};
pub use store::{LOG_DATABASE, LogFilter, LogStats, Writer, query};

pub(crate) use record::now_ms;

use crate::{
    Error, Result,
    config::{LogFormat, LogLevel, LogSettings},
};
use serde_json::{Value, json};
use std::sync::OnceLock;
use std::{path::Path, sync::Arc};
use time::format_description::well_known::Rfc3339;
use tracing_subscriber::layer::SubscriberExt;

static SETTINGS: OnceLock<LogSettings> = OnceLock::new();
static WRITER: OnceLock<Option<Arc<Writer>>> = OnceLock::new();

/// Emits one record at a level chosen at runtime. Tracing callsites require a constant
/// level, so severity policies live in one place instead of being repeated per call site.
macro_rules! log_at {
    ($level:expr, $($fields:tt)*) => {
        match $level {
            tracing::Level::TRACE => tracing::trace!($($fields)*),
            tracing::Level::DEBUG => tracing::debug!($($fields)*),
            tracing::Level::INFO => tracing::info!($($fields)*),
            tracing::Level::WARN => tracing::warn!($($fields)*),
            _ => tracing::error!($($fields)*),
        }
    };
}

/// Installs the process-wide subscriber. Call once during binary startup, before any
/// component that should be able to log.
pub fn install(settings: &LogSettings) -> Result<()> {
    let writer = if settings.persist_level == LogLevel::Off {
        None
    } else {
        Some(Arc::new(Writer::spawn(settings)?))
    };
    let subscriber = tracing_subscriber::registry::Registry::default()
        .with(LogLayer::new(settings, writer.clone()));
    tracing::subscriber::set_global_default(subscriber)
        .map_err(|_| Error::Config("logging subscriber is already installed"))?;
    let _ = SETTINGS.set(settings.clone());
    let _ = WRITER.set(writer);
    Ok(())
}

/// Opens `<data_dir>/logs.sqlite`, flushing everything buffered since [`install`].
/// Called by the service only after the data directory lock belongs to this process.
pub(crate) fn activate(data_dir: &Path) -> Result<()> {
    match active_writer() {
        Some(writer) => writer.activate(data_dir),
        None => Ok(()),
    }
}

/// Commits and closes the log writer. Called after every other subsystem has stopped.
pub fn shutdown() -> Result<()> {
    match active_writer() {
        Some(writer) => writer.shutdown(),
        None => Ok(()),
    }
}

pub fn stats() -> Option<LogStats> {
    active_writer().map(|writer| writer.stats())
}

/// Metrics document fragment. Reading it never touches the log database.
pub(crate) fn metrics_value() -> Value {
    match stats() {
        Some(stats) => serde_json::to_value(stats).unwrap_or_else(|_| json!({"enabled": true})),
        None => json!({
            "enabled": false,
            "persistLevel": LogLevel::Off.as_str(),
            "activated": false
        }),
    }
}

/// Whether call sites should build expensive development detail records.
pub fn payload_enabled() -> bool {
    SETTINGS
        .get()
        .is_some_and(|settings| settings.payload_detail && settings.level.includes(LogLevel::Debug))
}

pub fn settings() -> Option<LogSettings> {
    SETTINGS.get().cloned()
}

/// Normalizes a user-supplied severity for filters and the storage CHECK constraint.
pub(crate) fn normalize_level(value: &str) -> String {
    value.trim().to_ascii_uppercase()
}

fn active_writer() -> Option<Arc<Writer>> {
    WRITER.get().and_then(|writer| writer.as_ref().cloned())
}

/// Renders one record for the console or the `logs` command output.
pub fn render(record: &LogRecord, format: LogFormat) -> String {
    match format {
        LogFormat::Json => {
            serde_json::to_string(&enriched(record)).unwrap_or_else(|_| record.message.clone())
        }
        LogFormat::Text => {
            let mut line = format!(
                "{} {} {} {}",
                timestamp(record.ts_ms),
                record.level,
                record.target,
                record.message
            );
            for (key, value) in &record.fields {
                line.push_str(&format!(" {key}={}", scalar(value)));
            }
            line
        }
    }
}

fn enriched(record: &LogRecord) -> Value {
    let mut value = serde_json::to_value(record).unwrap_or_else(|_| json!({}));
    if let Value::Object(map) = &mut value {
        map.insert(
            "timestamp".to_owned(),
            Value::String(timestamp(record.ts_ms)),
        );
    }
    value
}

fn timestamp(ts_ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ts_ms) * 1_000_000)
        .ok()
        .and_then(|moment| moment.format(&Rfc3339).ok())
        .unwrap_or_else(|| format!("epochms={ts_ms}"))
}

fn scalar(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}
