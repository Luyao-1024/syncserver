//! M2 record transactions on the durable M1 startup and metadata foundation.
//! Snapshot/change feeds and trusted asset publication remain later milestones.
mod api;
mod config;
mod credentials;
pub mod domain;
mod error;
pub mod jobs;
pub mod store;

pub use config::{Config, Limits};
pub use error::{Error, Result};
use std::{
    future::Future,
    sync::{Arc, atomic::Ordering},
};
use tokio::net::TcpListener;

pub struct Server {
    state: Arc<api::AppState>,
}

impl Server {
    pub async fn open(config: Config) -> Result<Self> {
        let credential_config = config.clone();
        let credentials = tokio::task::spawn_blocking(move || {
            credentials::Credentials::load(
                &credential_config.api_key_file,
                &credential_config.signing_key_file,
            )
        })
        .await
        .map_err(|_| Error::WorkerFailed)??;
        let store = store::Store::open(config.clone(), credentials.signing_fingerprint).await?;
        Ok(Self {
            state: Arc::new(api::AppState::new(config, credentials, store)),
        })
    }

    pub fn router(&self) -> axum::Router {
        api::router(self.state.clone())
    }
    pub fn metadata(&self) -> &store::Metadata {
        self.state.store.metadata()
    }
    pub fn store(&self) -> store::Store {
        self.state.store.clone()
    }

    pub async fn shutdown(self) -> Result<()> {
        self.state.ready.store(false, Ordering::Release);
        self.state.store.shutdown().await
    }

    /// Listener ownership belongs to this server invocation. Graceful termination
    /// first closes HTTP admission, then drains queued database work and joins it.
    pub async fn serve<F>(self, listener: TcpListener, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send + 'static,
    {
        let state = self.state.clone();
        let result = axum::serve(listener, self.router())
            .with_graceful_shutdown(async move {
                shutdown.await;
                state.ready.store(false, Ordering::Release);
            })
            .await;
        self.state.ready.store(false, Ordering::Release);
        let database = self.state.store.shutdown().await;
        result?;
        database
    }
}
