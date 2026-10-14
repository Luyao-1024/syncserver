use super::{AppState, error::ApiError};
use crate::domain::records::ModifyRequest;
use axum::{
    Json,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::Value;

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
    match state.store.modify(request).await {
        Ok(result) => (
            StatusCode::from_u16(result.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(result.body),
        )
            .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
