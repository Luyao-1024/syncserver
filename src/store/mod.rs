pub(crate) mod files;
mod migrations;
mod records;
pub use records::Outcome;

use crate::{
    Config, Error, Result,
    jobs::{self, RecoveryReport},
};
use files::DataDirectory;
use rusqlite::Connection;
use serde::Serialize;
use std::{
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
};
use tokio::sync::{mpsc, oneshot};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub schema_version: i64,
    pub database_generation: Uuid,
    pub recovery: RecoveryReport,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueMetrics {
    pub queue_depth: usize,
    pub queue_capacity: usize,
    pub queue_rejections: u64,
    pub commands_completed: u64,
}

enum Command {
    Ping(oneshot::Sender<Result<()>>),
    Modify(
        crate::domain::records::Modify,
        oneshot::Sender<Result<Outcome>>,
    ),
    Lookup(
        crate::domain::records::Lookup,
        oneshot::Sender<Result<Outcome>>,
    ),
    #[cfg(test)]
    Hold(oneshot::Sender<()>, std::sync::mpsc::Receiver<()>),
}
struct Inner {
    // Taking the only sender atomically closes admission; queued commands drain.
    sender: Mutex<Option<mpsc::Sender<Command>>>,
    worker: Mutex<Option<thread::JoinHandle<Result<()>>>>,
    alive: AtomicBool,
    rejections: AtomicU64,
    completed: AtomicU64,
    capacity: usize,
    shutdown: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
    metadata: Arc<Metadata>,
}

impl Store {
    pub(crate) async fn open(config: Config, fingerprint: [u8; 32]) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel(config.limits.db_queue_capacity);
        let inner = Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            worker: Mutex::new(None),
            alive: AtomicBool::new(false),
            rejections: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            capacity: config.limits.db_queue_capacity,
            shutdown: tokio::sync::Mutex::new(()),
        });
        let worker_inner = Arc::downgrade(&inner);
        let (startup, initialized) = oneshot::channel();
        let worker = thread::Builder::new()
            .name("syncserver-database".into())
            .spawn(move || {
                let setup = (|| -> Result<_> {
                    let directory = DataDirectory::acquire(&config.data_dir)?;
                    let mut connection = Connection::open(&directory.database)?;
                    let generation =
                        migrations::initialize(&mut connection, &config, &fingerprint)?;
                    let recovery = jobs::recover(&mut connection)?;
                    Ok((
                        directory,
                        connection,
                        Metadata {
                            schema_version: migrations::SCHEMA_VERSION,
                            database_generation: generation,
                            recovery,
                        },
                    ))
                })();
                let (_directory, mut connection, metadata) = match setup {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = startup.send(Err(error));
                        return Ok(());
                    }
                };
                let run_id = metadata.recovery.run_id;
                if let Some(inner) = worker_inner.upgrade() {
                    inner.alive.store(true, Ordering::Release);
                }
                let _ = startup.send(Ok(metadata));
                while let Some(command) = receiver.blocking_recv() {
                    match command {
                        Command::Modify(request, reply) => {
                            let _ = reply.send(records::modify(&mut connection, request));
                        }
                        Command::Lookup(request, reply) => {
                            let _ = reply.send(records::lookup(&mut connection, request));
                        }
                        #[cfg(test)]
                        Command::Hold(started, release) => {
                            let _ = started.send(());
                            let _ = release.recv();
                        }
                        Command::Ping(reply) => {
                            let result = connection
                                .query_row("SELECT 1", [], |_| Ok(()))
                                .map_err(Error::from);
                            let _ = reply.send(result);
                        }
                    }
                    if let Some(inner) = worker_inner.upgrade() {
                        inner.completed.fetch_add(1, Ordering::Relaxed);
                    }
                }
                if let Some(inner) = worker_inner.upgrade() {
                    inner.alive.store(false, Ordering::Release);
                }
                jobs::finish(&mut connection, run_id)?;
                // Connection drops before the directory lock. No lock release during a transaction.
                drop(connection);
                Ok(())
            })?;
        *inner.worker.lock().map_err(|_| Error::WorkerFailed)? = Some(worker);
        let metadata = match initialized.await {
            Ok(Ok(metadata)) => metadata,
            Ok(Err(error)) => {
                inner_shutdown(&inner).await?;
                return Err(error);
            }
            Err(_) => {
                inner_shutdown(&inner).await?;
                return Err(Error::WorkerFailed);
            }
        };
        let store = Self {
            inner,
            metadata: Arc::new(metadata),
        };
        Ok(store)
    }

    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
    pub fn accepting(&self) -> bool {
        self.inner.alive.load(Ordering::Acquire)
            && self
                .inner
                .sender
                .lock()
                .map(|sender| sender.as_ref().is_some_and(|tx| !tx.is_closed()))
                .unwrap_or(false)
    }
    pub fn metrics(&self) -> QueueMetrics {
        let depth = self
            .inner
            .sender
            .lock()
            .ok()
            .and_then(|s| s.as_ref().map(|tx| tx.max_capacity() - tx.capacity()))
            .unwrap_or(0);
        QueueMetrics {
            queue_depth: depth,
            queue_capacity: self.inner.capacity,
            queue_rejections: self.inner.rejections.load(Ordering::Relaxed),
            commands_completed: self.inner.completed.load(Ordering::Relaxed),
        }
    }
    fn send(&self, command: Command) -> Result<()> {
        let guard = self.inner.sender.lock().map_err(|_| Error::WorkerFailed)?;
        let sender = guard.as_ref().ok_or(Error::Unavailable)?;
        sender.try_send(command).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => {
                self.inner.rejections.fetch_add(1, Ordering::Relaxed);
                Error::Busy
            }
            mpsc::error::TrySendError::Closed(_) => Error::Unavailable,
        })
    }
    pub async fn ping(&self) -> Result<()> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Ping(reply))?;
        response.await.map_err(|_| Error::WorkerFailed)?
    }
    pub async fn modify(&self, request: crate::domain::records::Modify) -> Result<Outcome> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Modify(request, reply))?;
        response.await.map_err(|_| Error::WorkerFailed)?
    }
    pub async fn lookup(&self, request: crate::domain::records::Lookup) -> Result<Outcome> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Lookup(request, reply))?;
        response.await.map_err(|_| Error::WorkerFailed)?
    }
    pub async fn shutdown(&self) -> Result<()> {
        inner_shutdown(&self.inner).await
    }
}

async fn inner_shutdown(inner: &Arc<Inner>) -> Result<()> {
    let _closing = inner.shutdown.lock().await;
    inner.sender.lock().map_err(|_| Error::WorkerFailed)?.take();
    let worker = inner.worker.lock().map_err(|_| Error::WorkerFailed)?.take();
    if let Some(worker) = worker {
        tokio::task::spawn_blocking(move || worker.join().map_err(|_| Error::WorkerFailed)?)
            .await
            .map_err(|_| Error::WorkerFailed)??;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn bounded_queue_rejects_excess_work_and_drains_cancelled_commands_before_unlocking() {
        let root = tempfile::tempdir().unwrap();
        let config = Config::from_pairs([
            (
                "SYNC_DATA_DIR",
                root.path().join("data").display().to_string(),
            ),
            ("SYNC_API_KEY_FILE", "/unused/api".into()),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing".into()),
            ("SYNC_DB_QUEUE_CAPACITY", "1".into()),
        ])
        .unwrap();
        let store = Store::open(config.clone(), [1; 32]).await.unwrap();
        let (started, entered) = oneshot::channel();
        let (release, wait) = std::sync::mpsc::channel();
        store
            .inner
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(Command::Hold(started, wait))
            .unwrap();
        entered.await.unwrap();
        let (reply, cancelled) = oneshot::channel();
        store
            .inner
            .sender
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .try_send(Command::Ping(reply))
            .unwrap();
        drop(cancelled);
        assert!(matches!(store.ping().await, Err(Error::Busy)));
        assert_eq!(store.metrics().queue_depth, 1);
        assert_eq!(store.metrics().queue_rejections, 1);
        // Async timers keep running while the dedicated SQLite thread is held.
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            tokio::time::sleep(std::time::Duration::from_millis(1)),
        )
        .await
        .unwrap();
        let first = store.clone();
        let closing = tokio::spawn(async move { first.shutdown().await });
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while store.accepting() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let second = store.clone();
        let duplicate = tokio::spawn(async move { second.shutdown().await });
        tokio::task::yield_now().await;
        assert!(!closing.is_finished());
        assert!(!duplicate.is_finished());
        assert!(matches!(
            Store::open(config.clone(), [1; 32]).await,
            Err(Error::DataDirLocked)
        ));
        release.send(()).unwrap();
        closing.await.unwrap().unwrap();
        duplicate.await.unwrap().unwrap();
        assert_eq!(store.metrics().commands_completed, 2);
        let reopened = Store::open(config, [1; 32]).await.unwrap();
        assert_eq!(reopened.metadata().recovery.interrupted_starts, 0);
        reopened.shutdown().await.unwrap();
    }
}
