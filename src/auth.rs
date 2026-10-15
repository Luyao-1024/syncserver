//! Authentication selects a server-owned storage context, before any API handler.
use crate::{
    Config, Error, Result,
    api::AppState,
    credentials::Credentials,
    store::{Store, files::directory},
    users::{User, Users},
};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, OnceLock, Weak},
    time::{Duration, Instant},
};
use tokio::sync::{OnceCell, Semaphore};

type UserCell = Arc<OnceCell<Arc<AppState>>>;
pub(crate) struct Authority {
    pub users: Arc<Users>,
    config: Config,
    credentials: Credentials,
    legacy: Store,
    legacy_runtime: OnceLock<Weak<AppState>>,
    contexts: Mutex<HashMap<String, UserCell>>,
    pub passwords: Arc<Semaphore>,
    attempts: Mutex<HashMap<String, (Instant, u32)>>,
}
impl Authority {
    pub fn new(config: Config, credentials: Credentials, legacy: Store, users: Users) -> Self {
        Self {
            users: Arc::new(users),
            config,
            credentials,
            legacy,
            legacy_runtime: OnceLock::new(),
            contexts: Mutex::new(HashMap::new()),
            passwords: Arc::new(Semaphore::new(2)),
            attempts: Mutex::new(HashMap::new()),
        }
    }
    pub fn bind_legacy_runtime(&self, state: &Arc<AppState>) {
        let _ = self.legacy_runtime.set(Arc::downgrade(state));
    }
    pub fn admit_login(&self, name: &str) -> Result<bool> {
        let mut attempts = self.attempts.lock().map_err(|_| Error::WorkerFailed)?;
        attempts.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        if attempts.len() >= 4096 && !attempts.contains_key(name) {
            return Ok(false);
        }
        let entry = attempts
            .entry(name.to_owned())
            .or_insert((Instant::now(), 0));
        entry.1 = entry.1.saturating_add(1);
        Ok(entry.1 <= 8)
    }
    pub async fn legacy_allowed(&self) -> Result<bool> {
        let users = self.users.clone();
        tokio::task::spawn_blocking(move || users.has_users().map(|v| !v))
            .await
            .map_err(|_| Error::WorkerFailed)?
    }
    pub async fn resolve(&self, token: &str) -> Result<Option<Arc<AppState>>> {
        let users = self.users.clone();
        let secret = zeroize::Zeroizing::new(token.to_owned());
        let user = tokio::task::spawn_blocking(move || users.authenticate(&secret))
            .await
            .map_err(|_| Error::WorkerFailed)??;
        let Some(user) = user else { return Ok(None) };
        let cell = {
            let mut contexts = self.contexts.lock().map_err(|_| Error::WorkerFailed)?;
            if !contexts.contains_key(&user.id) && contexts.len() >= 128 {
                return Err(Error::Busy);
            }
            contexts
                .entry(user.id.clone())
                .or_insert_with(|| Arc::new(OnceCell::new()))
                .clone()
        };
        let state = cell
            .get_or_try_init(|| self.open_context(user))
            .await?
            .clone();
        // A reset/disable during store recovery cannot authorize this request.
        let users = self.users.clone();
        let secret = zeroize::Zeroizing::new(token.to_owned());
        let still_valid = tokio::task::spawn_blocking(move || users.authenticate(&secret))
            .await
            .map_err(|_| Error::WorkerFailed)??;
        Ok(still_valid
            .filter(|u| Some(&u.id) == state.user.as_ref().map(|u| &u.id))
            .map(|_| state))
    }
    async fn open_context(&self, user: User) -> Result<Arc<AppState>> {
        let mut config = self.config.clone();
        let credentials;
        let store;
        if user.legacy {
            credentials = self.credentials.clone();
            store = self.legacy.clone();
        } else {
            uuid::Uuid::parse_str(&user.id).map_err(|_| Error::RecoveryFailed)?;
            let parent = config.data_dir.join("users");
            tokio::task::spawn_blocking(move || directory(&parent))
                .await
                .map_err(|_| Error::WorkerFailed)??;
            config.data_dir = config.data_dir.join("users").join(&user.id);
            config.namespace.tenant = user.id.clone();
            credentials = self.credentials.scoped(&user.id)?;
            store = Store::open(
                config.clone(),
                credentials.signing_fingerprint,
                credentials.tokens.clone(),
            )
            .await?;
        }
        let mut state = AppState::new(config, credentials, store);
        if user.legacy {
            let root = self
                .legacy_runtime
                .get()
                .and_then(Weak::upgrade)
                .ok_or(Error::Unavailable)?;
            // Requests accepted just before adoption and owner-session requests
            // still share one publication exclusion and transfer capacity.
            state.publication = root.publication.clone();
            state.asset_metrics = root.asset_metrics.clone();
            state.original_uploads = root.original_uploads.clone();
            state.thumbnail_uploads = root.thumbnail_uploads.clone();
            state.original_downloads = root.original_downloads.clone();
            state.thumbnail_downloads = root.thumbnail_downloads.clone();
        }
        state.user = Some(user);
        Ok(Arc::new(state))
    }
    pub async fn shutdown(&self) -> Result<()> {
        let contexts = self
            .contexts
            .lock()
            .map_err(|_| Error::WorkerFailed)?
            .values()
            .filter_map(|cell| cell.get().cloned())
            .collect::<Vec<_>>();
        for state in contexts {
            if state.user.as_ref().is_some_and(|u| !u.legacy) {
                state.store.shutdown().await?;
            }
        }
        Ok(())
    }
}
