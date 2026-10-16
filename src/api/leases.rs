use super::{AppState, error::ApiError};
use crate::domain::{
    leases::{Acquire, Check, Control, LeaseProof, Phase},
    records::identifier,
};
use axum::{
    Json,
    extract::{Path, Request},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use std::sync::Arc;
pub(super) fn proof(headers: &HeaderMap) -> Result<LeaseProof, ApiError> {
    let one = |name: &str| -> Result<String, ApiError> {
        let mut values = headers.get_all(name).iter();
        let value = values
            .next()
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| ApiError::from(crate::store::lease_lost()))?;
        if values.next().is_some() {
            return Err(ApiError::bad_request());
        }
        if value.len() > crate::domain::sync::MAX_TOKEN_BYTES {
            return Err(ApiError::bad_request());
        }
        Ok(value.to_owned())
    };
    let proof = LeaseProof {
        id: one("x-sync-lease-id")?,
        token: one("x-sync-lease-token")?,
        fence: one("x-sync-lease-fence")?,
    };
    if uuid::Uuid::parse_str(&proof.id).is_err()
        || proof
            .fence
            .parse::<u64>()
            .ok()
            .is_none_or(|v| v.to_string() != proof.fence)
    {
        return Err(ApiError::bad_request());
    }
    Ok(proof)
}
pub(super) async fn result(state: &AppState, request: Control) -> Response {
    let operation = match &request {
        Control::Acquire(_) => "acquire",
        Control::Check(..) => "check",
        Control::Phase(..) => "phase",
        Control::Renew(_) => "renew",
        Control::Release(_) => "release",
        Control::Status(_) => "status",
    };
    match state.store.control(request).await {
        Ok(result) => {
            let error_code = result.body.get("serverErrorCode").and_then(Value::as_str);
            match error_code {
                // Losing exclusive uplink ownership is an operational event, not merely
                // a rejected request: a whole client round stops here.
                Some("UPLOAD_LEASE_LOST") | Some("UPLOAD_PHASE_MISMATCH") => {
                    tracing::warn!(
                        lease_operation = operation,
                        error_code,
                        "upload lease ownership ended for this round"
                    )
                }
                Some(code) => tracing::debug!(
                    lease_operation = operation,
                    error_code = code,
                    status = result.status,
                    "upload lease control rejected"
                ),
                None if matches!(operation, "acquire" | "release") => tracing::info!(
                    lease_operation = operation,
                    status = result.status,
                    "upload lease ownership changed"
                ),
                None => tracing::debug!(
                    lease_operation = operation,
                    status = result.status,
                    "upload lease control completed"
                ),
            }
            (
                StatusCode::from_u16(result.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
                Json(result.body),
            )
                .into_response()
        }
        Err(error) => ApiError::from(error).into_response(),
    }
}
pub(super) async fn acquire(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    let value = match super::read_json(request, state.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    match serde_json::from_value::<Acquire>(value) {
        Ok(request) => result(&state, Control::Acquire(request)).await,
        Err(_) => ApiError::bad_request().into_response(),
    }
}
pub(super) async fn control(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path((id, operation)): Path<(String, String)>,
    request: Request,
) -> Response {
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    let proof = match proof(request.headers()) {
        Ok(proof) if proof.id == id => proof,
        Ok(_) => return ApiError::bad_request().into_response(),
        Err(error) => return error.into_response(),
    };
    let value = match super::read_json(request, state.config.limits.json_body_limit).await {
        Ok(value) => value,
        Err(error) => return error.into_response(),
    };
    let command = match operation.as_str() {
        "check" => serde_json::from_value::<Check>(value).map(|r| Control::Check(proof, r)),
        "phase" => serde_json::from_value::<Phase>(value).map(|r| Control::Phase(proof, r)),
        "renew" | "release" => {
            if value != serde_json::json!({}) {
                return ApiError::bad_request().into_response();
            }
            return result(
                &state,
                if operation == "renew" {
                    Control::Renew(proof)
                } else {
                    Control::Release(proof)
                },
            )
            .await;
        }
        _ => return ApiError::unsupported().into_response(),
    };
    match command {
        Ok(command) => result(&state, command).await,
        Err(_) => ApiError::bad_request().into_response(),
    }
}
pub(super) async fn status(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    if request.uri().query().is_some() || !identifier(&id) {
        return ApiError::bad_request().into_response();
    }
    result(&state, Control::Status(id)).await
}
