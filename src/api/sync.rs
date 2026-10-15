use super::{AppState, error::ApiError};
use crate::domain::{
    records::LookupRequest,
    sync::{BatchRequest, DatabaseRequest, QueryRequest, Read, SnapshotRequest, ZonesRequest},
};
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use serde_json::Value;
use std::sync::atomic::Ordering;
fn prepare(operation: &str, value: Value) -> Result<Read, ApiError> {
    let malformed = |_| ApiError::bad_request();
    let request = match operation {
        "internal/records/query/batch" => {
            Read::Batch(serde_json::from_value::<BatchRequest>(value).map_err(malformed)?)
        }
        "snapshots" => {
            Read::Snapshot(serde_json::from_value::<SnapshotRequest>(value).map_err(malformed)?)
        }
        "records/query" => {
            Read::Query(serde_json::from_value::<QueryRequest>(value).map_err(malformed)?)
        }
        "records/lookup" => Read::Lookup(
            serde_json::from_value::<LookupRequest>(value)
                .map_err(malformed)?
                .prepare()
                .map_err(ApiError::from)?,
        ),
        "changes/zone" | "records/changes" | "zones/changes" => {
            Read::Zones(serde_json::from_value::<ZonesRequest>(value).map_err(malformed)?)
        }
        "changes/database" => {
            Read::Database(serde_json::from_value::<DatabaseRequest>(value).map_err(malformed)?)
        }
        _ => return Err(ApiError::unsupported()),
    };
    request.validate().map_err(ApiError::from)?;
    Ok(request)
}
pub(super) async fn read(state: &AppState, operation: &str, value: Value) -> Response {
    let request = match prepare(operation, value) {
        Ok(request) => request,
        Err(error) => return error.into_response(),
    };
    match state
        .store
        .read(request, state.credentials.tokens.clone())
        .await
    {
        Ok(result) => {
            if result.status == 200 {
                match operation {
                    "changes/database" => {
                        state.database_change_pages.fetch_add(1, Ordering::Relaxed);
                    }
                    "changes/zone" | "records/changes" | "zones/changes" => {
                        let pages = result.body["zones"].as_array().map_or(0, |zones| {
                            zones
                                .iter()
                                .filter(|zone| zone.get("serverErrorCode").is_none())
                                .count()
                        });
                        state
                            .zone_change_pages
                            .fetch_add(pages as u64, Ordering::Relaxed);
                    }
                    _ => {}
                }
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
