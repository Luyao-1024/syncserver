mod error;

use crate::{Config, credentials::Credentials, store::Store};
use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{Method, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{any, get},
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
    requests: AtomicU64,
    rejected: AtomicU64,
}

impl AppState {
    pub fn new(config: Config, credentials: Credentials, store: Store) -> Self {
        let capacity = config.limits.db_queue_capacity;
        Self {
            config,
            credentials,
            store,
            ready: AtomicBool::new(true),
            admission: Arc::new(Semaphore::new(capacity)),
            requests: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
        }
    }
}

pub(crate) fn router(state: Arc<AppState>) -> Router {
    let private = Router::new()
        .route("/v1/capabilities", get(capabilities))
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
    if headers.next().is_some() || !key.is_some_and(|key| state.credentials.authenticate(key)) {
        return ApiError::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_REQUIRED",
            "A valid Bearer API key is required",
        )
        .into_response();
    }
    if !state.ready.load(Ordering::Acquire) || !state.store.accepting() {
        return ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVICE_UNAVAILABLE",
            "Service is not ready",
        )
        .into_response();
    }
    let Ok(_permit) = state.admission.clone().try_acquire_owned() else {
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
        "service": "syncserver", "version": env!("CARGO_PKG_VERSION"), "stage": "M1",
        "namespace": state.config.namespace, "zones": state.config.zones,
        "publicBaseURL": state.config.public_base_url.as_str(),
        "databaseGeneration": state.store.metadata().database_generation,
        "schemaVersion": state.store.metadata().schema_version,
        "capabilities": {
            "zoneListing": true, "recordCrud": false, "snapshots": false,
            "incrementalChanges": false, "assetUpload": false, "assetDownload": false,
            "appleAuthentication": false, "sharing": false, "uploadResume": false
        },
        // Asset/upload limits are configured reservations for M4, not enabled APIs.
        "limits": state.config.limits,
        "recovery": state.store.metadata().recovery,
        "metrics": { "requests": state.requests.load(Ordering::Relaxed),
            "requestRejections": state.rejected.load(Ordering::Relaxed),
            "database": state.store.metrics(), "assetBytesRead": 0, "assetBytesHashed": 0 }
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
    if operation != "zones/list" {
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
            "Use POST for zones/list",
        )
        .into_response();
        response
            .headers_mut()
            .insert(header::ALLOW, "POST".parse().unwrap());
        return response;
    }
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
        return ApiError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "UNSUPPORTED_MEDIA_TYPE",
            "Use application/json or text/plain containing JSON",
        )
        .into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(30),
        to_bytes(request.into_body(), state.config.limits.json_body_limit),
    )
    .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return ApiError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "LIMIT_EXCEEDED",
                "JSON body limit exceeded or body could not be read",
            )
            .into_response();
        }
        Err(_) => {
            return ApiError::new(
                StatusCode::REQUEST_TIMEOUT,
                "REQUEST_TIMEOUT",
                "JSON body read timed out",
            )
            .into_response();
        }
    };
    let value: Value = match serde_json::from_slice(&bytes) {
        Ok(value) => value,
        Err(_) => return ApiError::bad_request().into_response(),
    };
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
