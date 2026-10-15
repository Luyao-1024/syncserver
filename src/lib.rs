//! Incremental photo metadata, fenced upload leases and immutable paired resources.
mod api;
mod assets;
mod auth;
mod config;
mod credentials;
pub mod domain;
mod error;
pub mod jobs;
pub mod store;
mod tokens;
pub mod user_cli;
pub mod users;

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
        let store = store::Store::open(
            config.clone(),
            credentials.signing_fingerprint,
            credentials.tokens.clone(),
        )
        .await?;
        let root = config.data_dir.clone();
        let users = tokio::task::spawn_blocking(move || users::Users::open(&root))
            .await
            .map_err(|_| Error::WorkerFailed)??;
        let authority = Arc::new(auth::Authority::new(
            config.clone(),
            credentials.clone(),
            store.clone(),
            users,
        ));
        let mut state = api::AppState::new(config, credentials, store);
        state.authority = Some(authority.clone());
        let state = Arc::new(state);
        authority.bind_legacy_runtime(&state);
        Ok(Self { state })
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
        if let Some(authority) = &self.state.authority {
            authority.shutdown().await?;
        }
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
        if let Some(authority) = &self.state.authority {
            authority.shutdown().await?;
        }
        let database = self.state.store.shutdown().await;
        result?;
        database
    }
}
