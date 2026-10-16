mod common;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use syncserver::{
    LogFormat, LogLevel, LogSettings,
    logging::{LOG_DATABASE, LogFilter, LogLayer, LogRecord, Writer, query},
};
use tracing_subscriber::{layer::SubscriberExt, registry::Registry};

const REQUEST_ID: &str = "0d1f2c3a-5b67-4a89-9b0c-1d2e3f405a6b";

fn configure(level: LogLevel, persist: LogLevel) -> LogSettings {
    LogSettings {
        level,
        persist_level: persist,
        format: LogFormat::Text,
        payload_detail: false,
        retention_raw_secs: 86_400,
        archive_max_bytes: 1_073_741_824,
        queue_capacity: 1024,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn record(message: &str, ts_ms: i64, fields: Value) -> LogRecord {
    LogRecord {
        ts_ms,
        level: "INFO".to_owned(),
        target: "syncserver::test".to_owned(),
        message: message.to_owned(),
        fields: fields.as_object().cloned().unwrap_or_else(Map::new),
        request_id: None,
    }
}

/// Activates the writer inside a private service-owned directory, mirroring the
/// production requirement that the data directory is mode 0700.
fn open(root: &Path, settings: &LogSettings) -> Arc<Writer> {
    let state = root.join("state");
    let writer = Arc::new(Writer::spawn(settings).unwrap());
    writer.activate(&state).unwrap();
    writer
}

fn state(root: &Path) -> std::path::PathBuf {
    root.join("state")
}

fn database(root: &Path) -> std::path::PathBuf {
    state(root).join(LOG_DATABASE)
}

fn capture(writer: &Arc<Writer>, settings: &LogSettings, emit: impl FnOnce()) {
    let subscriber = Registry::default().with(LogLayer::new(settings, Some(writer.clone())));
    tracing::subscriber::with_default(subscriber, emit);
}

#[test]
fn persist_level_filters_records_that_only_reach_the_console() {
    let root = tempfile::tempdir().unwrap();
    let settings = configure(LogLevel::Info, LogLevel::Warn);
    let writer = open(root.path(), &settings);
    capture(&writer, &settings, || {
        tracing::warn!("persisted warning");
        tracing::info!("console only message");
    });
    writer.flush().unwrap();
    let rows = query(&database(root.path()), &LogFilter::default()).unwrap();
    assert_eq!(rows.len(), 1, "only the threshold and above persist");
    assert_eq!(rows[0].record.level, "WARN");
    assert_eq!(rows[0].record.message, "persisted warning");
    assert_eq!(writer.stats().persisted, 1);
    writer.shutdown().unwrap();
}

#[test]
fn secrets_are_always_redacted_and_library_data_needs_detail_logging() {
    let root = tempfile::tempdir().unwrap();
    let settings = configure(LogLevel::Debug, LogLevel::Debug);
    let writer = open(root.path(), &settings);
    capture(&writer, &settings, || {
        tracing::info!(
            request_id = %REQUEST_ID,
            password = "super-secret-value",
            lease_token = "lease-token-value",
            public_base_url = "https://sync.example.invalid",
            record_name = "IMG_0001",
            relative_path = "Album/IMG_0001.heic",
            zone = "PrimarySync",
            items = 3u64,
            atomic = true,
            "unit of work"
        );
        tracing::debug!(content_digest = "aa11bb22cc33", "detail record");
    });
    writer.flush().unwrap();
    let rows = query(&database(root.path()), &LogFilter::default()).unwrap();
    assert_eq!(rows.len(), 2);
    let work = &rows[0].record;
    assert_eq!(work.request_id.as_deref(), Some(REQUEST_ID));
    assert_eq!(work.fields["items"], json!(3));
    assert_eq!(work.fields["atomic"], json!(true));
    for secret in ["password", "lease_token", "public_base_url"] {
        let text = work.fields[secret].as_str().unwrap_or_default();
        assert!(text.starts_with("[redacted:"), "{secret} leaked: {text}");
        assert!(
            !text.contains("super") && !text.contains("lease-token") && !text.contains("invalid")
        );
    }
    for detail in ["record_name", "relative_path", "zone"] {
        assert_eq!(
            work.fields[detail].as_str(),
            Some("[detail-off]"),
            "{detail} leaked without detail logging"
        );
    }
    assert_eq!(
        rows[1].record.fields["content_digest"].as_str(),
        Some("[detail-off]")
    );
    writer.shutdown().unwrap();

    // Development detail mode reveals library data fields but never credentials.
    let detailed = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Debug, LogLevel::Debug);
    settings.payload_detail = true;
    let writer = open(detailed.path(), &settings);
    capture(&writer, &settings, || {
        tracing::info!(
            record_name = "IMG_0002",
            relative_path = "Album/IMG_0002.heic",
            password = "hunter2secret",
            "detail run"
        );
    });
    writer.flush().unwrap();
    let rows = query(&database(detailed.path()), &LogFilter::default()).unwrap();
    assert_eq!(
        rows[0].record.fields["record_name"].as_str(),
        Some("IMG_0002")
    );
    assert_eq!(
        rows[0].record.fields["relative_path"].as_str(),
        Some("Album/IMG_0002.heic")
    );
    assert_eq!(
        rows[0].record.fields["password"].as_str(),
        Some("[redacted:13]")
    );
    writer.shutdown().unwrap();
}

#[test]
fn records_older_than_the_threshold_are_compressed_into_segments() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Info, LogLevel::Info);
    settings.retention_raw_secs = 1;
    let writer = open(root.path(), &settings);
    let old = now_ms() - 60_000;
    for index in 0..25 {
        writer.publish(record(
            &format!("archived record {index}"),
            old + index,
            json!({"index": index}),
        ));
    }
    writer.publish(record("fresh record", now_ms(), json!({})));
    writer.flush().unwrap();
    assert_eq!(
        query(&database(root.path()), &LogFilter::default())
            .unwrap()
            .len(),
        26
    );
    writer.archive().unwrap();

    let stats = writer.stats();
    assert_eq!(stats.raw_rows, 1, "the current window stays uncompressed");
    assert_eq!(stats.archived_rows, 25);
    assert_eq!(stats.archive_segments, 1);
    assert_eq!(stats.trimmed_segments, 0);
    assert!(stats.archive_bytes < stats.raw_rows.max(1) * 4096);

    // Raw storage keeps only the recent record.
    let raw = query(&database(root.path()), &LogFilter::default()).unwrap();
    assert_eq!(raw.len(), 1);
    assert_eq!(raw[0].record.message, "fresh record");

    // The archived window is readable through the same filter and really is gzip data.
    let filter = LogFilter {
        include_archive: true,
        limit: 100,
        ..LogFilter::default()
    };
    let all = query(&database(root.path()), &filter).unwrap();
    assert_eq!(all.len(), 26);
    assert_eq!(all[0].record.message, "archived record 0");
    assert_eq!(all[24].record.message, "archived record 24");
    assert_eq!(all[24].seq, all[0].seq + 24, "sequence order is preserved");
    assert_eq!(
        all[25].record.message, "fresh record",
        "newest stays in the raw window"
    );
    assert_eq!(all[3].record.fields["index"], json!(3));

    let connection = rusqlite::Connection::open(database(root.path())).unwrap();
    let (codec, row_count, checksum, payload): (String, i64, String, Vec<u8>) = connection
        .query_row(
            "SELECT codec, row_count, checksum, payload FROM log_segments",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!((codec.as_str(), row_count), ("gzip", 25));
    assert_eq!(&payload[..2], &[0x1f, 0x8b], "segment payload is gzip");
    assert_eq!(checksum, hex(Sha256::digest(&payload)));
    writer.shutdown().unwrap();
    assert!(database(root.path()).exists());
}

#[test]
fn compressed_archive_stays_under_its_cap_and_drops_oldest_segments_first() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Info, LogLevel::Info);
    settings.retention_raw_secs = 1;
    settings.archive_max_bytes = 4096;
    let writer = open(root.path(), &settings);
    let old = now_ms() - 120_000;
    let mut noise = 0x2545_f491_4f6c_dd1du64;
    for pass in 0..60 {
        for slot in 0..3 {
            noise = noise
                .wrapping_mul(6364136223846793005)
                .wrapping_add(14426950408889034);
            let filler = format!("{:016x}", noise >> 32);
            writer.publish(record(
                &format!("pass {pass} slot {slot} {filler}"),
                old + pass * 1000 + slot,
                json!({"pass": pass}),
            ));
        }
        writer.flush().unwrap();
        writer.archive().unwrap();
        let stats = writer.stats();
        assert!(
            stats.archive_bytes <= settings.archive_max_bytes,
            "archive grew past its cap at pass {pass}"
        );
    }
    let stats = writer.stats();
    assert!(
        stats.trimmed_segments >= 1,
        "oldest segments must be trimmed"
    );
    let filter = LogFilter {
        include_archive: true,
        limit: 2000,
        ..LogFilter::default()
    };
    let rows = query(&database(root.path()), &filter).unwrap();
    let passes: Vec<u64> = rows
        .iter()
        .filter_map(|row| row.record.fields.get("pass").and_then(Value::as_u64))
        .collect();
    assert!(passes.contains(&59), "the newest archive survived");
    assert!(
        !passes.contains(&0),
        "the oldest archive was discarded first"
    );
    assert_eq!(stats.raw_rows, 0);
    writer.shutdown().unwrap();
}

#[test]
fn a_full_log_queue_drops_records_instead_of_blocking_the_caller() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Off, LogLevel::Info);
    settings.queue_capacity = 16;
    let writer = Arc::new(Writer::spawn(&settings).unwrap());
    let started = Instant::now();
    for index in 0..400 {
        writer.publish(record(&format!("burst {index}"), now_ms(), json!({})));
    }
    let elapsed = started.elapsed();
    assert!(
        elapsed < Duration::from_secs(2),
        "publishing blocked {elapsed:?}"
    );
    assert_eq!(
        writer.stats().persisted,
        0,
        "nothing is stored before activation"
    );
    let stats = writer.stats();
    assert!(
        stats.dropped_queue_full + stats.dropped_pending > 300,
        "queue overflow was not counted"
    );
    writer.activate(&state(root.path())).unwrap();
    writer.flush().unwrap();
    let stats = writer.stats();
    assert!(stats.persisted > 0, "buffered records survived activation");
    assert!(stats.persisted <= 64, "bounded buffering was not enforced");
    writer.shutdown().unwrap();
}

#[test]
fn logs_command_reads_the_live_database_and_rejects_bad_options() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config(root.path());
    assert!(
        syncserver::logging::run(config.clone(), &["--json".to_owned()]).is_err(),
        "a missing log database must be reported"
    );
    let settings = configure(LogLevel::Info, LogLevel::Info);
    // The CLI reads the service data directory, so activate the writer there.
    let writer = Arc::new(Writer::spawn(&settings).unwrap());
    writer.activate(config.data_directory()).unwrap();
    writer.publish(record(
        "operational entry",
        now_ms(),
        json!({"route": "/v1/metrics"}),
    ));
    writer.publish(record(
        "noise",
        now_ms() - 500,
        json!({"route": "/healthz"}),
    ));
    writer.flush().unwrap();
    writer.shutdown().unwrap();
    for arguments in [
        vec!["--json".to_owned()],
        vec!["--limit".to_owned(), "5".to_owned()],
        vec!["--level".to_owned(), "info,warn".to_owned()],
        vec!["--contains".to_owned(), "operational".to_owned()],
        vec!["--since".to_owned(), "1h".to_owned()],
        vec!["--archive".to_owned(), "--json".to_owned()],
        vec!["--help".to_owned()],
    ] {
        syncserver::logging::run(config.clone(), &arguments).unwrap();
    }
    for arguments in [
        vec!["--level".to_owned()],
        vec!["--limit".to_owned(), "0".to_owned()],
        vec!["--since".to_owned(), "yesterday".to_owned()],
        vec!["--unknown".to_owned()],
    ] {
        assert!(syncserver::logging::run(config.clone(), &arguments).is_err());
    }
}

#[test]
fn rendered_text_and_json_keep_message_fields_and_timestamp() {
    let record = record("rendered", 1_700_000_000_000, json!({"items": 2}));
    let text = syncserver::logging::render(&record, LogFormat::Text);
    assert!(text.contains("INFO syncserver::test rendered"));
    assert!(text.contains("items=2"));
    assert!(text.contains("2023-11-14T22:13:20"));
    let json: Value =
        serde_json::from_str(&syncserver::logging::render(&record, LogFormat::Json)).unwrap();
    assert_eq!(json["level"], "INFO");
    assert_eq!(json["message"], "rendered");
    assert_eq!(json["fields"]["items"], 2);
    assert!(
        json["timestamp"]
            .as_str()
            .unwrap()
            .starts_with("2023-11-14")
    );
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
