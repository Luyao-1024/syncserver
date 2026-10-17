//! Administration API for the console's management view.
//!
//! Every route here is refused unless the session belongs to an account whose role
//! is `admin`. Nothing in this module can read, list or download a photo of any
//! account: administration sees account state and storage accounting only. There is
//! deliberately no impersonation path, and no parameter can select another account's
//! namespace for a data read.
use super::{AppState, error::ApiError};
use crate::{
    Error, Result,
    store::admin_usage::{self, AccountUsage},
    users::{Account, Role, User},
};
use axum::{
    Json,
    extract::{Extension, Path, Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use zeroize::Zeroizing;

fn denied() -> Response {
    ApiError::new(
        StatusCode::FORBIDDEN,
        "ADMIN_REQUIRED",
        "This endpoint requires an administrator session",
    )
    .into_response()
}

/// Administration runs against the authentication catalogue, not a photo namespace,
/// so it is served from the deployment's own context rather than the caller's.
fn authority(state: &Arc<AppState>) -> Option<&Arc<crate::auth::Authority>> {
    state.authority.as_ref()
}

fn require_admin(state: &Arc<AppState>) -> Option<User> {
    let user = state.user.clone()?;
    user.role.is_admin().then_some(user)
}

fn not_found() -> Response {
    ApiError::new(StatusCode::NOT_FOUND, "UNKNOWN_ITEM", "Unknown account").into_response()
}

/// Strict query parsing shared by the read-only administration views.
fn parameters(
    request: &Request,
    allowed: &[&str],
) -> std::result::Result<BTreeMap<String, String>, ApiError> {
    let mut params = BTreeMap::new();
    for (key, value) in
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
    {
        if !allowed.contains(&key.as_ref())
            || params
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(ApiError::bad_request());
        }
    }
    Ok(params)
}

fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UserSummary {
    #[serde(flatten)]
    account: Account,
    usage: UsageView,
    physical_bytes: u64,
    has_store: bool,
    /// False when this account's database could not be read, so its zeros mean
    /// "unknown", not "empty".
    readable: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct UsageView {
    quota_bytes: u64,
    quota_is_override: bool,
    default_quota_bytes: u64,
    used_bytes: u64,
    reserved_bytes: u64,
    available_bytes: u64,
    reserved_exact: bool,
    image_bytes: u64,
    video_bytes: u64,
    other_bytes: u64,
}

/// The catalogue and the physical layout of one account's storage directory.
fn account_directory(state: &Arc<AppState>, account: &Account) -> std::path::PathBuf {
    if account.legacy {
        state.config.data_dir.clone()
    } else {
        state.config.data_dir.join("users").join(&account.id)
    }
}

fn effective_quota(state: &Arc<AppState>, account: &Account) -> u64 {
    account
        .quota_bytes
        .unwrap_or(state.config.limits.user_storage_quota_bytes)
}

fn usage_view(state: &Arc<AppState>, account: &Account, usage: &AccountUsage) -> UsageView {
    UsageView {
        quota_bytes: usage.usage.quota_bytes,
        quota_is_override: account.quota_bytes.is_some(),
        default_quota_bytes: state.config.limits.user_storage_quota_bytes,
        used_bytes: usage.usage.used_bytes,
        reserved_bytes: usage.usage.reserved_bytes,
        available_bytes: usage.usage.available_bytes,
        reserved_exact: usage.usage.reserved_exact,
        image_bytes: usage.usage.media_usage.image_bytes,
        video_bytes: usage.usage.media_usage.video_bytes,
        other_bytes: usage.usage.media_usage.other_bytes,
    }
}

/// Reads one account's usage, preferring the exact route.
///
/// An account whose store this process already holds open is read through that
/// store, so its reservations are compared against the right lease clock and the
/// figure is exact. Any other account is read from its metadata database through a
/// lock-free connection, which yields an upper bound on reservations.
///
/// Accounts are read one after another rather than concurrently: each is a single
/// small metadata aggregation, and a console refresh must never turn into a burst of
/// work competing with real synchronization.
async fn read_usage(
    authority: &crate::auth::Authority,
    state: &Arc<AppState>,
    account: &Account,
) -> Result<AccountUsage> {
    let quota = effective_quota(state, account);
    if let Some(store) = authority.open_store(&account.id) {
        let usage = store.storage_usage().await?;
        let directory = account_directory(state, account);
        let legacy = account.legacy;
        let physical =
            tokio::task::spawn_blocking(move || admin_usage::owned_bytes(&directory, legacy))
                .await
                .map_err(|_| Error::WorkerFailed)??;
        return Ok(AccountUsage {
            usage,
            has_store: true,
            physical_bytes: physical,
        });
    }
    let directory = account_directory(state, account);
    tokio::task::spawn_blocking(move || admin_usage::account_usage(&directory, quota))
        .await
        .map_err(|_| Error::WorkerFailed)?
}

pub(super) async fn users(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
) -> Response {
    let Some(_actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    let users = authority.users.clone();
    let accounts = match tokio::task::spawn_blocking(move || users.accounts()).await {
        Ok(Ok(accounts)) => accounts,
        Ok(Err(error)) => return ApiError::from(error).into_response(),
        Err(_) => return ApiError::from(Error::WorkerFailed).into_response(),
    };
    let mut summaries = Vec::with_capacity(accounts.len());
    for account in accounts {
        let usage = read_usage(authority, &root, &account).await;
        // One unreadable account must not hide the rest of the deployment, and it
        // must not look like an empty account either.
        let readable = usage.is_ok();
        let quota = effective_quota(&root, &account);
        let usage = usage.unwrap_or(AccountUsage {
            usage: crate::store::Usage::zero(quota),
            has_store: false,
            physical_bytes: 0,
        });
        summaries.push(UserSummary {
            usage: usage_view(&root, &account, &usage),
            physical_bytes: usage.physical_bytes,
            has_store: usage.has_store,
            readable,
            account,
        });
    }
    no_store(Json(json!({"users": summaries})).into_response())
}

/// Looks one account up by immutable id. Catalogue management addresses accounts by
/// id; a name is never accepted here, so an id from one deployment cannot resolve
/// against another.
async fn lookup(
    authority: &crate::auth::Authority,
    id: &str,
) -> std::result::Result<Account, Box<Response>> {
    let users = authority.users.clone();
    let wanted = id.to_owned();
    let accounts = match tokio::task::spawn_blocking(move || users.accounts()).await {
        Ok(Ok(accounts)) => accounts,
        Ok(Err(error)) => return Err(Box::new(ApiError::from(error).into_response())),
        Err(_) => {
            return Err(Box::new(
                ApiError::from(Error::WorkerFailed).into_response(),
            ));
        }
    };
    accounts
        .into_iter()
        .find(|account| account.id == wanted)
        .ok_or_else(|| Box::new(not_found()))
}

pub(super) async fn storage(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
) -> Response {
    let Some(_actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    let account = match lookup(authority, &id).await {
        Ok(account) => account,
        Err(response) => return *response,
    };
    match read_usage(authority, &root, &account).await {
        Ok(usage) => no_store(
            Json(json!({
                "user": account,
                "usage": usage_view(&root, &account, &usage),
                "physicalBytes": usage.physical_bytes,
                "hasStore": usage.has_store,
            }))
            .into_response(),
        ),
        Err(error) => ApiError::from(error).into_response(),
    }
}

pub(super) async fn overview(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let Some(_actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    // The history is drawn at a resolution that stays readable at the requested span:
    // five-second detail for the last quarter hour, minute rollups for a day, hour
    // rollups for a week.
    let window = match parameters(&request, &["window"]) {
        Ok(params) => match params.get("window").map(String::as_str) {
            None => crate::host::Window::QuarterHour,
            Some(raw) => match crate::host::Window::parse(raw) {
                Some(window) => window,
                None => return ApiError::bad_request().into_response(),
            },
        },
        Err(error) => return error.into_response(),
    };
    let (window, resolution_ms, history) = root.samples.window(window);
    let users = authority.users.clone();
    let accounts = match tokio::task::spawn_blocking(move || users.accounts()).await {
        Ok(Ok(accounts)) => accounts,
        Ok(Err(error)) => return ApiError::from(error).into_response(),
        Err(_) => return ApiError::from(Error::WorkerFailed).into_response(),
    };
    let mut logical = 0u64;
    let mut quota_total = 0u64;
    let mut image = 0u64;
    let mut video = 0u64;
    let mut other = 0u64;
    let mut over_quota = 0u64;
    let mut readable = true;
    for account in &accounts {
        // An unreadable account is counted as unknown rather than as empty, so the
        // console can say the total is incomplete instead of quietly understating it.
        let Ok(usage) = read_usage(authority, &root, account).await else {
            readable = false;
            continue;
        };
        logical = logical.saturating_add(usage.usage.used_bytes);
        quota_total = quota_total.saturating_add(usage.usage.quota_bytes);
        image = image.saturating_add(usage.usage.media_usage.image_bytes);
        video = video.saturating_add(usage.usage.media_usage.video_bytes);
        other = other.saturating_add(usage.usage.media_usage.other_bytes);
        if usage.usage.used_bytes > usage.usage.quota_bytes {
            over_quota += 1;
        }
    }
    // Walking the data directory and reading the host figures both block, so neither
    // runs on the async runtime.
    let data_dir = root.config.data_dir.clone();
    let (data_bytes, disk) = tokio::task::spawn_blocking(move || {
        (
            admin_usage::directory_bytes(&data_dir).unwrap_or(0),
            crate::host::disk(&data_dir),
        )
    })
    .await
    .unwrap_or((0, Default::default()));
    // The live reading, read once more than the history so the console can show the
    // newest figure even between samples.
    let (process, memory) = tokio::task::spawn_blocking(|| {
        let process = crate::host::process();
        (process, crate::host::memory())
    })
    .await
    .unwrap_or((Default::default(), Default::default()));
    let latest = root.samples.latest();
    let http = &root.http;
    let http_total = http.success.load(std::sync::atomic::Ordering::Relaxed)
        + http
            .client_errors
            .load(std::sync::atomic::Ordering::Relaxed)
        + http
            .server_errors
            .load(std::sync::atomic::Ordering::Relaxed);
    let http_total_ms = http.total_ms.load(std::sync::atomic::Ordering::Relaxed);
    let routes = http
        .routes()
        .into_iter()
        .map(|(name, route)| {
            json!({
                "route": name,
                "requests": route.requests,
                "errors": route.errors,
                "averageMs": route.total_ms.checked_div(route.requests).unwrap_or(0),
            })
        })
        .collect::<Vec<Value>>();
    no_store(
        Json(json!({
            "service": "syncserver",
            "version": env!("CARGO_PKG_VERSION"),
            // Readiness, counters and metadata come from the deployment's own context.
            // Reading the caller's context here would describe an administrator's empty
            // namespace rather than the server.
            "ready": root.ready.load(std::sync::atomic::Ordering::Acquire),
            "runId": root.store.metadata().recovery.run_id,
            "databaseGeneration": root.store.metadata().database_generation,
            "schemaVersion": root.store.metadata().schema_version,
            "recovery": root.store.metadata().recovery,
            "limits": root.config.limits,
            "metrics": super::metrics::snapshot(&root),
            "http": {
                "requests": http_total,
                "success": http.success.load(std::sync::atomic::Ordering::Relaxed),
                "clientErrors": http.client_errors.load(std::sync::atomic::Ordering::Relaxed),
                "serverErrors": http.server_errors.load(std::sync::atomic::Ordering::Relaxed),
                "slowRequests": http.slow.load(std::sync::atomic::Ordering::Relaxed),
                "averageMs": http_total_ms.checked_div(http_total).unwrap_or(0),
                "slowestMs": http.slowest_ms.load(std::sync::atomic::Ordering::Relaxed),
                "admissionRejections": root.rejected.load(std::sync::atomic::Ordering::Relaxed),
                "routes": routes,
                // Oldest first. Sampled for the life of the service, so this is a real
                // recent history rather than a chart that starts when a page opens.
                "window": window.as_str(),
                "resolutionMs": resolution_ms,
                "history": history,
            },
            "host": {
                // The machine this deployment runs on...
                "systemCpuPercent": latest.system_cpu_percent,
                "systemCpuCores": latest.system_cpu_cores,
                "loadAverage": {
                    "oneMinute": latest.load_1m,
                    "fiveMinutes": latest.load_5m,
                    "fifteenMinutes": latest.load_15m,
                },
                "memoryTotalBytes": latest.host_memory_total_bytes.or(memory.total_bytes),
                // What programs and the kernel hold. This excludes the file cache,
                // which the kernel drops on demand, so it does not read as spent.
                "memoryUsedBytes": latest.host_memory_used_bytes.or_else(|| memory.used_bytes()),
                "memoryProcessBytes": latest.host_memory_anon_bytes.or(memory.anon_bytes),
                "memoryFileCacheBytes": latest.host_memory_file_cache_bytes.or(memory.file_cache_bytes),
                "memoryAvailableBytes": memory.available_bytes,
                "memorySwapTotalBytes": memory.swap_total_bytes,
                "memorySwapUsedBytes": memory.swap_used_bytes(),
                // ...and this process within it.
                "cpuPercent": latest.cpu_percent,
                "cpuTimeMs": process.cpu_time_ms,
                "processMemoryBytes": process.rss_bytes,
                "processVirtualBytes": process.virtual_bytes,
                "threads": process.threads,
                "openFiles": process.open_files,
            },
            "accounts": {
                "total": accounts.len(),
                "enabled": accounts.iter().filter(|account| account.enabled).count(),
                "administrators": accounts.iter().filter(|account| account.role.is_admin()).count(),
                "usageComplete": readable,
                "overQuota": over_quota,
                "logicalBytes": logical,
                "quotaBytes": quota_total,
                "imageBytes": image,
                "videoBytes": video,
                "otherBytes": other,
            },
            "disk": {
                "filesystemTotalBytes": disk.total_bytes,
                "filesystemFreeBytes": disk.free_bytes,
                // Counted once for the whole data directory rather than summed per
                // account: the account that adopted the legacy storage owns the root,
                // so a per-account sum would count every other account twice.
                "dataDirectoryBytes": data_bytes,
            },
        }))
        .into_response(),
    )
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateUser {
    username: String,
    password: String,
    #[serde(default)]
    admin: bool,
}

/// Runs the catalogue write on the blocking pool. Argon2 hashing is deliberately
/// slow; keeping it off the async worker matches the login path.
async fn blocking<T, F>(work: F) -> Response
where
    F: FnOnce() -> Result<T> + Send + 'static,
    T: serde::Serialize + Send + 'static,
{
    match tokio::task::spawn_blocking(work).await {
        Ok(Ok(value)) => Json(value).into_response(),
        // A rejected catalogue request is a client error, not a server fault.
        Ok(Err(error)) => catalogue_error(error).into_response(),
        Err(_) => ApiError::from(Error::WorkerFailed).into_response(),
    }
}

/// The catalogue rejects a request rather than failing: an unknown account, a taken
/// name or an out-of-range value is the caller's problem and must not be reported as
/// a server fault. The messages are this module's own static strings.
fn catalogue_error(error: Error) -> ApiError {
    match error {
        Error::Config(reason) => ApiError::new(StatusCode::BAD_REQUEST, "INVALID_REQUEST", reason),
        Error::LastAdministrator => ApiError::new(
            StatusCode::CONFLICT,
            "LAST_ADMINISTRATOR",
            "Promote another administrator before removing this account's privileges",
        ),
        other => ApiError::from(other),
    }
}

pub(super) async fn create_user(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let Some(actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    let body = match super::read_json(request, root.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Ok(create) = serde_json::from_value::<CreateUser>(body) else {
        return ApiError::bad_request().into_response();
    };
    let users = authority.users.clone();
    let username = create.username;
    let password = Zeroizing::new(create.password);
    let role = if create.admin {
        Role::Admin
    } else {
        Role::User
    };
    let response = blocking(move || {
        let user = users.create(&username, &password, false, role)?;
        // The response carries only the created identity, never the password.
        Ok(json!({"user": user}))
    })
    .await;
    if response.status().is_success() {
        tracing::info!(
            actor_id = %actor.id,
            actor_username = %actor.username,
            "account created"
        );
    }
    no_store(response)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct QuotaRequest {
    #[serde(default)]
    bytes: Option<u64>,
    #[serde(default)]
    inherit: bool,
}

pub(super) async fn set_quota(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let Some(actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    let body = match super::read_json(request, root.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Ok(change) = serde_json::from_value::<QuotaRequest>(body) else {
        return ApiError::bad_request().into_response();
    };
    // Exactly one of the two forms must be given, so a typo cannot silently clear a
    // quota that was meant to be raised.
    let requested = match (change.inherit, change.bytes) {
        (true, None) => None,
        (false, Some(bytes)) => Some(bytes),
        _ => return ApiError::bad_request().into_response(),
    };
    let users = authority.users.clone();
    let effective = requested.unwrap_or(root.config.limits.user_storage_quota_bytes);
    let target = id.clone();
    let response = blocking(move || {
        users.set_quota(&target, requested)?;
        Ok(json!({"quotaBytes": requested, "effectiveQuotaBytes": effective}))
    })
    .await;
    if response.status().is_success() {
        // A store that is already open must adopt the new ceiling now, not at the
        // next restart; a closed store reads it from the catalogue when it opens.
        if let Err(error) = authority.apply_quota(&id, effective) {
            return ApiError::from(error).into_response();
        }
        tracing::info!(
            actor_id = %actor.id,
            target_user_id = %id,
            effective_quota_bytes = effective,
            "account quota changed"
        );
    }
    no_store(response)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RoleRequest {
    role: String,
}

pub(super) async fn set_role(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let Some(actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    let body = match super::read_json(request, root.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Ok(change) = serde_json::from_value::<RoleRequest>(body) else {
        return ApiError::bad_request().into_response();
    };
    let Ok(role) = Role::parse(&change.role) else {
        return ApiError::bad_request().into_response();
    };
    if role == Role::Admin {
        // An administrator cannot synchronize, so promoting an account that already
        // holds photos would leave that library unreachable from every interface.
        // The bytes are not deleted — the account can be demoted again — but asking
        // for that by accident is worse than refusing here.
        let account = match lookup(authority, &id).await {
            Ok(account) => account,
            Err(response) => return *response,
        };
        match read_usage(authority, &root, &account).await {
            Ok(usage) if usage.usage.used_bytes > 0 => {
                return ApiError::new(
                    StatusCode::CONFLICT,
                    "ACCOUNT_HAS_PHOTOS",
                    "This account already stores photos; create a separate administrator account instead",
                )
                .into_response();
            }
            Ok(_) => {}
            // If the usage cannot be read, the guard would be guessing; refusing is
            // the safe direction, because the alternative strands a library silently.
            Err(_) => {
                return ApiError::new(
                    StatusCode::CONFLICT,
                    "ACCOUNT_USAGE_UNREADABLE",
                    "This account's storage could not be read, so it cannot be promoted safely",
                )
                .into_response();
            }
        }
    }
    let users = authority.users.clone();
    let target = id.clone();
    let response = blocking(move || {
        users.set_role(&target, role)?;
        Ok(json!({"role": role}))
    })
    .await;
    if response.status().is_success() {
        tracing::info!(
            actor_id = %actor.id,
            target_user_id = %id,
            role = role.as_str(),
            "account role changed"
        );
    }
    no_store(response)
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordRequest {
    password: String,
}

pub(super) async fn set_password(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let Some(actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    let body = match super::read_json(request, root.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Ok(change) = serde_json::from_value::<PasswordRequest>(body) else {
        return ApiError::bad_request().into_response();
    };
    let users = authority.users.clone();
    // Resetting a password revokes that account's sessions, so it must go through
    // the same path the command line uses.
    let target = match tokio::task::spawn_blocking({
        let users = users.clone();
        let id = id.clone();
        move || users.username_of(&id)
    })
    .await
    {
        Ok(Ok(name)) => name,
        Ok(Err(error)) => return ApiError::from(error).into_response(),
        Err(_) => return ApiError::from(Error::WorkerFailed).into_response(),
    };
    let password = Zeroizing::new(change.password);
    let response = blocking(move || {
        users.change_password(&target, &password)?;
        Ok(json!({"sessionsRevoked": true}))
    })
    .await;
    if response.status().is_success() {
        // The password itself never reaches a log line.
        tracing::info!(
            actor_id = %actor.id,
            target_user_id = %id,
            "account password reset"
        );
    }
    no_store(response)
}

pub(super) async fn set_enabled(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let Some(actor) = require_admin(&state) else {
        return denied();
    };
    let Some(authority) = authority(&root) else {
        return ApiError::from(Error::Unavailable).into_response();
    };
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    let body = match super::read_json(request, root.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let Some(enabled) = body.as_bool() else {
        return ApiError::bad_request().into_response();
    };
    let users = authority.users.clone();
    let target = match tokio::task::spawn_blocking({
        let users = users.clone();
        let id = id.clone();
        move || users.username_of(&id)
    })
    .await
    {
        Ok(Ok(name)) => name,
        Ok(Err(error)) => return ApiError::from(error).into_response(),
        Err(_) => return ApiError::from(Error::WorkerFailed).into_response(),
    };
    let response = blocking(move || {
        users.set_enabled(&target, enabled)?;
        Ok(json!({"enabled": enabled, "sessionsRevoked": !enabled}))
    })
    .await;
    if response.status().is_success() {
        tracing::info!(
            actor_id = %actor.id,
            target_user_id = %id,
            enabled,
            "account enabled state changed"
        );
    }
    no_store(response)
}

/// Tail of the deployment's own log database, for the console's operations view.
///
/// This reads the same redacted records the `syncserver logs` command prints. It
/// opens the log database read-only and takes no data directory lock, so it works
/// while the service is running and cannot slow the logging pipeline down. Passwords,
/// bearer tokens and capability URLs are already redacted before a record is
/// persisted, so nothing that reaches this endpoint needs masking again.
pub(super) async fn logs(
    State(root): State<Arc<AppState>>,
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let Some(_actor) = require_admin(&state) else {
        return denied();
    };
    let params = match parameters(&request, &["level", "since", "until", "contains", "limit"]) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let limit = match params.get("limit").map(String::as_str) {
        None => 200,
        Some(raw) => match raw.parse::<usize>() {
            Ok(value) if (1..=1000).contains(&value) => value,
            _ => return ApiError::bad_request().into_response(),
        },
    };
    let levels = match params.get("level") {
        None => Vec::new(),
        Some(raw) => raw
            .split(',')
            .map(|level| level.trim().to_ascii_uppercase())
            .filter(|level| !level.is_empty())
            .collect(),
    };
    // Bounded so one request cannot scan an unbounded window.
    let since_ms = params.get("since").and_then(|raw| raw.parse::<i64>().ok());
    let until_ms = params.get("until").and_then(|raw| raw.parse::<i64>().ok());
    let contains = params
        .get("contains")
        .filter(|value| !value.is_empty())
        .cloned();
    let path = root.config.data_dir.join(crate::logging::LOG_DATABASE);
    let persisted = path.exists();

    let read = tokio::task::spawn_blocking(move || {
        if !persisted {
            // Persisting is off in this deployment; say so rather than showing an
            // empty list that looks like a quiet server.
            return Ok(Vec::new());
        }
        crate::logging::query(
            &path,
            &crate::logging::LogFilter {
                levels,
                since_ms,
                until_ms,
                contains,
                request_id: None,
                limit,
                include_archive: false,
            },
        )
    })
    .await;

    let records = match read {
        Ok(Ok(records)) => records,
        Ok(Err(error)) => return ApiError::from(error).into_response(),
        Err(_) => return ApiError::from(Error::WorkerFailed).into_response(),
    };
    let entries: Vec<Value> = records
        .iter()
        .map(|stored| {
            let record = &stored.record;
            json!({
                "seq": stored.seq,
                "tsMs": record.ts_ms,
                "level": record.level,
                "target": record.target,
                "message": record.message,
                "fields": record.fields,
                "requestId": record.request_id,
            })
        })
        .collect();
    no_store(Json(json!({"logs": entries, "persisted": persisted})).into_response())
}
