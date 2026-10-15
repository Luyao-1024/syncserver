mod assets;
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
    extract::{Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get, post},
};
use error::ApiError;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::Semaphore;
use uuid::Uuid;

pub(crate) struct AppState {
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
    requests: AtomicU64,
    rejected: AtomicU64,
    database_change_pages: AtomicU64,
    zone_change_pages: AtomicU64,
}

impl AppState {
    pub fn new(config: Config, credentials: Credentials, store: Store) -> Self {
        let capacity = config.limits.db_queue_capacity;
        Self {
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
            database_change_pages: AtomicU64::new(0),
            zone_change_pages: AtomicU64::new(0),
            metrics_admission: Arc::new(Semaphore::new(1)),
        }
    }
}

pub(crate) fn router(state: Arc<AppState>) -> Router {
    let private = Router::new()
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/metrics", get(metrics::read))
        .route("/v1/snapshots", post(snapshot))
        .route("/v1/uploads/{id}", get(assets::session))
        .route("/v1/uploads/{id}/renew", post(assets::renew))
        .route("/v1/uploads/{id}/content", post(assets::upload))
        .route("/v1/assets/{id}", get(assets::describe))
        .route("/v1/assets/{id}/content", get(assets::download))
        .route("/v1/upload-leases", post(leases::acquire))
        .route("/v1/upload-leases/{id}/{operation}", post(leases::control))
        .route("/v1/operations/{id}", get(leases::status))
        .route("/v1/{*operation}", any(native_unsupported))
        .route(
            "/database/1/{container}/{environment}/{database}/{*operation}",
            any(cloudkit),
        )
        .fallback(unknown)
        .route_layer(middleware::from_fn_with_state(state.clone(), authenticate));
    Router::new()
        .route("/healthz", get(health))
        .route("/readyz", get(ready))
        .merge(private)
        .with_state(state.clone())
        .layer(middleware::from_fn_with_state(state, response_metadata))
}

async fn response_metadata(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    state.requests.fetch_add(1, Ordering::Relaxed);
    let request_id = Uuid::new_v4();
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-request-id", request_id.to_string().parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    tracing::info!(%request_id, status = response.status().as_u16(), database = "private", "request completed");
    response
}

async fn authenticate(
    State(state): State<Arc<AppState>>,
    request: Request,
    next: Next,
) -> Response {
    let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
    let key = headers
        .next()
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));
    let valid_bearer = key.is_some_and(|key| state.credentials.authenticate(key));
    let valid_capability = !request.headers().contains_key(header::AUTHORIZATION)
        && request.uri().path().ends_with("/content")
        && assets::capability(&state, &request).is_ok();
    if headers.next().is_some() || (!valid_bearer && !valid_capability) {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_REQUIRED",
            "A valid Bearer API key is required",
        )
        .into_response();
    }
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

async fn capabilities(State(state): State<Arc<AppState>>) -> Json<Value> {
    Json(json!({
        "service": "syncserver", "version": env!("CARGO_PKG_VERSION"), "stage": "M4",
        "namespace": state.config.namespace, "zones": state.config.zones,
        "publicBaseURL": state.config.public_base_url.as_str(),
        "databaseGeneration": state.store.metadata().database_generation,
        "schemaVersion": state.store.metadata().schema_version,
        "capabilities": {
            "zoneListing": true, "recordCrud": true, "snapshots": true,
            "incrementalChanges": true, "uploadLeases": true, "fencing": true, "uploadPhases": true, "operationStatusRead": true, "tokenCurrentGate": true, "queryBatch": true, "changeAliases": true, "assetUpload": true, "assetDownload": true,
            "operationalMetrics": true, "appleAuthentication": false, "sharing": false, "uploadResume": false, "photoPublication": true
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
    State(state): State<Arc<AppState>>,
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

async fn snapshot(State(state): State<Arc<AppState>>, request: Request) -> Response {
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    match read_json(request, state.config.limits.json_body_limit).await {
        Ok(value) => sync::read(&state, "snapshots", value).await,
        Err(error) => error.into_response(),
    }
}
