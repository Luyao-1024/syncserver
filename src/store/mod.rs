mod assets;
pub(crate) mod files;
mod leases;
mod migrations;
mod records;
mod sync;
pub(crate) use leases::lost as lease_lost;
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
    pub control_queue_depth: usize,
    pub control_queue_capacity: usize,
    pub control_queue_rejections: u64,
}

enum Command {
    Maintenance,
    Ping(oneshot::Sender<Result<()>>),
    Asset(
        crate::domain::assets::AssetCommand,
        oneshot::Sender<Result<crate::domain::assets::AssetReply>>,
    ),
    Control(
        crate::domain::leases::Control,
        oneshot::Sender<Result<Outcome>>,
    ),
    Read(
        crate::domain::sync::Read,
        Arc<crate::tokens::TokenCodec>,
        oneshot::Sender<Result<Outcome>>,
    ),
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
    control_sender: Mutex<Option<mpsc::Sender<Command>>>,
    worker: Mutex<Option<thread::JoinHandle<Result<()>>>>,
    alive: AtomicBool,
    rejections: AtomicU64,
    completed: AtomicU64,
    capacity: usize,
    control_capacity: usize,
    control_rejections: AtomicU64,
    shutdown: tokio::sync::Mutex<()>,
}

#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
    metadata: Arc<Metadata>,
}

impl Store {
    pub(crate) async fn open(
        config: Config,
        fingerprint: [u8; 32],
        tokens: Arc<crate::tokens::TokenCodec>,
    ) -> Result<Self> {
        let (sender, mut receiver) = mpsc::channel(config.limits.db_queue_capacity);
        let (control_sender, mut control_receiver) =
            mpsc::channel(config.limits.control_queue_capacity);
        let inner = Arc::new(Inner {
            sender: Mutex::new(Some(sender)),
            control_sender: Mutex::new(Some(control_sender)),
            worker: Mutex::new(None),
            alive: AtomicBool::new(false),
            rejections: AtomicU64::new(0),
            completed: AtomicU64::new(0),
            capacity: config.limits.db_queue_capacity,
            control_capacity: config.limits.control_queue_capacity,
            control_rejections: AtomicU64::new(0),
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
                    let recovery = jobs::recover(&mut connection,&config.data_dir)?;
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
                let database_generation = metadata.database_generation;
                if let Some(inner) = worker_inner.upgrade() {
                    inner.alive.store(true, Ordering::Release);
                }
                let _ = startup.send(Ok(metadata));
                let clock=leases::Clock::new();
                let executor=tokio::runtime::Builder::new_current_thread().enable_time().build().map_err(|_|Error::WorkerFailed)?;
                let mut maintenance={let _entered=executor.enter();tokio::time::interval_at(tokio::time::Instant::now()+std::time::Duration::from_secs(60),std::time::Duration::from_secs(60))};
                maintenance.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                let mut normal_closed=false;let mut control_closed=false;
                loop {
                    if normal_closed && control_closed {break;}
                    let command=executor.block_on(async {
                        tokio::select! {biased;
                            command=control_receiver.recv(), if !control_closed=>{if command.is_none(){control_closed=true;}command},
                            command=receiver.recv(), if !normal_closed=>{if command.is_none(){normal_closed=true;}command},
                            _=maintenance.tick()=>Some(Command::Maintenance),
                            else=>None,
                        }
                    });
                    let Some(command)=command else {continue;};
                    match command {
                        Command::Maintenance=>{if jobs::expire_sessions(&mut connection).is_err(){tracing::error!("upload expiry maintenance failed");}},
                        Command::Asset(request,reply)=>{
                            let guard=leases::Guard{clock:&clock,config:&config,generation:database_generation,codec:&tokens};
                            let _=reply.send(assets::execute(&mut connection,request,&guard));
                        }
                        Command::Read(request, codec, reply) => {
                            let needs_render=matches!(&request,crate::domain::sync::Read::Lookup(r) if r.snapshot_token.is_none());
                            let guard=leases::Guard{clock:&clock,config:&config,generation:database_generation,codec:&tokens};
                            let result=sync::read(
                                &mut connection,
                                request,
                                &codec,
                                &config.namespace,
                                database_generation,
                                &guard,
                            );
                            let guard=leases::Guard{clock:&clock,config:&config,generation:database_generation,codec:&tokens};
                            let _=reply.send(if needs_render {render_outcome(&connection,result,&guard)}else{result});
                        }
                        Command::Control(request,reply)=>{
                            let guard=leases::Guard{clock:&clock,config:&config,generation:database_generation,codec:&tokens};
                            let result=guard.control(&mut connection,request);let _=reply.send(render_outcome(&connection,result,&guard));
                        }
                        Command::Modify(request, reply) => {
                            let guard=leases::Guard{clock:&clock,config:&config,generation:database_generation,codec:&tokens};
                            let result=records::modify(&mut connection,request,&guard);let _=reply.send(render_outcome(&connection,result,&guard));
                        }
                        Command::Lookup(request, reply) => {
                            let result=records::lookup(&mut connection, request);
                            let guard=leases::Guard{clock:&clock,config:&config,generation:database_generation,codec:&tokens};
                            let _=reply.send(render_outcome(&connection,result,&guard));
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
            control_queue_depth: self
                .inner
                .control_sender
                .lock()
                .ok()
                .and_then(|s| s.as_ref().map(|tx| tx.max_capacity() - tx.capacity()))
                .unwrap_or(0),
            control_queue_capacity: self.inner.control_capacity,
            control_queue_rejections: self.inner.control_rejections.load(Ordering::Relaxed),
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
    fn send_control(&self, command: Command) -> Result<()> {
        let guard = self
            .inner
            .control_sender
            .lock()
            .map_err(|_| Error::WorkerFailed)?;
        guard
            .as_ref()
            .ok_or(Error::Unavailable)?
            .try_send(command)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => {
                    self.inner
                        .control_rejections
                        .fetch_add(1, Ordering::Relaxed);
                    Error::Busy
                }
                mpsc::error::TrySendError::Closed(_) => Error::Unavailable,
            })
    }
    pub(crate) async fn asset(
        &self,
        request: crate::domain::assets::AssetCommand,
    ) -> Result<crate::domain::assets::AssetReply> {
        let (reply, response) = oneshot::channel();
        if matches!(
            request,
            crate::domain::assets::AssetCommand::Validate(..)
                | crate::domain::assets::AssetCommand::Fail(..)
        ) {
            self.send_control(Command::Asset(request, reply))?;
        } else {
            self.send(Command::Asset(request, reply))?;
        }
        response.await.map_err(|_| Error::WorkerFailed)?
    }
    pub(crate) async fn control(&self, request: crate::domain::leases::Control) -> Result<Outcome> {
        let (reply, response) = oneshot::channel();
        if matches!(request, crate::domain::leases::Control::Status(_)) {
            self.send(Command::Control(request, reply))?;
        } else {
            self.send_control(Command::Control(request, reply))?;
        }
        response.await.map_err(|_| Error::WorkerFailed)?
    }
    pub(crate) async fn read(
        &self,
        request: crate::domain::sync::Read,
        codec: Arc<crate::tokens::TokenCodec>,
    ) -> Result<Outcome> {
        let (reply, response) = oneshot::channel();
        self.send(Command::Read(request, codec, reply))?;
        response.await.map_err(|_| Error::WorkerFailed)?
    }
    pub async fn shutdown(&self) -> Result<()> {
        inner_shutdown(&self.inner).await
    }
}

async fn inner_shutdown(inner: &Arc<Inner>) -> Result<()> {
    let _closing = inner.shutdown.lock().await;
    inner.sender.lock().map_err(|_| Error::WorkerFailed)?.take();
    inner
        .control_sender
        .lock()
        .map_err(|_| Error::WorkerFailed)?
        .take();
    let worker = inner.worker.lock().map_err(|_| Error::WorkerFailed)?.take();
    if let Some(worker) = worker {
        tokio::task::spawn_blocking(move || worker.join().map_err(|_| Error::WorkerFailed)?)
            .await
            .map_err(|_| Error::WorkerFailed)??;
    }
    Ok(())
}

fn render_outcome(
    connection: &Connection,
    result: Result<Outcome>,
    guard: &leases::Guard<'_>,
) -> Result<Outcome> {
    let mut result = result?;
    assets::render_bounded(connection, &mut result.body, guard)?;
    if serde_json::to_vec(&result.body)
        .map_err(|_| Error::RecoveryFailed)?
        .len()
        > crate::domain::records::MAX_RESULT_BYTES
    {
        return Err(crate::domain::records::Failure::limit().into());
    }
    Ok(result)
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
            ("SYNC_CONTROL_QUEUE_CAPACITY", "1".into()),
        ])
        .unwrap();
        let store = Store::open(
            config.clone(),
            [1; 32],
            Arc::new(crate::tokens::TokenCodec::new([1; 32])),
        )
        .await
        .unwrap();
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
        let (control_reply, control_response) = oneshot::channel();
        store.send_control(Command::Ping(control_reply)).unwrap();
        drop(control_response);
        let (extra_reply, _) = oneshot::channel();
        assert!(matches!(
            store.send_control(Command::Ping(extra_reply)),
            Err(Error::Busy)
        ));
        let metrics = store.metrics();
        assert_eq!(metrics.control_queue_depth, 1);
        assert_eq!(metrics.control_queue_capacity, 1);
        assert_eq!(metrics.control_queue_rejections, 1);
        assert_eq!(metrics.queue_rejections, 1);

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
            Store::open(
                config.clone(),
                [1; 32],
                Arc::new(crate::tokens::TokenCodec::new([1; 32]))
            )
            .await,
            Err(Error::DataDirLocked)
        ));
        release.send(()).unwrap();
        closing.await.unwrap().unwrap();
        duplicate.await.unwrap().unwrap();
        assert_eq!(store.metrics().commands_completed, 3);
        assert_eq!(store.metrics().control_queue_depth, 0);
        let reopened = Store::open(
            config,
            [1; 32],
            Arc::new(crate::tokens::TokenCodec::new([1; 32])),
        )
        .await
        .unwrap();
        assert_eq!(reopened.metadata().recovery.interrupted_starts, 0);
        reopened.shutdown().await.unwrap();
    }
}
