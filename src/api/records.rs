use super::{AppState, error::ApiError};
use crate::domain::records::ModifyRequest;
use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use tracing::Level;

pub(super) fn idempotency_key(
    headers: &HeaderMap,
) -> std::result::Result<Option<String>, ApiError> {
    let mut values = headers.get_all("idempotency-key").iter();
    let key = values
        .next()
        .map(|v| {
            v.to_str()
                .map(str::to_owned)
                .map_err(|_| ApiError::bad_request())
        })
        .transpose()?;
    if values.next().is_some() {
        return Err(ApiError::bad_request());
    }
    Ok(key)
}
pub(super) async fn modify(
    state: &AppState,
    value: Value,
    key: Option<String>,
    lease: Option<crate::domain::leases::LeaseProof>,
) -> Response {
    let request = match serde_json::from_value::<ModifyRequest>(value) {
        Ok(request) => request,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    let mut request = match request.prepare(key) {
        Ok(request) => request,
        Err(error) => return ApiError::from(error).into_response(),
    };
    request.lease = lease;
    // Summarize before the request is consumed by the store.
    let planned = request.operations.len();
    let idempotent_replay = request.idempotency_key.is_some();
    let lease_bound = request.lease.is_some();
    let preview = crate::logging::payload_enabled().then(|| {
        request
            .operations
            .iter()
            .map(|operation| {
                format!(
                    "{} {}",
                    operation.operation_type,
                    operation.record_type.as_deref().unwrap_or("-")
                )
            })
            .collect::<Vec<_>>()
            .join(",")
    });
    match state.store.modify(request).await {
        Ok(result) => {
            let level = if result.status >= 500 {
                Level::ERROR
            } else if result.status >= 400 {
                Level::WARN
            } else {
                Level::INFO
            };
            log_at!(
                level,
                // `record_operations` is the count; the account is what makes it
                // answerable as "how much did this person change".
                user_id = state.account_id.as_deref().unwrap_or(""),
                record_operations = planned as u64,
                idempotent_replay,
                lease_bound,
                status = result.status,
                "record modification settled"
            );
            if let Some(preview) = preview.filter(|text| !text.is_empty()) {
                tracing::debug!(operation_preview = %preview, "record modification operands");
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
