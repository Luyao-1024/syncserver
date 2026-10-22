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
    pub login_bodies: Arc<Semaphore>,
    attempts: Mutex<HashMap<(std::net::IpAddr, String), (Instant, u32)>>,
    sources: Mutex<HashMap<std::net::IpAddr, (Instant, u32)>>,
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
            login_bodies: Arc::new(Semaphore::new(16)),
            attempts: Mutex::new(HashMap::new()),
            sources: Mutex::new(HashMap::new()),
        }
    }
    pub fn bind_legacy_runtime(&self, state: &Arc<AppState>) {
        let _ = self.legacy_runtime.set(Arc::downgrade(state));
    }
    pub fn admit_login(&self, source: std::net::IpAddr) -> Result<bool> {
        let mut sources = self.sources.lock().map_err(|_| Error::WorkerFailed)?;
        sources.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        if sources.len() >= 4096 && !sources.contains_key(&source) {
            return Ok(false);
        }
        let entry = sources.entry(source).or_insert((Instant::now(), 0));
        entry.1 = entry.1.saturating_add(1);
        Ok(entry.1 <= 32)
    }
    // A failed attempt must never consume a valid password's account-wide quota.
    // The source admission and Argon2 semaphore still bound password computation.
    pub fn login_outcome(
        &self,
        source: std::net::IpAddr,
        name: &str,
        success: bool,
    ) -> Result<bool> {
        let mut attempts = self.attempts.lock().map_err(|_| Error::WorkerFailed)?;
        attempts.retain(|_, (at, _)| at.elapsed() < Duration::from_secs(60));
        let key = (source, name.to_owned());
        if success {
            attempts.remove(&key);
            return Ok(true);
        }
        if attempts.len() >= 4096 && !attempts.contains_key(&key) {
            return Ok(false);
        }
        let entry = attempts.entry(key).or_insert((Instant::now(), 0));
        entry.1 = entry.1.saturating_add(1);
        Ok(entry.1 <= 8)
    }
    pub async fn legacy_allowed(&self) -> Result<bool> {
        let users = self.users.clone();
        tokio::task::spawn_blocking(move || users.has_users().map(|v| !v))
            .await
            .map_err(|_| Error::WorkerFailed)?
    }
    pub async fn resolve(&self, token: &str) -> Result<Option<(Arc<AppState>, User)>> {
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
            .filter(|u| Some(&u.id) == state.account_id.as_ref())
            .map(|user| (state, user)))
    }
    /// Pushes a new ceiling into an already-open context so an administrator sees
    /// the effect immediately. An account with no open context picks the value up
    /// from the catalogue the next time it authenticates.
    pub fn apply_quota(&self, id: &str, bytes: u64) -> Result<()> {
        let contexts = self.contexts.lock().map_err(|_| Error::WorkerFailed)?;
        if let Some(state) = contexts.get(id).and_then(|cell| cell.get()) {
            state.store.set_quota_bytes(bytes);
        }
        Ok(())
    }
    /// The store of an account this process currently holds open, if any. The
    /// administration console uses it to compute that account's usage exactly rather
    /// than as a bound, because the lease clock belongs to that store.
    pub fn open_store(&self, id: &str) -> Option<Store> {
        self.contexts
            .lock()
            .ok()?
            .get(id)
            .and_then(|cell| cell.get())
            .map(|state| state.store.clone())
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
            // The per-account override is resolved here and nowhere else: the store's
            // database worker captures this config, so every quota check, admission
            // decision and usage report for this account uses the same ceiling.
            if let Some(bytes) = user.quota_bytes {
                config.limits.user_storage_quota_bytes =
                    u64::try_from(bytes).map_err(|_| Error::RecoveryFailed)?;
            }
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
        state.legacy = user.legacy;
        state.account_id = Some(user.id);
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
            if !state.legacy {
                state.store.shutdown().await?;
            }
        }
        Ok(())
    }
}
