//! Bounded log queue and the private SQLite log database with its archive pass.

use super::record::now_ms;
use super::{archive, record::LogRecord};
use crate::{Error, Result, config::LogSettings, logging::record::StoredRecord, store::files};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicI64, AtomicU64, AtomicUsize, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

/// Log storage sits beside the metadata database in its own file: operational logging
/// never joins sync transactions, and the metadata schema stays at its current version.
pub const LOG_DATABASE: &str = "logs.sqlite";
const SCHEMA_VERSION: i64 = 2;
/// Records commit in batches, so a short interval bounds how long a record stays buffered.
const FLUSH_INTERVAL: Duration = Duration::from_millis(1000);
const FLUSH_MAX_ROWS: usize = 512;
/// A short interval keeps the compressed window close to the configured threshold
/// without making every pass expensive: each pass only seeks the index.
const ARCHIVE_INTERVAL: Duration = Duration::from_secs(5);
const ARCHIVE_BATCH_ROWS: usize = 4096;
const QUEUE_WAIT: Duration = Duration::from_secs(10);
const SCHEMA: &str = "
CREATE TABLE log_meta (key TEXT PRIMARY KEY, value TEXT NOT NULL) STRICT;
CREATE TABLE log_events (
    seq INTEGER PRIMARY KEY AUTOINCREMENT,
    ts_ms INTEGER NOT NULL,
    level TEXT NOT NULL CHECK (level IN ('ERROR','WARN','INFO','DEBUG','TRACE')),
    target TEXT NOT NULL,
    message TEXT NOT NULL,
    fields TEXT NOT NULL CHECK (json_valid(fields)),
    request_id TEXT,
    user_id TEXT
) STRICT;
CREATE INDEX log_events_ts ON log_events(ts_ms, seq);
CREATE INDEX log_events_level ON log_events(level, seq);
CREATE INDEX log_events_request ON log_events(request_id, seq);
CREATE INDEX log_events_user ON log_events(user_id, seq);
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

/// Lifts a database written at version 1 to the current one.
///
/// Added in a single transaction because SQLite's DDL is transactional: either the
/// column and its index both land or neither does, so a database is never left at a
/// version that the open path would reject. `ADD COLUMN` rewrites no rows, so this is
/// the same cost against a 30,000-record window as against an empty one.
const MIGRATE_1_TO_2: &str = "
ALTER TABLE log_events ADD COLUMN user_id TEXT;
CREATE INDEX log_events_user ON log_events(user_id, seq);
";

type Reply = mpsc::Sender<Result<()>>;

enum Command {
    Record(LogRecord),
    Activate(PathBuf, Reply),
    Flush(Reply),
    Archive(Reply),
    Stop(Reply),
}

#[derive(Debug, Default)]
struct Counters {
    persisted: AtomicU64,
    dropped_queue_full: AtomicU64,
    dropped_pending: AtomicU64,
    write_failures: AtomicU64,
    queued: AtomicUsize,
    activated: AtomicBool,
    raw_rows: AtomicU64,
    oldest_raw_ts_ms: AtomicI64,
    archived_rows: AtomicU64,
    archive_segments: AtomicU64,
    archive_bytes: AtomicU64,
    trimmed_segments: AtomicU64,
    last_archive_ts_ms: AtomicI64,
}

/// Counters readable from the metrics handler without touching the log database.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogStats {
    pub persist_level: String,
    pub activated: bool,
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub persisted: u64,
    pub dropped_queue_full: u64,
    pub dropped_pending: u64,
    pub write_failures: u64,
    pub raw_rows: u64,
    pub oldest_raw_ts_ms: i64,
    pub archived_rows: u64,
    pub archive_segments: u64,
    pub archive_bytes: u64,
    pub archive_max_bytes: u64,
    pub database_max_bytes: u64,
    pub trimmed_segments: u64,
    pub last_archive_ts_ms: i64,
}

/// Sending half of the log pipeline. Publishing never blocks the caller.
pub struct Writer {
    sender: SyncSender<Command>,
    counters: Arc<Counters>,
    settings: LogSettings,
    worker: Mutex<Option<JoinHandle<()>>>,
}

impl Writer {
    /// Starts the writer thread. Storage is buffered in memory until [`Writer::activate`]
    /// opens the database, which only happens once the data directory lock is ours.
    pub fn spawn(settings: &LogSettings) -> Result<Self> {
        let (sender, receiver) = mpsc::sync_channel(settings.queue_capacity.max(1));
        let counters = Arc::new(Counters::default());
        let thread_counters = Arc::clone(&counters);
        let thread_settings = settings.clone();
        let worker = thread::Builder::new()
            .name("syncserver-log-writer".into())
            .spawn(move || {
                if let Err(error) = worker(receiver, thread_settings, thread_counters) {
                    // Only the sanitized kind: no path, SQL or credential text.
                    eprintln!("syncserver: log writer stopped: {}", error.kind_name());
                }
            })
            .map_err(|_| Error::LogStorage)?;
        Ok(Self {
            sender,
            counters,
            settings: settings.clone(),
            worker: Mutex::new(Some(worker)),
        })
    }

    pub fn publish(&self, record: LogRecord) {
        self.counters.queued.fetch_add(1, Ordering::Relaxed);
        let Err(mpsc::TrySendError::Full(record)) = self.sender.try_send(Command::Record(record))
        else {
            return;
        };
        self.counters.queued.fetch_sub(1, Ordering::Relaxed);
        let dropped = self
            .counters
            .dropped_queue_full
            .fetch_add(1, Ordering::Relaxed)
            + 1;
        drop(record);
        if dropped == 1 || dropped.is_multiple_of(1024) {
            tracing::warn!(
                dropped_logs = dropped,
                log_queue_capacity = self.settings.queue_capacity,
                "log queue is full and records were discarded"
            );
        }
    }

    /// Opens `<data_dir>/logs.sqlite` and buffers whatever was accepted beforehand.
    pub fn activate(&self, data_dir: &Path) -> Result<()> {
        let path = data_dir.join(LOG_DATABASE);
        self.request(|reply| Command::Activate(path, reply))
    }

    /// Commits buffered records and refreshes the stored gauges.
    pub fn flush(&self) -> Result<()> {
        self.request(Command::Flush)
    }

    /// Runs one archive pass now instead of waiting for the interval.
    pub fn archive(&self) -> Result<()> {
        self.request(Command::Archive)
    }

    pub fn stats(&self) -> LogStats {
        LogStats {
            persist_level: self.settings.persist_level.as_str().to_owned(),
            activated: self.counters.activated.load(Ordering::Relaxed),
            queue_depth: self.counters.queued.load(Ordering::Relaxed),
            queue_capacity: self.settings.queue_capacity,
            persisted: self.counters.persisted.load(Ordering::Relaxed),
            dropped_queue_full: self.counters.dropped_queue_full.load(Ordering::Relaxed),
            dropped_pending: self.counters.dropped_pending.load(Ordering::Relaxed),
            write_failures: self.counters.write_failures.load(Ordering::Relaxed),
            raw_rows: self.counters.raw_rows.load(Ordering::Relaxed),
            oldest_raw_ts_ms: self.counters.oldest_raw_ts_ms.load(Ordering::Relaxed),
            archived_rows: self.counters.archived_rows.load(Ordering::Relaxed),
            archive_segments: self.counters.archive_segments.load(Ordering::Relaxed),
            archive_bytes: self.counters.archive_bytes.load(Ordering::Relaxed),
            archive_max_bytes: self.settings.archive_max_bytes,
            database_max_bytes: self.settings.database_max_bytes,
            trimmed_segments: self.counters.trimmed_segments.load(Ordering::Relaxed),
            last_archive_ts_ms: self.counters.last_archive_ts_ms.load(Ordering::Relaxed),
        }
    }

    fn request(&self, build: impl FnOnce(Reply) -> Command) -> Result<()> {
        let (reply, response) = mpsc::channel();
        self.sender
            .send(build(reply))
            .map_err(|_| Error::LogStorage)?;
        response
            .recv_timeout(QUEUE_WAIT)
            .map_err(|_| Error::LogStorage)?
    }

    /// Flushes pending records, checkpoints the write-ahead log and joins the writer.
    pub fn shutdown(&self) -> Result<()> {
        let handle = self.worker.lock().map_err(|_| Error::LogStorage)?.take();
        let Some(handle) = handle else { return Ok(()) };
        let (reply, response) = mpsc::channel();
        if self.sender.send(Command::Stop(reply)).is_err() {
            let _ = handle.join();
            return Ok(());
        }
        let result = response
            .recv_timeout(QUEUE_WAIT)
            .map_err(|_| Error::LogStorage);
        let _ = handle.join();
        result?
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // A dropped handle still flushes whatever it had already accepted.
        let _ = self.flush();
        let _ = self.shutdown();
    }
}

fn worker(
    receiver: Receiver<Command>,
    settings: LogSettings,
    counters: Arc<Counters>,
) -> Result<()> {
    let mut session: Option<Session> = None;
    // Records emitted before activation, for example while loading credentials, are held
    // in a bounded queue so nothing already accepted is silently lost.
    let mut pending: VecDeque<LogRecord> = VecDeque::new();
    let mut batch: Vec<LogRecord> = Vec::new();
    let mut batch_started: Option<Instant> = None;
    let mut next_archive = Instant::now();
    let mut running = true;
    while running {
        match receiver.recv_timeout(remaining_wait(batch_started, next_archive)) {
            Ok(Command::Record(record)) => {
                counters.queued.fetch_sub(1, Ordering::Relaxed);
                if session.is_some() {
                    if batch.len() >= FLUSH_MAX_ROWS {
                        commit(&mut session, &mut batch, &counters);
                    }
                    batch_started.get_or_insert_with(Instant::now);
                    batch.push(record);
                } else {
                    if pending.len() >= settings.queue_capacity {
                        pending.pop_front();
                        counters.dropped_pending.fetch_add(1, Ordering::Relaxed);
                    }
                    pending.push_back(record);
                }
            }
            Ok(Command::Activate(path, reply)) => {
                let opened = Session::open(&path, &settings);
                match opened {
                    Ok(opened) => {
                        session = Some(opened);
                        counters.activated.store(true, Ordering::Relaxed);
                        batch.extend(pending.drain(..));
                        batch_started.get_or_insert_with(Instant::now);
                        if let Err(error) = session
                            .as_ref()
                            .map(|opened| opened.initialize(&counters))
                            .unwrap_or(Ok(()))
                        {
                            note_failure(&counters, &error);
                        }
                        tracing::info!(
                            log_retention_raw_secs = settings.retention_raw_secs,
                            log_archive_max_bytes = settings.archive_max_bytes,
                            log_persist_level = settings.persist_level.as_str(),
                            "log database ready"
                        );
                        let _ = reply.send(Ok(()));
                    }
                    Err(error) => {
                        note_failure(&counters, &error);
                        let _ = reply.send(Err(error));
                    }
                }
            }
            Ok(Command::Flush(reply)) => {
                commit(&mut session, &mut batch, &counters);
                batch_started = None;
                let _ = reply.send(Ok(()));
            }
            Ok(Command::Archive(reply)) => {
                let result = match &mut session {
                    Some(opened) => opened.archive(&settings, &counters),
                    None => Ok(()),
                };
                let _ = reply.send(result);
            }
            Ok(Command::Stop(reply)) => {
                commit(&mut session, &mut batch, &counters);
                let result = match &mut session {
                    Some(opened) => opened.close(),
                    None => Ok(()),
                };
                let _ = reply.send(result);
                running = false;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                commit(&mut session, &mut batch, &counters);
                if let Some(opened) = &mut session {
                    let _ = opened.close();
                }
                running = false;
            }
        }
        if session.is_some()
            && batch_started.is_some_and(|started| {
                batch.len() >= FLUSH_MAX_ROWS || started.elapsed() >= FLUSH_INTERVAL
            })
        {
            commit(&mut session, &mut batch, &counters);
            batch_started = None;
        }
        if Instant::now() >= next_archive {
            if let Some(opened) = &mut session
                && let Err(error) = opened.archive(&settings, &counters)
            {
                note_failure(&counters, &error);
            }
            next_archive = Instant::now() + ARCHIVE_INTERVAL;
        }
    }
    Ok(())
}

fn remaining_wait(batch_started: Option<Instant>, next_archive: Instant) -> Duration {
    let flush = batch_started
        .map(|started| FLUSH_INTERVAL.saturating_sub(started.elapsed()))
        .unwrap_or(ARCHIVE_INTERVAL);
    let archive = next_archive.saturating_duration_since(Instant::now());
    flush.min(archive).max(Duration::from_millis(10))
}

fn commit(session: &mut Option<Session>, batch: &mut Vec<LogRecord>, counters: &Arc<Counters>) {
    if batch.is_empty() {
        return;
    }
    let Some(opened) = session else { return };
    match opened.commit(batch) {
        Ok(rows) => {
            counters.persisted.fetch_add(rows as u64, Ordering::Relaxed);
            counters.raw_rows.fetch_add(rows as u64, Ordering::Relaxed);
            batch.clear();
        }
        Err(error) => {
            note_failure(counters, &error);
            // A failing database must not grow the buffer without bound: keep the newest
            // half and let the next attempt retry them.
            let keep = batch.len() / 2;
            let skip = batch.len() - keep;
            batch.drain(0..skip);
        }
    }
}

fn note_failure(counters: &Arc<Counters>, error: &Error) {
    let failures = counters.write_failures.fetch_add(1, Ordering::Relaxed) + 1;
    if failures == 1 || failures.is_multiple_of(64) {
        tracing::error!(
            log_write_failures = failures,
            error = error.kind_name(),
            "log database operation failed"
        );
    }
}

struct Session {
    connection: Connection,
}

impl Session {
    fn open(path: &Path, settings: &LogSettings) -> Result<Self> {
        let root = path.parent().ok_or(Error::LogStorage)?;
        files::directory(root)?;
        drop(files::open_private(path, true)?);
        let stem = path.file_name().ok_or(Error::LogStorage)?.to_string_lossy();
        for suffix in ["-wal", "-shm", "-journal"] {
            let sibling = root.join(format!("{stem}{suffix}"));
            if fs::symlink_metadata(&sibling).is_ok() {
                files::open_private(&sibling, false)?;
            }
        }
        let connection = Connection::open(path)?;
        connection.pragma_update(None, "journal_mode", "WAL")?;
        // Log records are operational data, not sync truth: a bounded loss window before
        // the next commit is acceptable, a structurally invalid database is not.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
        if version > SCHEMA_VERSION {
            return Err(Error::SchemaUnsupported);
        }
        if version == 0 {
            connection.execute_batch(SCHEMA)?;
            store_setting(&connection, "retention_raw_secs", settings)?;
            store_setting(&connection, "archive_max_bytes", settings)?;
            connection.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else {
            let tables: i64 = connection.query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table'
                 AND name IN ('log_events','log_segments','log_meta')",
                [],
                |row| row.get(0),
            )?;
            if tables != 3 {
                return Err(Error::SchemaUnsupported);
            }
            if version < SCHEMA_VERSION {
                migrate(&connection, version)?;
            }
            store_setting(&connection, "retention_raw_secs", settings)?;
            store_setting(&connection, "archive_max_bytes", settings)?;
        }
        files::cap_database(&connection, settings.database_max_bytes)?;
        Ok(Self { connection })
    }

    fn commit(&mut self, batch: &mut [LogRecord]) -> Result<usize> {
        let tx = self
            .connection
            .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut rows = 0;
        {
            let mut statement = tx.prepare(
                "INSERT INTO log_events (ts_ms, level, target, message, fields, request_id, user_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )?;
            for record in batch.iter() {
                let fields =
                    serde_json::to_string(&record.fields).map_err(|_| Error::LogStorage)?;
                rows += statement.execute(rusqlite::params![
                    record.ts_ms,
                    record.level,
                    record.target,
                    record.message,
                    fields,
                    record.request_id,
                    record.user_id
                ])?;
            }
        }
        tx.commit()?;
        Ok(rows)
    }

    /// Compresses every record older than the raw retention threshold into gzip segments.
    fn archive(&mut self, settings: &LogSettings, counters: &Arc<Counters>) -> Result<()> {
        let cutoff = now_ms() - settings.retention_raw_secs as i64 * 1000;
        let mut archived = 0_u64;
        loop {
            let rows = self.older_than(cutoff)?;
            if rows.is_empty() {
                break;
            }
            let (Some(first), Some(last)) = (rows.first(), rows.last()) else {
                break;
            };
            let segment = archive::compress(&rows)?;
            let tx = self
                .connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "INSERT INTO log_segments (first_seq, last_seq, first_ts_ms, last_ts_ms, row_count,
                     raw_bytes, stored_bytes, codec, checksum, created_at_ms, payload)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'gzip', ?8, ?9, ?10)",
                params![
                    first.seq,
                    last.seq,
                    first.record.ts_ms,
                    last.record.ts_ms,
                    rows.len() as i64,
                    segment.raw_bytes as i64,
                    segment.stored_bytes as i64,
                    segment.checksum,
                    now_ms(),
                    segment.payload
                ],
            )?;
            let deleted = tx.execute(
                "DELETE FROM log_events WHERE seq <= ?1 AND ts_ms < ?2",
                params![last.seq, cutoff],
            )?;
            tx.commit()?;
            archived += deleted as u64;
            counters
                .archived_rows
                .fetch_add(deleted as u64, Ordering::Relaxed);
            counters
                .raw_rows
                .fetch_sub(deleted as u64, Ordering::Relaxed);
            counters.archive_segments.fetch_add(1, Ordering::Relaxed);
            counters
                .archive_bytes
                .fetch_add(segment.stored_bytes, Ordering::Relaxed);
            if rows.len() < ARCHIVE_BATCH_ROWS {
                break;
            }
        }
        self.enforce_cap(settings, counters)?;
        self.note_oldest(counters)?;
        counters
            .last_archive_ts_ms
            .store(now_ms(), Ordering::Relaxed);
        if archived > 0 {
            tracing::debug!(
                archived_rows = archived,
                archive_segments = counters.archive_segments.load(Ordering::Relaxed),
                archive_bytes = counters.archive_bytes.load(Ordering::Relaxed),
                "records older than the raw retention threshold were compressed"
            );
        }
        Ok(())
    }

    /// The compressed archive has a hard cap; the current raw window deliberately has none.
    fn enforce_cap(&mut self, settings: &LogSettings, counters: &Arc<Counters>) -> Result<()> {
        let mut trimmed = 0;
        loop {
            let stored: i64 = self.connection.query_row(
                "SELECT COALESCE(SUM(stored_bytes),0) FROM log_segments",
                [],
                |row| row.get(0),
            )?;
            if u64::try_from(stored).unwrap_or(u64::MAX) <= settings.archive_max_bytes {
                break;
            }
            let oldest = self
                .connection
                .query_row(
                    "SELECT segment_id FROM log_segments ORDER BY first_ts_ms, segment_id LIMIT 1",
                    [],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?;
            let Some(segment_id) = oldest else { break };
            let freed: i64 = self.connection.query_row(
                "SELECT stored_bytes FROM log_segments WHERE segment_id=?1",
                params![segment_id],
                |row| row.get(0),
            )?;
            let tx = self
                .connection
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            tx.execute(
                "DELETE FROM log_segments WHERE segment_id=?1",
                params![segment_id],
            )?;
            tx.commit()?;
            counters
                .archive_bytes
                .fetch_sub(freed.max(0) as u64, Ordering::Relaxed);
            counters.archive_segments.fetch_sub(1, Ordering::Relaxed);
            trimmed += 1;
        }
        if trimmed > 0 {
            counters
                .trimmed_segments
                .fetch_add(trimmed, Ordering::Relaxed);
            tracing::warn!(
                trimmed_segments = trimmed,
                log_archive_max_bytes = settings.archive_max_bytes,
                "compressed log archive exceeded its cap and the oldest segments were discarded"
            );
        }
        Ok(())
    }

    fn older_than(&self, cutoff: i64) -> Result<Vec<StoredRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT seq, ts_ms, level, target, message, fields, request_id, user_id FROM log_events
             WHERE ts_ms < ?1 ORDER BY seq LIMIT ?2",
        )?;
        let rows = statement.query_map(params![cutoff, ARCHIVE_BATCH_ROWS as i64], read_row)?;
        let mut collected = Vec::new();
        for row in rows {
            collected.push(row?);
        }
        Ok(collected)
    }

    /// Gauges become deltas after this first read, so steady-state passes never scan.
    fn initialize(&self, counters: &Arc<Counters>) -> Result<()> {
        let rows: i64 =
            self.connection
                .query_row("SELECT COUNT(*) FROM log_events", [], |row| row.get(0))?;
        counters
            .raw_rows
            .store(rows.max(0) as u64, Ordering::Relaxed);
        let (segments, bytes): (i64, i64) = self.connection.query_row(
            "SELECT COUNT(*), COALESCE(SUM(stored_bytes),0) FROM log_segments",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        counters
            .archive_segments
            .store(segments.max(0) as u64, Ordering::Relaxed);
        counters
            .archive_bytes
            .store(bytes.max(0) as u64, Ordering::Relaxed);
        self.note_oldest(counters)
    }

    /// Index seek for the oldest uncompressed record instead of an aggregate scan.
    fn note_oldest(&self, counters: &Arc<Counters>) -> Result<()> {
        let oldest: Option<i64> = self
            .connection
            .query_row(
                "SELECT ts_ms FROM log_events ORDER BY ts_ms LIMIT 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        counters
            .oldest_raw_ts_ms
            .store(oldest.unwrap_or_default(), Ordering::Relaxed);
        Ok(())
    }

    fn close(&mut self) -> Result<()> {
        self.connection
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
        Ok(())
    }
}

/// Brings an older log database up to [`SCHEMA_VERSION`] in one transaction.
///
/// Steps are applied in order and only those the database has not yet seen, so an
/// interrupted upgrade resumes where it stopped rather than failing on a column that
/// is already there.
fn migrate(connection: &Connection, from: i64) -> Result<()> {
    if from >= SCHEMA_VERSION {
        return Ok(());
    }
    let tx = connection.unchecked_transaction()?;
    if from < 2 {
        tx.execute_batch(MIGRATE_1_TO_2)?;
    }
    tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
    tx.commit()?;
    Ok(())
}

fn store_setting(connection: &Connection, key: &str, settings: &LogSettings) -> Result<()> {
    let value = match key {
        "retention_raw_secs" => settings.retention_raw_secs.to_string(),
        _ => settings.archive_max_bytes.to_string(),
    };
    connection.execute(
        "INSERT INTO log_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        params![key, value],
    )?;
    Ok(())
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRecord> {
    let fields: String = row.get(5)?;
    Ok(StoredRecord {
        seq: row.get(0)?,
        record: LogRecord {
            ts_ms: row.get(1)?,
            level: row.get(2)?,
            target: row.get(3)?,
            message: row.get(4)?,
            fields: serde_json::from_str(&fields).unwrap_or_default(),
            request_id: row.get(6)?,
            user_id: row.get(7)?,
        },
    })
}

/// Read filter over the raw window and, optionally, the compressed archive.
#[derive(Clone, Debug, Default)]
pub struct LogFilter {
    /// Uppercase severities; empty means every level.
    pub levels: Vec<String>,
    pub since_ms: Option<i64>,
    pub until_ms: Option<i64>,
    pub contains: Option<String>,
    pub request_id: Option<String>,
    /// The account a record belongs to. Like `request_id` this is exact, not a
    /// substring: an operator asks about one account, not about text that looks like
    /// one.
    pub user_id: Option<String>,
    pub limit: usize,
    /// Include gzip segments as well as the uncompressed window.
    pub include_archive: bool,
}

impl LogFilter {
    fn matches(&self, row: &StoredRecord) -> bool {
        let ts = row.record.ts_ms;
        if self.since_ms.is_some_and(|since| ts < since)
            || self.until_ms.is_some_and(|until| ts > until)
        {
            return false;
        }
        if !self.levels.is_empty() && !self.levels.contains(&row.record.level) {
            return false;
        }
        if self
            .request_id
            .as_ref()
            .is_some_and(|want| Some(want) != row.record.request_id.as_ref())
        {
            return false;
        }
        // Archive rows never reach the SQL above, so without this a search by account
        // would stop dead at the compression boundary — exactly where an operator
        // looking for last week's activity lands.
        if self
            .user_id
            .as_ref()
            .is_some_and(|want| Some(want) != row.record.user_id.as_ref())
        {
            return false;
        }
        if let Some(needle) = &self.contains {
            let haystack = format!(
                "{} {}",
                row.record.message,
                serde_json::to_string(&row.record.fields).unwrap_or_default()
            )
            .to_ascii_lowercase();
            if !haystack.contains(&needle.to_ascii_lowercase()) {
                return false;
            }
        }
        true
    }
}

fn escape_like(needle: &str) -> String {
    let mut out = String::with_capacity(needle.len() + 4);
    for character in needle.chars() {
        if matches!(character, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(character);
    }
    out
}

/// Reads the log database without joining any writer. This needs read access to the
/// service-private data directory, so it stays an operator tool rather than an HTTP surface.
pub fn query(path: &Path, filter: &LogFilter) -> Result<Vec<StoredRecord>> {
    let connection = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let limit = if filter.limit == 0 { 500 } else { filter.limit };
    let mut sql = String::from(
        "SELECT seq, ts_ms, level, target, message, fields, request_id, user_id FROM log_events WHERE 1",
    );
    let mut arguments: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::new();
    if let Some(since) = filter.since_ms {
        sql.push_str(" AND ts_ms>=?");
        arguments.push(Box::new(since));
    }
    if let Some(until) = filter.until_ms {
        sql.push_str(" AND ts_ms<=?");
        arguments.push(Box::new(until));
    }
    if !filter.levels.is_empty() {
        sql.push_str(&format!(
            " AND level IN ({})",
            vec!["?"; filter.levels.len()].join(",")
        ));
        for level in &filter.levels {
            arguments.push(Box::new(level.clone()));
        }
    }
    if let Some(request_id) = &filter.request_id {
        sql.push_str(" AND request_id=?");
        arguments.push(Box::new(request_id.clone()));
    }
    if let Some(user_id) = &filter.user_id {
        sql.push_str(" AND user_id=?");
        arguments.push(Box::new(user_id.clone()));
    }
    if let Some(needle) = &filter.contains {
        sql.push_str(" AND (message LIKE '%' || ? || '%' ESCAPE '\\' OR fields LIKE '%' || ? || '%' ESCAPE '\\')");
        let pattern = escape_like(needle);
        arguments.push(Box::new(pattern.clone()));
        arguments.push(Box::new(pattern));
    }
    sql.push_str(" ORDER BY seq DESC LIMIT ?");
    arguments.push(Box::new(limit as i64));
    let bounds: Vec<&dyn rusqlite::types::ToSql> =
        arguments.iter().map(|value| value.as_ref()).collect();
    let mut rows = {
        let mut statement = connection.prepare(&sql)?;
        statement
            .query_map(bounds.as_slice(), read_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    rows.reverse();
    if filter.include_archive {
        rows.extend(query_segments(&connection, filter, limit)?);
        rows.sort_by_key(|row| (row.record.ts_ms, row.seq));
        if rows.len() > limit {
            let skip = rows.len() - limit;
            rows.drain(0..skip);
        }
    }
    Ok(rows)
}

fn query_segments(
    connection: &Connection,
    filter: &LogFilter,
    limit: usize,
) -> Result<Vec<StoredRecord>> {
    let since = filter.since_ms.unwrap_or(i64::MIN);
    let until = filter.until_ms.unwrap_or(i64::MAX);
    let mut statement = connection.prepare(
        "SELECT checksum, payload FROM log_segments
         WHERE last_ts_ms >= ?1 AND first_ts_ms <= ?2
         ORDER BY segment_id DESC LIMIT ?3",
    )?;
    let segments = statement
        .query_map(params![since, until, (limit.max(1) * 4) as i64], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    let mut rows = Vec::new();
    for (checksum, payload) in segments {
        // A segment that cannot be decoded is reported instead of silently skipped.
        rows.extend(archive::decompress(&payload, &checksum)?);
    }
    Ok(rows.into_iter().filter(|row| filter.matches(row)).collect())
}
