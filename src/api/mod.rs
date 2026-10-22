mod admin;
mod assets;
mod auth;
mod console;
mod error;
mod json;
mod leases;
mod metrics;
mod records;
mod sync;

use crate::{Config, credentials::Credentials, store::Store};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Extension, Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use error::ApiError;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Semaphore;
use tracing::Instrument;
use uuid::Uuid;

#[derive(Clone, Default)]
pub(super) struct CurrentUser(pub Option<crate::users::User>);

pub(crate) struct AppState {
    pub authority: Option<Arc<crate::auth::Authority>>,
    // Storage identity only; authorization always uses the fresh request identity.
    pub account_id: Option<String>,
    pub legacy: bool,
    pub config: Config,
    pub credentials: Credentials,
    pub store: Store,
    pub ready: AtomicBool,
    admission: Arc<Semaphore>,
    control_admission: Arc<Semaphore>,
    metrics_admission: Arc<Semaphore>,
    pub original_uploads: Arc<Semaphore>,
    pub thumbnail_uploads: Arc<Semaphore>,
    pub original_downloads: Arc<Semaphore>,
    pub thumbnail_downloads: Arc<Semaphore>,
    pub asset_metrics: Arc<crate::assets::Metrics>,
    pub publication: Arc<std::sync::Mutex<()>>,
    pub requests: AtomicU64,
    pub rejected: AtomicU64,
    database_change_pages: AtomicU64,
    zone_change_pages: AtomicU64,
    /// Request accounting for the administration console. Counters live on the
    /// deployment's own context, so they describe the whole process rather than one
    /// account's namespace.
    pub http: HttpStats,
    /// A bounded ring of recent process readings, filled by the sampler that runs for
    /// as long as the server does.
    pub samples: std::sync::Arc<crate::host::History>,
}

/// Per-route request counters.
///
/// Routes are the matched path patterns, not the raw paths, so the map is bounded by
/// the number of routes this binary declares rather than by anything a caller sends.
#[derive(Default)]
pub struct HttpStats {
    pub success: AtomicU64,
    pub client_errors: AtomicU64,
    pub server_errors: AtomicU64,
    /// Requests that took at least a second.
    pub slow: AtomicU64,
    pub total_ms: AtomicU64,
    pub slowest_ms: AtomicU64,
    routes: std::sync::Mutex<std::collections::BTreeMap<String, RouteStats>>,
}

#[derive(Default, Clone, Copy)]
pub struct RouteStats {
    pub requests: u64,
    pub errors: u64,
    pub total_ms: u64,
}

impl HttpStats {
    pub fn routes(&self) -> Vec<(String, RouteStats)> {
        self.routes
            .lock()
            .map(|routes| {
                routes
                    .iter()
                    .map(|(name, stats)| (name.clone(), *stats))
                    .collect()
            })
            .unwrap_or_default()
    }
}

impl std::fmt::Debug for HttpStats {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HttpStats")
            .field("success", &self.success.load(Ordering::Relaxed))
            .field("clientErrors", &self.client_errors.load(Ordering::Relaxed))
            .field("serverErrors", &self.server_errors.load(Ordering::Relaxed))
            .finish()
    }
}

impl AppState {
    pub fn new(config: Config, credentials: Credentials, store: Store) -> Self {
        let capacity = config.limits.db_queue_capacity;
        Self {
            authority: None,
            account_id: None,
            legacy: true,
            original_uploads: Arc::new(Semaphore::new(
                (config.limits.upload_concurrency - config.limits.upload_concurrency / 2).max(1),
            )),
            thumbnail_uploads: Arc::new(Semaphore::new(
                (config.limits.upload_concurrency / 2).max(1),
            )),
            original_downloads: Arc::new(Semaphore::new(
                (config.limits.download_concurrency - config.limits.download_concurrency / 4)
                    .max(1),
            )),
            thumbnail_downloads: Arc::new(Semaphore::new(
                (config.limits.download_concurrency / 4).max(1),
            )),
            asset_metrics: Arc::new(crate::assets::Metrics::default()),
            publication: Arc::new(std::sync::Mutex::new(())),
            config: config.clone(),
            credentials,
            store,
            ready: AtomicBool::new(true),
            admission: Arc::new(Semaphore::new(capacity)),
            control_admission: Arc::new(Semaphore::new(config.limits.control_queue_capacity)),
            requests: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            http: HttpStats::default(),
            samples: std::sync::Arc::new(crate::host::History::default()),
            database_change_pages: AtomicU64::new(0),
            zone_change_pages: AtomicU64::new(0),
            metrics_admission: Arc::new(Semaphore::new(1)),
        }
    }
}

/// An administrator administers the deployment; it does not hold a photo library.
///
/// Every route that could read or write an account's photo namespace lives in the
/// `namespace` group below, and this middleware refuses an administrator session
/// before the handler runs. That is what makes "administrators cannot synchronize"
/// an enforced fact rather than a UI convention: the refusal covers the record,
/// resource, lease and console APIs alike, so there is no path by which an
/// administrator could place or read a photo.
async fn deny_admin_namespace(
    Extension(user): Extension<CurrentUser>,
    request: Request,
    next: Next,
) -> Response {
    if user.0.as_ref().is_some_and(|user| user.role.is_admin()) {
        return ApiError::new(
            StatusCode::FORBIDDEN,
            "ADMIN_CANNOT_SYNC",
            "An administrator account manages the deployment and has no photo library",
        )
        .into_response();
    }
    next.run(request).await
}

pub(crate) fn router(state: Arc<AppState>) -> Router {
    // Routes that touch an account's photo namespace, and therefore only an ordinary
    // account may reach.
    let namespace = Router::new()
        .route("/v1/snapshots", post(snapshot))
        .route("/v1/storage", get(assets::storage))
        .route("/v1/uploads/{id}", get(assets::session))
        .route("/v1/uploads/{id}/renew", post(assets::renew))
        .route("/v1/uploads/{id}/content", post(assets::upload))
        .route("/v1/assets/{id}", get(assets::describe))
        .route("/v1/assets/{id}/content", get(assets::download))
        .route("/v1/upload-leases", post(leases::acquire))
        .route("/v1/upload-leases/{id}/{operation}", post(leases::control))
        .route("/v1/operations/{id}", get(leases::status))
        .route("/v1/console/photos", get(console::photos))
        .route("/v1/console/photos/{id}", get(console::photo))
        .route("/v1/console/albums", get(console::albums))
        .route("/v1/console/timeline", get(console::timeline))
        .route("/v1/console/summary", get(console::summary))
        .route("/v1/{*operation}", any(native_unsupported))
        .route(
            "/database/1/{container}/{environment}/{database}/{*operation}",
            any(cloudkit),
        )
        .fallback(unknown)
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            deny_admin_namespace,
        ));

    let private = Router::new()
        .route("/v1/auth/logout", post(auth::logout))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/metrics", get(metrics::read))
        .route("/v1/admin/overview", get(admin::overview))
        .route("/v1/admin/logs", get(admin::logs))
        .route(
            "/v1/admin/users",
            get(admin::users).post(admin::create_user),
        )
        .route("/v1/admin/users/{id}/storage", get(admin::storage))
        .route("/v1/admin/users/{id}/quota", post(admin::set_quota))
        .route("/v1/admin/users/{id}/role", post(admin::set_role))
        .route("/v1/admin/users/{id}/password", post(admin::set_password))
        .route("/v1/admin/users/{id}/enabled", post(admin::set_enabled))
        .merge(namespace)
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .route("/v1/auth/login", post(auth::login))
        .merge(private)
        // The console shell carries no user data, so it is served without a session.
        // Everything it then fetches goes through the authenticated routes above.
        .merge(crate::web::router())
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, response_metadata))
}

/// Folds one completed request into the console's counters.
///
/// The route recorded is the matched pattern, which the router places in the request
/// extensions before this runs. Anything unmatched is folded into a single bucket, so
/// a caller cannot grow the map by sending arbitrary paths.
fn record_request(
    state: &Arc<AppState>,
    matched: Option<&str>,
    method: &str,
    request_path: &str,
    status: u16,
    duration_ms: u64,
) {
    let stats = &state.http;
    let errored = status >= 400;
    if errored {
        if status >= 500 {
            stats.server_errors.fetch_add(1, Ordering::Relaxed);
        } else {
            stats.client_errors.fetch_add(1, Ordering::Relaxed);
        }
    } else {
        stats.success.fetch_add(1, Ordering::Relaxed);
    }
    if duration_ms >= 1000 {
        stats.slow.fetch_add(1, Ordering::Relaxed);
    }
    stats.total_ms.fetch_add(duration_ms, Ordering::Relaxed);
    stats.slowest_ms.fetch_max(duration_ms, Ordering::Relaxed);

    let name = match matched {
        Some(pattern) => format!("{method} {pattern}"),
        // Not every route reports a matched pattern, so the raw path is folded into a
        // shape instead: `/v1/assets/<id>` and `/v1/assets/<other id>` are one row.
        None => format!("{method} {}", route_shape(request_path)),
    };
    if let Ok(mut routes) = stats.routes.lock() {
        if !routes.contains_key(&name) && routes.len() >= MAX_TRACKED_ROUTES {
            // A hard bound: however many distinct shapes arrive, the table cannot grow
            // without limit. Anything new from here lands in the same catch-all row.
            let name = format!("{method} (other)");
            let route = routes.entry(name).or_default();
            route.requests += 1;
            route.total_ms = route.total_ms.saturating_add(duration_ms);
            route.errors += u64::from(errored);
            return;
        }
        let route = routes.entry(name).or_default();
        route.requests += 1;
        route.total_ms = route.total_ms.saturating_add(duration_ms);
        route.errors += u64::from(errored);
    }
}

/// How many distinct route shapes the console will keep separate rows for.
const MAX_TRACKED_ROUTES: usize = 128;

/// Collapses a request path into a route shape, so identifiers do not each become
/// their own row. A segment that is a number, a UUID or a long opaque token becomes
/// `:id`; everything else is kept, because that is the part a reader recognizes.
fn route_shape(path: &str) -> String {
    let mut shape = String::with_capacity(path.len());
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            shape.push('/');
        }
        if segment.is_empty() {
            continue;
        }
        let opaque = (segment.len() >= 3 && segment.parse::<u64>().is_ok())
            || segment.len() >= 32
            || (segment.len() >= 16 && segment.bytes().all(|b| b.is_ascii_hexdigit() || b == b'-'));
        shape.push_str(if opaque { ":id" } else { segment });
    }
    if shape.is_empty() {
        "/".to_owned()
    } else {
        shape
    }
}

/// The account a request was authenticated as, carried back out of [`authenticate`].
///
/// `response_metadata` is the outermost layer, so it runs *before* the session is
/// resolved and cannot see the user directly — yet the `request completed` record it
/// writes is the one line an operator reads per request. This is a per-request cell,
/// not shared state: two concurrent requests never write to the same slot.
#[derive(Clone)]
struct ActingUser(Arc<OnceLock<String>>);

impl ActingUser {
    fn new() -> Self {
        Self(Arc::new(OnceLock::new()))
    }

    fn note(&self, id: &str) {
        // The first writer wins. Only `authenticate` writes, and it writes once.
        let _ = self.0.set(id.to_owned());
    }

    fn id(&self) -> Option<&str> {
        self.0.get().map(String::as_str)
    }
}

async fn response_metadata(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    state.requests.fetch_add(1, Ordering::Relaxed);
    let request_id = Uuid::new_v4();
    let route = request.uri().path().to_owned();
    // Health, readiness and metrics are polled continuously and must not drown the
    // operational stream, so probe traffic stays at debug.
    let probe = matches!(route.as_str(), "/healthz" | "/readyz" | "/v1/metrics");
    let started = Instant::now();
    // The matched route is read before the request is consumed.
    let matched = request
        .extensions()
        .get::<axum::extract::MatchedPath>()
        .map(|path| path.as_str().to_owned());
    let method = request.method().as_str().to_owned();
    let mut request = request;
    let acting_user = ActingUser::new();
    request.extensions_mut().insert(acting_user.clone());
    let mut response = next
        .run(request)
        .instrument(tracing::info_span!(
            "request",
            request_id = %request_id,
            method = %method,
            route = %route,
        ))
        .await;
    let duration_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    let error_code = response
        .extensions()
        .get::<error::ResponseError>()
        .map(|value| value.0);
    let status = response.status().as_u16();
    record_request(
        &state,
        matched.as_deref(),
        &method,
        route.as_str(),
        status,
        duration_ms,
    );
    let level = if probe {
        tracing::Level::DEBUG
    } else if status >= 500 {
        tracing::Level::ERROR
    } else if status >= 400 {
        tracing::Level::WARN
    } else {
        tracing::Level::INFO
    };
    log_at!(
        level,
        request_id = %request_id,
        method = %method,
        route = route,
        status,
        duration_ms,
        error_code,
        // Absent for a capability download and for anything that never reached
        // authentication; the field is skipped rather than logged empty.
        user_id = acting_user.id().unwrap_or(""),
        "request completed"
    );
    // Responses are private and live by default. A handler that deliberately set a
    // cache policy — the console's content-hashed bundles — keeps it; everything else,
    // including every API response, is explicitly not stored.
    if !response.headers().contains_key(header::CACHE_CONTROL) {
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    }
    response
        .headers_mut()
        .insert("x-request-id", request_id.to_string().parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let mut request = request;
    let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
    let token = headers
        .next()
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_owned);
    let duplicate = headers.next().is_some();
    let denied = || {
        let mut response = ApiError::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_REQUIRED",
            "A valid login session is required",
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        response
    };
    if duplicate {
        return denied();
    }
    if !state.ready.load(Ordering::Acquire) {
        return ApiError::from(crate::Error::Unavailable).into_response();
    }
    let Some(authority) = &state.authority else {
        return denied();
    };
    let selected = if let Some(token) = token.as_deref() {
        match authority.resolve(token).await {
            Ok(Some((context, user))) => Some((context, CurrentUser(Some(user)))),
            Ok(None) => match authority.legacy_allowed().await {
                Ok(true) if state.credentials.authenticate(token) => {
                    Some((state.clone(), CurrentUser(None)))
                }
                Ok(_) => None,
                Err(e) => return ApiError::from(e).into_response(),
            },
            Err(e) => return ApiError::from(e).into_response(),
        }
    } else if !request.headers().contains_key(header::AUTHORIZATION) {
        match authority.legacy_allowed().await {
            Ok(true)
                if request.uri().path().ends_with("/content")
                    && assets::capability(&state, &request).is_ok() =>
            {
                Some((state.clone(), CurrentUser(None)))
            }
            Ok(_) => None,
            Err(e) => return ApiError::from(e).into_response(),
        }
    } else {
        None
    };
    let Some((state, user)) = selected else {
        return denied();
    };
    if let Some(user) = &user.0 {
        state.requests.fetch_add(1, Ordering::Relaxed);
        // Handed to `response_metadata`, which cannot see the resolved session itself.
        if let Some(acting_user) = request.extensions().get::<ActingUser>() {
            acting_user.note(&user.id);
        }
    }
    request.extensions_mut().insert(state.clone());
    request.extensions_mut().insert(user);
    let monitoring = request.uri().path() == "/v1/metrics";
    if !monitoring && (!state.ready.load(Ordering::Acquire) || !state.store.accepting()) {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "Service is not ready",
        )
        .into_response();
    }
    let Ok(_permit) = (if monitoring {
        state.metrics_admission.clone()
    } else if request.uri().path().starts_with("/v1/upload-leases") {
        state.control_admission.clone()
    } else {
        state.admission.clone()
    })
    .try_acquire_owned() else {
        state.rejected.fetch_add(1, Ordering::Relaxed);
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVER_BUSY",
            "Request capacity is exhausted; retry later",
        )
        .into_response();
    };
    next.run(request).await
}

async fn health() -> Json<Value> {
    Json(json!({"status":"alive"}))
}

async fn ready(State(state): State<Arc<AppState>>) -> Response {
    if state.ready.load(Ordering::Acquire)
        && let Some(authority) = &state.authority
    {
        match authority.legacy_allowed().await {
            Ok(false) => return Json(json!({"status":"ready"})).into_response(),
            Ok(true) => {}
            Err(error) => return ApiError::from(error).into_response(),
        }
    }
    if !state.ready.load(Ordering::Acquire) || !state.store.accepting() {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "Service is not ready",
        )
        .into_response();
    }
    match tokio::time::timeout(Duration::from_secs(1), state.store.ping()).await {
        Ok(Ok(())) => Json(json!({"status":"ready"})).into_response(),
        Ok(Err(error)) => ApiError::from(error).into_response(),
        Err(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "Readiness check timed out",
        )
        .into_response(),
    }
}

async fn capabilities(
    Extension(state): Extension<Arc<AppState>>,
    Extension(user): Extension<CurrentUser>,
) -> Json<Value> {
    // An administrator has no photo library, so it is offered no synchronization
    // capability at all. Advertising them and then refusing the calls would leave a
    // client guessing which half of the contract to believe.
    let administrator = user.0.as_ref().is_some_and(|user| user.role.is_admin());
    let syncs = !administrator;
    Json(json!({
        "service": "syncserver", "version": env!("CARGO_PKG_VERSION"), "stage": "M4",
        "namespace": state.config.namespace, "zones": state.config.zones,
        "authentication":{"mode":if user.0.is_some(){"username_password"}else{"api_key"},"user":user.0,"resourceBearerRequired":user.0.is_some(),"sessionTTLSeconds":if administrator {crate::users::ADMIN_SESSION_TTL_SECONDS} else {crate::users::SESSION_TTL_SECONDS}},
        "publicBaseURL": state.config.public_base_url.as_str(),
        "databaseGeneration": state.store.metadata().database_generation,
        "schemaVersion": state.store.metadata().schema_version,
        "administrator": administrator,
        "capabilities": {
            "storageQuota": syncs, "storageUsageByMediaType": syncs, "zoneListing": syncs, "recordCrud": syncs, "snapshots": syncs,
            "incrementalChanges": syncs, "uploadLeases": syncs, "fencing": syncs, "uploadPhases": syncs, "operationStatusRead": syncs, "tokenCurrentGate": syncs, "queryBatch": syncs, "changeAliases": syncs, "assetUpload": syncs, "assetDownload": syncs,
            "operationalMetrics": true, "appleAuthentication": false, "sharing": false, "uploadResume": false, "photoPublication": syncs,
            "webConsole": syncs, "adminConsole": administrator
        },
        "limits": state.config.limits,
        "protocolVersion": 1, "checksumAlgorithm": "SHA256", "historyRetention": "all",
        "recordFieldTypes": ["STRING","INT64","DOUBLE","TIMESTAMP","BYTES","REFERENCE",
            "STRING_LIST","INT64_LIST","DOUBLE_LIST","TIMESTAMP_LIST","BYTES_LIST","REFERENCE_LIST","ASSETID"],
        "syncLimits": {"maxPage":crate::domain::sync::MAX_PAGE,"maxZones":crate::domain::sync::MAX_ZONES},
        "recordLimits": { "maxOperations": crate::domain::records::MAX_OPERATIONS,
            "maxRecordBytes": crate::domain::records::MAX_RECORD_BYTES, "maxResultBytes": crate::domain::records::MAX_RESULT_BYTES },
        "recovery": state.store.metadata().recovery,
        "metrics": metrics::snapshot(&state)
    }))
}

async fn cloudkit(
    Extension(state): Extension<Arc<AppState>>,
    Path((container, environment, database, operation)): Path<(String, String, String, String)>,
    request: Request,
) -> Response {
    if container != state.config.namespace.container {
        return unknown().await;
    }
    if environment != "production" || database != "private" {
        return ApiError::unsupported().into_response();
    }
    if !matches!(
        operation.as_str(),
        "zones/list"
            | "records/modify"
            | "records/lookup"
            | "records/query"
            | "changes/database"
            | "changes/zone"
            | "records/changes"
            | "zones/changes"
            | "internal/records/query/batch"
            | "assets/upload"
    ) {
        return if matches!(
            operation.as_str(),
            "records/query"
                | "records/lookup"
                | "records/modify"
                | "records/changes"
                | "changes/database"
                | "changes/zone"
                | "zones/changes"
                | "assets/upload"
                | "internal/records/query/batch"
        ) {
            ApiError::unsupported().into_response()
        } else {
            unknown().await
        };
    }
    if request.method() != Method::POST {
        let mut response = ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "METHOD_NOT_ALLOWED",
            "Use POST for record and zone operations",
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::ALLOW, "POST".parse().unwrap());
        return response;
    }
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    let lease = if matches!(operation.as_str(), "records/modify" | "assets/upload") {
        match leases::proof(request.headers()) {
            Ok(proof) => Some(proof),
            Err(error) => return error.into_response(),
        }
    } else {
        None
    };
    let key = if matches!(operation.as_str(), "records/modify" | "assets/upload") {
        match records::idempotency_key(request.headers()) {
            Ok(key) => key,
            Err(error) => return error.into_response(),
        }
    } else {
        None
    };
    let mut value = match read_json(request, state.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    if let Some(map) = value.as_object_mut() {
        if let Some(client) = map.remove("clientId")
            && !client
                .as_str()
                .is_some_and(crate::domain::records::identifier)
        {
            return ApiError::bad_request().into_response();
        }
        if let Some(remap) = map.remove("remapEnums")
            && !remap.is_boolean()
        {
            return ApiError::bad_request().into_response();
        }
    }
    if operation == "records/modify" {
        return records::modify(&state, value, key, lease).await;
    }
    if operation == "records/lookup" {
        return sync::read(&state, "records/lookup", value).await;
    }
    if operation == "assets/upload" {
        return assets::reserve(
            &state,
            value,
            match lease {
                Some(proof) => proof,
                None => return ApiError::bad_request().into_response(),
            },
            key,
        )
        .await;
    }
    if matches!(
        operation.as_str(),
        "records/query"
            | "changes/database"
            | "changes/zone"
            | "records/changes"
            | "zones/changes"
            | "internal/records/query/batch"
    ) {
        return sync::read(&state, &operation, value).await;
    }
    if !value.as_object().is_some_and(|map| map.is_empty()) {
        return ApiError::bad_request().into_response();
    }
    let zones: Vec<_> = state
        .config
        .zones
        .iter()
        .map(|name| json!({"zoneID":{"zoneName":name, "ownerRecordName":"_defaultOwner"}}))
        .collect();
    Json(json!({"zones":zones})).into_response()
}

async fn read_json(request: Request, limit: usize) -> std::result::Result<Value, ApiError> {
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|s| {
            s.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
    if !matches!(
        content_type.as_deref(),
        Some("application/json" | "text/plain")
    ) {
        return Err(ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Use application/json or text/plain containing JSON",
        ));
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(request.into_body(), limit),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return Err(ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "LIMIT_EXCEEDED",
                "JSON body limit exceeded or body could not be read",
            ));
        }
        Err(_) => {
            return Err(ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "REQUEST_TIMEOUT",
                "JSON body read timed out",
            ));
        }
    };
    let value: Value = match json::parse(&bytes) {
        Ok(value) => value,
        Err(_) => return Err(ApiError::bad_request()),
    };
    Ok(value)
}

async fn native_unsupported(Path(operation): Path<String>) -> Response {
    if operation == "snapshots"
        || operation.starts_with("uploads/")
        || operation.starts_with("assets/")
    {
        ApiError::unsupported().into_response()
    } else {
        unknown().await
    }
}
async fn unknown() -> Response {
    ApiError::new(
        StatusCode::NOT_FOUND,
        "UNKNOWN_ITEM",
        "Unknown endpoint or container",
    )
    .into_response()
}

async fn snapshot(Extension(state): Extension<Arc<AppState>>, request: Request) -> Response {
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    match read_json(request, state.config.limits.json_body_limit).await {
        Ok(value) => sync::read(&state, "snapshots", value).await,
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::route_shape;

    #[test]
    fn identifiers_collapse_into_one_route_shape() {
        assert_eq!(route_shape("/v1/assets/abc"), "/v1/assets/abc");
        // A UUID, a long digest and a number are all the same kind of thing to a
        // reader: a value in the position of a route parameter.
        assert_eq!(
            route_shape("/v1/assets/d95b30c9-8e57-4817-9ee3-f156a061d8d5/content"),
            "/v1/assets/:id/content"
        );
        assert_eq!(
            route_shape("/v1/assets/0123456789abcdef0123456789abcdef"),
            "/v1/assets/:id"
        );
        assert_eq!(route_shape("/v1/operations/42"), "/v1/operations/42");
        assert_eq!(route_shape("/v1/operations/4242"), "/v1/operations/:id");
        // The part a reader recognizes is kept.
        assert_eq!(
            route_shape("/v1/upload-leases/38d2e51f-6319-4400-bfcd-3875f1bcfdbb/phase"),
            "/v1/upload-leases/:id/phase"
        );
        assert_eq!(
            route_shape("/database/1/com.apple.photos.cloud/production/private/records/modify"),
            "/database/1/com.apple.photos.cloud/production/private/records/modify"
        );
        assert_eq!(route_shape("/"), "/");
    }
}
