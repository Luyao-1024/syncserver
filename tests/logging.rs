mod common;

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
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
        database_max_bytes: 268_435_456,
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
        user_id: None,
    }
}

/// The same record, attributed to an account. Split out so that a test saying "this is
/// demo's traffic" has to name the account rather than reuse an unattributed helper.
fn owned(user_id: &str, message: &str, ts_ms: i64, fields: Value) -> LogRecord {
    LogRecord {
        user_id: Some(user_id.to_owned()),
        ..record(message, ts_ms, fields)
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

/// The console narrows a log search by request id and reaches past the raw window at
/// the same time: "what happened to this one call" is answered by records that were
/// compressed away hours ago. Neither filter is exercised on its own by the CLI
/// tests, and the console depends on the pair.
#[test]
fn a_request_can_be_traced_across_the_compressed_boundary() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Info, LogLevel::Info);
    settings.retention_raw_secs = 1;
    let writer = open(root.path(), &settings);
    let old = now_ms() - 60_000;

    for index in 0..12 {
        let mut record = record(
            &format!("archived record {index}"),
            old + index,
            json!({"index": index}),
        );
        record.request_id = Some(if index % 3 == 0 {
            REQUEST_ID.to_owned()
        } else {
            "someone-elses-request".to_owned()
        });
        writer.publish(record);
    }
    // The call the reader is chasing also has a live record, so the pinned search has
    // to reach across both stores rather than only whichever one is open.
    for (index, level) in ["INFO", "WARN", "ERROR"].into_iter().enumerate() {
        let mut record = record(&format!("live record {index}"), now_ms(), json!({}));
        record.level = level.to_owned();
        record.request_id = Some(REQUEST_ID.to_owned());
        writer.publish(record);
    }
    writer.flush().unwrap();
    writer.archive().unwrap();

    // Nothing reaches past the raw window without asking for the archive.
    let raw = query(
        &database(root.path()),
        &LogFilter {
            request_id: Some(REQUEST_ID.to_owned()),
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert_eq!(raw.len(), 3, "only the live records are uncompressed");

    let pinned = query(
        &database(root.path()),
        &LogFilter {
            request_id: Some(REQUEST_ID.to_owned()),
            include_archive: true,
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert_eq!(pinned.len(), 7, "4 archived plus 3 live, all one request");
    for stored in &pinned {
        assert_eq!(stored.record.request_id.as_deref(), Some(REQUEST_ID));
    }
    // Merged by (timestamp, sequence), so the reader sees one ordered story.
    assert_eq!(pinned[0].record.message, "archived record 0");
    assert_eq!(pinned[6].record.message, "live record 2");
    assert!(
        pinned
            .windows(2)
            .all(|pair| pair[0].record.ts_ms <= pair[1].record.ts_ms),
        "the pinned records stay in time order"
    );
    assert_eq!(pinned[5].record.level, "WARN");
    assert_eq!(pinned[6].record.level, "ERROR");

    // A request id nobody has is an empty result, not everything.
    let absent = query(
        &database(root.path()),
        &LogFilter {
            request_id: Some("never-issued".to_owned()),
            include_archive: true,
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert!(absent.is_empty());

    // The rest of the traffic is untouched by that filter.
    let others = query(
        &database(root.path()),
        &LogFilter {
            request_id: Some("someone-elses-request".to_owned()),
            include_archive: true,
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert_eq!(others.len(), 8);
    writer.shutdown().unwrap();
}

/// A database written by an older build is lifted in place, and everything it already
/// held is still readable afterwards. Log records are not sync truth, but they are the
/// only record of what a deployment did last week, so an upgrade that quietly drops
/// them would cost more than the column it adds.
#[test]
fn a_previous_version_database_is_migrated_and_keeps_every_record() {
    let root = tempfile::tempdir().unwrap();
    let state = state(root.path());
    fs::create_dir_all(&state).unwrap();
    fs::set_permissions(&state, fs::Permissions::from_mode(0o700)).unwrap();

    // Version 1: the schema as it was before accounts were attributed. The record is
    // stamped recently so it stays inside the retention window — a timestamp from 1970
    // would be swept into the archive on the first pass, which is correct behaviour but
    // would test the archive rather than the migration.
    let fresh = now_ms() - 1_000;
    let connection = rusqlite::Connection::open(database(root.path())).unwrap();
    connection.execute_batch(V1_SCHEMA).unwrap();
    connection
        .execute(
            "INSERT INTO log_events (ts_ms, level, target, message, fields, request_id)
             VALUES (?1, 'INFO', 'syncserver::test', 'before the upgrade', '{}', ?2)",
            rusqlite::params![fresh, "8f14e45f-ea8f-4b2c-9c3d-1a2b3c4d5e6f"],
        )
        .unwrap();
    connection
        .pragma_update(None, "user_version", 1_i64)
        .unwrap();
    drop(connection);
    // The writer refuses to open a log database that is not privately owned, and it is
    // right to: these records describe what each account did.
    fs::set_permissions(database(root.path()), fs::Permissions::from_mode(0o600)).unwrap();

    let settings = configure(LogLevel::Info, LogLevel::Info);
    let writer = open(root.path(), &settings);
    // A record written after the upgrade must land beside the old ones.
    writer.publish(record("after the upgrade", now_ms(), json!({})));
    writer.flush().unwrap();

    let connection = rusqlite::Connection::open(database(root.path())).unwrap();
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(version, 2, "the database is lifted to the current version");

    let columns = connection
        .prepare("PRAGMA table_info(log_events)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert!(
        columns.iter().any(|name| name == "user_id"),
        "the account column exists: {columns:?}"
    );

    let indexed = connection
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='index' AND name='log_events_user'",
            [],
            |row| row.get::<_, i64>(0),
        )
        .unwrap();
    assert_eq!(indexed, 1, "the account column is indexed");
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM log_events", [], |row| row.get(0))
        .unwrap();
    assert_eq!(
        count, 2,
        "the pre-upgrade record survives alongside the new one"
    );
    // Records written before the column existed read back as unattributed, not as a
    // failure: the account genuinely was not recorded at the time.
    let old_user: Option<String> = connection
        .query_row(
            "SELECT user_id FROM log_events WHERE message='before the upgrade'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(old_user, None);
    drop(connection);

    let rows = query(&database(root.path()), &LogFilter::default()).unwrap();
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|row| row.record.message == "before the upgrade")
    );
    writer.shutdown().unwrap();
}

/// The schema as it was at version 1, written out rather than imported so that the
/// test keeps testing the real migration even after the current schema moves on.
const V1_SCHEMA: &str = "
CREATE TABLE log_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE log_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    level TEXT NOT NULL CHECK (level IN ('ERROR','WARN','INFO','DEBUG','TRACE')),
    target TEXT NOT NULL,
    message TEXT NOT NULL,
    fields TEXT NOT NULL CHECK (json_valid(fields)),
    request_id TEXT
) STRICT;
CREATE INDEX log_events_ts ON log_events(ts_ms, seq);
CREATE TABLE log_segments (
    segment_id INTEGER PRIMARY KEY AUTOINCREMENT,
    first_seq INTEGER NOT NULL,
    last_seq INTEGER NOT NULL,
    first_ts_ms INTEGER NOT NULL,
    last_ts_ms INTEGER NOT NULL,
    row_count INTEGER NOT NULL CHECK (row_count > 0),
    raw_bytes INTEGER NOT NULL,
    stored_bytes INTEGER NOT NULL,
    codec TEXT NOT NULL CHECK (codec = 'gzip'),
    checksum TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    payload BLOB NOT NULL
) STRICT;
CREATE INDEX log_segments_time ON log_segments(first_ts_ms, segment_id);
";

/// Two accounts, and a search that has to reach past the compression boundary to find
/// one of them. An operator asking "what did this person do last week" lands exactly on
/// that boundary, so a filter that only covers the live window is useless there.
#[test]
fn an_account_filter_spans_the_compressed_boundary() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Info, LogLevel::Info);
    settings.retention_raw_secs = 1;
    let writer = open(root.path(), &settings);
    let old = now_ms() - 60_000;
    const DEMO: &str = "c094d2a8-dfc0-418b-ab28-76f658cb7a7d";
    const CAROL: &str = "9aac88e5-955f-4c51-8c30-0c332b13716d";

    // Six archived records, split between the two accounts.
    for index in 0..6 {
        let account = if index % 2 == 0 { DEMO } else { CAROL };
        writer.publish(owned(
            account,
            &format!("archived record {index}"),
            old + index,
            json!({"index": index}),
        ));
    }
    // And two live ones, so the same search has to reconcile both stores.
    writer.publish(owned(DEMO, "live record", now_ms(), json!({})));
    writer.publish(owned(CAROL, "live record", now_ms(), json!({})));
    writer.flush().unwrap();
    writer.archive().unwrap();

    let stats = writer.stats();
    assert_eq!(stats.raw_rows, 2, "only the live records stay uncompressed");
    assert_eq!(stats.archived_rows, 6);

    let demo = query(
        &database(root.path()),
        &LogFilter {
            user_id: Some(DEMO.to_owned()),
            include_archive: true,
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert_eq!(demo.len(), 4, "3 archived plus 1 live, all demo's");
    for stored in &demo {
        assert_eq!(stored.record.user_id.as_deref(), Some(DEMO));
    }
    // Ordered by (timestamp, sequence), so the reader sees one continuous story rather
    // than an archive pile and a live pile.
    assert_eq!(demo[0].record.message, "archived record 0");
    assert_eq!(demo[3].record.message, "live record");
    assert!(
        demo.windows(2)
            .all(|pair| pair[0].record.ts_ms <= pair[1].record.ts_ms),
        "the account's records stay in time order"
    );

    // The other account is untouched, and the two never bleed into each other.
    let carol = query(
        &database(root.path()),
        &LogFilter {
            user_id: Some(CAROL.to_owned()),
            include_archive: true,
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert_eq!(carol.len(), 4);
    assert!(
        carol
            .iter()
            .all(|row| row.record.user_id.as_deref() == Some(CAROL))
    );

    // An account nobody has is empty, not everything.
    let absent = query(
        &database(root.path()),
        &LogFilter {
            user_id: Some("00000000-0000-4000-8000-000000000000".to_owned()),
            include_archive: true,
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert!(absent.is_empty());

    // Without the archive the same search finds only the live record, which is the
    // difference the flag makes.
    let live_only = query(
        &database(root.path()),
        &LogFilter {
            user_id: Some(DEMO.to_owned()),
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert_eq!(live_only.len(), 1);

    // Records published before attribution existed carry no account and are not
    // claimed by any account's search.
    writer.publish(record("unattributed", now_ms(), json!({})));
    writer.flush().unwrap();
    let everyone = query(
        &database(root.path()),
        &LogFilter {
            user_id: Some(DEMO.to_owned()),
            limit: 100,
            ..LogFilter::default()
        },
    )
    .unwrap();
    assert!(
        everyone
            .iter()
            .all(|row| row.record.message != "unattributed"),
        "a record with no account does not belong to anyone's search"
    );
    writer.shutdown().unwrap();
}

/// The account id is an operator handle, not library content: it has to survive the
/// production default of `SYNC_LOG_PAYLOAD=off`, while everything that could name a
/// photo still has to be suppressed.
#[test]
fn an_account_id_survives_without_detail_logging_but_a_record_name_does_not() {
    let root = tempfile::tempdir().unwrap();
    let settings = configure(LogLevel::Info, LogLevel::Info);
    assert!(!settings.payload_detail, "the production default");
    let writer = open(root.path(), &settings);
    capture(&writer, &settings, || {
        tracing::info!(
            user_id = "c094d2a8-dfc0-418b-ab28-76f658cb7a7d",
            asset_id = "aef93656-b6c9-40c5-b991-0e8609d48aaa",
            record_name = "IMG_0001",
            relative_path = "Album/IMG_0001.heic",
            "asset verified and published"
        );
    });
    writer.flush().unwrap();
    let rows = query(&database(root.path()), &LogFilter::default()).unwrap();
    let record = &rows[0].record;
    assert_eq!(
        record.user_id.as_deref(),
        Some("c094d2a8-dfc0-418b-ab28-76f658cb7a7d"),
        "the account is searchable without turning on detail logging"
    );
    assert_eq!(
        record.fields["asset_id"],
        json!("aef93656-b6c9-40c5-b991-0e8609d48aaa"),
        "the object id is an operator handle too"
    );
    for detail in ["record_name", "relative_path"] {
        assert_eq!(
            record.fields[detail].as_str(),
            Some("[detail-off]"),
            "{detail} names a photo and stays suppressed"
        );
    }
    writer.shutdown().unwrap();
}

#[test]
fn log_database_has_an_emergency_storage_cap_without_trimming_recent_rows() {
    let root = tempfile::tempdir().unwrap();
    let mut settings = configure(LogLevel::Info, LogLevel::Info);
    settings.database_max_bytes = 1_048_576;
    let writer = open(root.path(), &settings);
    writer.publish(record("retained-first-record", now_ms(), json!({})));
    writer.flush().unwrap();
    for batch in 0..16 {
        for index in 0..100 {
            writer.publish(record(
                &format!("entry-{batch}-{index}"),
                now_ms(),
                json!({"text":"x".repeat(4096)}),
            ));
        }
        let _ = writer.flush();
        if writer.stats().write_failures > 0 {
            break;
        }
    }
    assert!(
        writer.stats().write_failures > 0,
        "the storage cap must refuse further growth"
    );
    let c = rusqlite::Connection::open(database(root.path())).unwrap();
    let retained: i64 = c
        .query_row(
            "SELECT count(*) FROM log_events WHERE message='retained-first-record'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(retained, 1);
    let pages: i64 = c
        .pragma_query_value(None, "page_count", |r| r.get(0))
        .unwrap();
    let size: i64 = c
        .pragma_query_value(None, "page_size", |r| r.get(0))
        .unwrap();
    assert!(pages * size <= 1_048_576);
    drop(c);
    let _ = writer.shutdown();
}
