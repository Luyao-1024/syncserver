//! Process-local operational snapshots. Reading these never enqueues database work.
use super::AppState;
use axum::Json;
use serde_json::{Value, json};
use std::sync::{Arc, atomic::Ordering};
use tokio::sync::Semaphore;

fn occupied(semaphore: &Semaphore, capacity: usize) -> usize {
    capacity - semaphore.available_permits()
}

pub(super) fn snapshot(state: &AppState) -> Value {
    let limits = &state.config.limits;
    json!({
        "requests": state.requests.load(Ordering::Relaxed),
        "requestRejections": state.rejected.load(Ordering::Relaxed),
        "database": state.store.metrics(),
        "databaseChangePages": state.database_change_pages.load(Ordering::Relaxed),
        "zoneChangePages": state.zone_change_pages.load(Ordering::Relaxed),
        "assetBytesRead": state.asset_metrics.bytes_read.load(Ordering::Relaxed),
        "assetBytesHashed": state.asset_metrics.bytes_hashed.load(Ordering::Relaxed),
        "uploadedBytes": state.asset_metrics.uploaded_bytes.load(Ordering::Relaxed),
        "httpAdmission": {
            "capacity": limits.db_queue_capacity,
            "active": occupied(&state.admission, limits.db_queue_capacity),
            "controlCapacity": limits.control_queue_capacity,
            "controlActive": occupied(&state.control_admission, limits.control_queue_capacity)
        },
        "transfers": {
            "originalUploads": occupied(&state.original_uploads, (limits.upload_concurrency - limits.upload_concurrency / 2).max(1)),
            "thumbnailUploads": occupied(&state.thumbnail_uploads, (limits.upload_concurrency / 2).max(1)),
            "originalDownloads": occupied(&state.original_downloads, (limits.download_concurrency - limits.download_concurrency / 4).max(1)),
            "thumbnailDownloads": occupied(&state.thumbnail_downloads, (limits.download_concurrency / 4).max(1))
        }
    })
}

pub(super) async fn read(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
) -> Json<Value> {
    Json(json!({
        "version": 1,
        "runId": state.store.metadata().recovery.run_id,
        "ready": state.ready.load(Ordering::Acquire) && state.store.accepting(),
        "recovery": state.store.metadata().recovery,
        "metrics": snapshot(&state)
    }))
}
