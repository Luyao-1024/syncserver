//! End-to-end protocol tests against a mock of the syncserver wire shapes.
//!
//! The mock is intentionally strict about the parts that define correctness:
//! uploads must match their reserved digest, lease headers must be present, and
//! conditional mutations must target the records they were built from.

use axum::extract::{Request, State};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use sync_client::{
    AssetRole, AssetUpload, Client, Connection, DownloadOptions, OperationState,
    PhotoContentUpdate, PhotoMetadata, PhotoUpload, UploadLease, UploadPhase, ZoneChangesRequest,
};
use uuid::Uuid;

const CONTAINER: &str = "com.apple.photos.cloud";
const ZONE: &str = "PrimarySync";

#[derive(Clone)]
struct Session {
    field: String,
    expected_size: u64,
    expected_hex: String,
}

#[derive(Clone)]
struct Asset {
    bytes: Vec<u8>,
    mime: String,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Default)]
struct Mock {
    origin: Mutex<String>,
    sessions: Mutex<HashMap<String, Session>>,
    session_counter: Mutex<u64>,
    assets: Mutex<HashMap<String, Asset>>,
    records: Mutex<HashMap<String, Value>>,
    query_calls: Mutex<HashMap<String, usize>>,
    phase: Mutex<String>,
    revision: Mutex<u64>,
    modify_keys: Mutex<Vec<String>>,
}

impl Mock {
    fn revision(&self) -> String {
        let mut revision = self.revision.lock().unwrap();
        *revision += 1;
        format!("rev-{}", *revision)
    }
}

fn zone_json() -> Value {
    json!({"zoneName": ZONE, "ownerRecordName": "_defaultOwner"})
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn checksum(bytes: &[u8]) -> String {
    STANDARD.encode(Sha256::digest(bytes))
}

fn descriptor_json(mock: &Mock, id: &str, asset: &Asset, role: &str) -> Value {
    let origin = mock.origin.lock().unwrap().clone();
    let mut value = json!({
        "assetId": id,
        "size": asset.bytes.len(),
        "fileChecksum": checksum(&asset.bytes),
        "checksumAlgorithm": "SHA256",
        "mimeType": asset.mime,
        "downloadURL": format!("{origin}/v1/assets/{id}/content?role={role}&token=cap"),
        "receipt": format!("receipt-{id}"),
    });
    if let Some(width) = asset.width {
        value["width"] = json!(width);
    }
    if let Some(height) = asset.height {
        value["height"] = json!(height);
    }
    value
}

fn error_response(status: u16, code: &str, reason: &str) -> Response {
    (
        axum::http::StatusCode::from_u16(status).unwrap(),
        Json(json!({"serverErrorCode": code, "reason": reason})),
    )
        .into_response()
}

async fn handle(State(mock): State<Arc<Mock>>, request: Request) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let path = uri.path().to_string();
    let headers = request.headers().clone();
    let body = axum::body::to_bytes(request.into_body(), 16 * 1024 * 1024)
        .await
        .unwrap_or_default();
    let value: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let origin = mock.origin.lock().unwrap().clone();

    match (method.as_str(), path.as_str()) {
        ("POST", "/v1/auth/login") => {
            assert_eq!(value["username"], "alice");
            assert_eq!(value["password"], "secret");
            Json(json!({
                "accessToken": "session-token",
                "tokenType": "Bearer",
                "expiresAt": 9999999999i64,
                "user": {"id": "user-1", "username": "alice", "role": "admin"},
            }))
            .into_response()
        }
        ("POST", "/v1/auth/logout") => {
            assert!(headers.contains_key("authorization"));
            axum::http::StatusCode::NO_CONTENT.into_response()
        }
        ("GET", "/readyz") => Json(json!({"status": "ready"})).into_response(),
        ("GET", "/v1/capabilities") => Json(capabilities(&origin)).into_response(),
        ("POST", "/v1/snapshots") => {
            assert!(headers.contains_key("authorization"));
            Json(json!({"zoneID": zone_json(), "snapshotToken": "snap-1", "syncToken": "sync-7"}))
                .into_response()
        }
        ("POST", "/v1/upload-leases") => {
            assert_eq!(value["deviceID"], "device-1");
            assert_eq!(value["zoneIDs"][0]["zoneName"], ZONE);
            *mock.phase.lock().unwrap() = "checking".into();
            Json(json!({
                "leaseId": "lease-1",
                "leaseToken": "lease-token",
                "fencingToken": "1",
                "expiresAtMs": 9999999999i64,
                "ttlSeconds": 900,
                "heartbeatSeconds": 450,
                "phase": "checking",
                "zoneIDs": [zone_json()],
            }))
            .into_response()
        }
        _ if path.starts_with("/v1/upload-leases/") => {
            for header in [
                "x-sync-lease-id",
                "x-sync-lease-token",
                "x-sync-lease-fence",
            ] {
                assert!(headers.contains_key(header), "missing {header}");
            }
            assert_eq!(headers["x-sync-lease-id"], "lease-1");
            assert_eq!(headers["x-sync-lease-fence"], "1");
            if path.ends_with("/check") {
                assert_eq!(value["zoneTokens"][0]["syncToken"], "sync-0");
                Json(json!({
                    "isCurrent": true,
                    "mayUpload": true,
                    "phase": "delete",
                    "zoneTokens": [{"zoneID": zone_json(), "syncToken": "sync-0"}],
                }))
                .into_response()
            } else if path.ends_with("/phase") {
                let phase = value["phase"].as_str().unwrap();
                *mock.phase.lock().unwrap() = phase.to_string();
                Json(json!({"phase": phase})).into_response()
            } else if path.ends_with("/renew") {
                Json(json!({
                    "leaseId": "lease-1",
                    "expiresAtMs": 9999999999i64,
                    "ttlSeconds": 900,
                }))
                .into_response()
            } else if path.ends_with("/release") {
                Json(json!({"released": true})).into_response()
            } else {
                error_response(404, "NOT_FOUND", "unknown lease operation")
            }
        }
        _ if path.ends_with("/assets/upload") => {
            assert!(headers.contains_key("idempotency-key"));
            let tokens = value["tokens"].as_array().cloned().unwrap_or_default();
            assert_eq!(value["zoneID"]["zoneName"], ZONE);
            let mut responses = Vec::new();
            for token in tokens {
                let mut counter = mock.session_counter.lock().unwrap();
                *counter += 1;
                let session_id = format!("session-{counter}");
                drop(counter);
                let options = &token["_syncserver"];
                let expected_hex = options["expectedSha256"].as_str().unwrap().to_string();
                let expected_size = options["expectedSize"].as_u64().unwrap();
                assert_eq!(expected_hex.len(), 64);
                mock.sessions.lock().unwrap().insert(
                    session_id.clone(),
                    Session {
                        field: token["fieldName"].as_str().unwrap().to_string(),
                        expected_size,
                        expected_hex,
                    },
                );
                responses.push(json!({
                    "recordName": token["recordName"],
                    "fieldName": token["fieldName"],
                    "url": format!("{origin}/v1/uploads/{session_id}/content?expires=1&token=cap"),
                    "_syncserver": {"sessionId": session_id, "state": "Reserved"},
                }));
            }
            Json(json!({"tokens": responses})).into_response()
        }
        ("POST", _) if path.starts_with("/v1/uploads/") && path.ends_with("/content") => {
            assert!(headers.contains_key("authorization"));
            assert!(headers.contains_key("x-sync-lease-fence"));
            let session_id = path.split('/').nth(3).unwrap().to_string();
            let session = mock.sessions.lock().unwrap().get(&session_id).cloned();
            let Some(session) = session else {
                return error_response(404, "NOT_FOUND", "unknown session");
            };
            let bytes = body.to_vec();
            if bytes.len() as u64 != session.expected_size
                || hex(&Sha256::digest(&bytes)) != session.expected_hex
            {
                return error_response(
                    409,
                    "UPLOAD_CHECKSUM_MISMATCH",
                    "bytes do not match the reservation",
                );
            }
            let role = if session.field == "resJPEGThumbRes" {
                "thumbnail"
            } else {
                "original"
            };
            let id = Uuid::new_v4().to_string();
            let asset = Asset {
                bytes,
                mime: if role == "original" {
                    "image/heic".into()
                } else {
                    "image/jpeg".into()
                },
                width: if role == "original" { Some(4) } else { None },
                height: if role == "original" { Some(3) } else { None },
            };
            let descriptor = descriptor_json(&mock, &id, &asset, role);
            mock.assets.lock().unwrap().insert(id, asset);
            Json(json!({"singleFile": descriptor})).into_response()
        }
        _ if path.starts_with("/v1/uploads/") => {
            error_response(404, "NOT_FOUND", "unknown upload operation")
        }
        ("GET", _) if path.starts_with("/v1/assets/") && path.ends_with("/content") => {
            assert!(headers.contains_key("authorization"));
            let id = path.split('/').nth(3).unwrap();
            let asset = mock.assets.lock().unwrap().get(id).cloned();
            match asset {
                Some(asset) => (
                    [
                        (
                            "etag",
                            format!("\"sha256:{}\"", hex(&Sha256::digest(&asset.bytes))),
                        ),
                        ("accept-ranges", "bytes".into()),
                    ],
                    asset.bytes,
                )
                    .into_response(),
                None => error_response(404, "NOT_FOUND", "unknown asset"),
            }
        }
        ("GET", _) if path.starts_with("/v1/assets/") => {
            assert!(headers.contains_key("authorization"));
            let id = path.split('/').nth(3).unwrap();
            let role = uri
                .query()
                .and_then(|query| query.split('&').find_map(|pair| pair.strip_prefix("role=")))
                .unwrap_or("original");
            let asset = mock.assets.lock().unwrap().get(id).cloned();
            match asset {
                Some(asset) => Json(descriptor_json(&mock, id, &asset, role)).into_response(),
                None => error_response(404, "NOT_FOUND", "unknown asset"),
            }
        }
        ("GET", "/v1/storage") => Json(json!({
            "quotaBytes": 1_000_000u64,
            "usedBytes": 4_096u64,
            "reservedBytes": 0u64,
            "availableBytes": 995_904u64,
            "imageBytes": 4_096u64,
            "videoBytes": 0u64,
            "otherBytes": 0u64,
        }))
        .into_response(),
        ("GET", _) if path.starts_with("/v1/operations/") => {
            let id = path.trim_start_matches("/v1/operations/");
            if id == "missing" {
                return error_response(404, "UNKNOWN_ITEM", "no receipt");
            }
            Json(json!({"state": "Committed", "result": {"received": id}, "items": []}))
                .into_response()
        }
        ("POST", _) if path.ends_with("/records/query") => {
            let record_type = value["query"]["recordType"].as_str().unwrap();
            let mut calls = mock.query_calls.lock().unwrap();
            let count = calls.entry(record_type.to_string()).or_insert(0);
            *count += 1;
            let first = *count == 1;
            drop(calls);
            let records = mock.records.lock().unwrap();
            let matching: Vec<Value> = records
                .values()
                .filter(|record| record["recordType"] == record_type)
                .cloned()
                .collect();
            let (records, marker) = if record_type == "CPLMaster" && first {
                (matching, Some("master-page-2"))
            } else if record_type == "CPLMaster" {
                (Vec::new(), None)
            } else {
                (matching, None)
            };
            let mut body = json!({"records": records, "syncToken": "sync-7"});
            if let Some(marker) = marker {
                body["continuationMarker"] = json!(marker);
            }
            Json(body).into_response()
        }
        ("POST", _) if path.ends_with("/records/lookup") => {
            let records = mock.records.lock().unwrap();
            let results: Vec<Value> = value["records"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .map(|entry| {
                    let name = entry["recordName"].as_str().unwrap();
                    records.get(name).cloned().unwrap_or_else(|| {
                        json!({
                            "serverErrorCode": "UNKNOWN_ITEM",
                            "reason": "not found",
                            "recordName": name,
                            "zoneID": zone_json(),
                        })
                    })
                })
                .collect();
            Json(json!({"records": results, "syncToken": "sync-7"})).into_response()
        }
        ("POST", _) if path.ends_with("/changes/database") => Json(json!({
            "zones": [{"zoneID": zone_json(), "deleted": false}],
            "moreComing": false,
            "syncToken": "db-1",
        }))
        .into_response(),
        ("POST", _) if path.ends_with("/changes/zone") => {
            assert_eq!(value["zones"][0]["syncToken"], "db-1");
            let records: Vec<Value> = mock
                .records
                .lock()
                .unwrap()
                .values()
                .filter(|record| record["recordType"] == "CPLAsset")
                .cloned()
                .collect();
            Json(json!({
                "zones": [{
                    "zoneID": zone_json(),
                    "records": records,
                    "syncToken": "sync-7",
                    "moreComing": false,
                }],
            }))
            .into_response()
        }
        ("POST", _) if path.ends_with("/records/modify") => {
            if let Some(key) = headers.get("idempotency-key") {
                mock.modify_keys
                    .lock()
                    .unwrap()
                    .push(key.to_str().unwrap().to_string());
            }
            let mut stored = mock.records.lock().unwrap();
            let mut results = Vec::new();
            for operation in value["operations"].as_array().cloned().unwrap_or_default() {
                let kind = operation["operationType"].as_str().unwrap();
                let record = &operation["record"];
                let name = record["recordName"].as_str().unwrap().to_string();
                match kind {
                    "create" => {
                        let mut created = record.clone();
                        created["recordChangeTag"] = json!(mock.revision());
                        created["zoneID"] = zone_json();
                        stored.insert(name.clone(), created.clone());
                        results.push(created);
                    }
                    "update" => {
                        let mut current = stored.get(&name).cloned().unwrap_or_else(|| {
                            json!({
                                "recordName": name,
                                "recordType": "CPLAsset",
                                "zoneID": zone_json(),
                                "fields": {},
                            })
                        });
                        let fields = current["fields"].as_object_mut().unwrap();
                        if let Some(patch) = record["fields"].as_object() {
                            for (field, value) in patch {
                                if value.is_null() {
                                    fields.remove(field);
                                } else {
                                    fields.insert(field.clone(), value.clone());
                                }
                            }
                        }
                        current["recordChangeTag"] = json!(mock.revision());
                        stored.insert(name.clone(), current.clone());
                        results.push(current);
                    }
                    "delete" => {
                        stored.remove(&name);
                        results.push(json!({
                            "recordName": name,
                            "recordType": null,
                            "zoneID": zone_json(),
                            "recordChangeTag": mock.revision(),
                            "deleted": true,
                            "_syncserver": {},
                        }));
                    }
                    other => panic!("unexpected operation {other}"),
                }
            }
            Json(json!({"records": results})).into_response()
        }
        _ => error_response(404, "NOT_FOUND", &format!("{method} {path}")),
    }
}

fn capabilities(origin: &str) -> Value {
    json!({
        "service": "syncserver",
        "version": "0.1.0",
        "stage": "M4",
        "namespace": {
            "tenant": "local",
            "container": CONTAINER,
            "environment": "production",
            "database": "private",
        },
        "zones": [ZONE],
        "publicBaseURL": format!("{origin}/"),
        "databaseGeneration": "501855bb-18d3-4008-abc3-37e4948bf6f0",
        "schemaVersion": 4,
        "authentication": {
            "mode": "account",
            "user": {"id": "user-1", "username": "alice", "role": "admin"},
            "resourceBearerRequired": true,
            "sessionTtlSeconds": 3600,
        },
        "capabilities": {
            "zoneListing": true, "recordCrud": true, "snapshots": true,
            "incrementalChanges": true, "uploadLeases": true, "fencing": true,
            "tokenCurrentGate": true, "uploadPhases": true,
            "operationStatusRead": true, "assetUpload": true,
            "assetDownload": true, "photoPublication": true,
            "storageQuota": true, "storageUsageByMediaType": true,
            "uploadResume": false, "sharing": false, "appleAuthentication": false,
        },
        "limits": {
            "maxAssetBytes": 1_048_576u64,
            "userStorageQuotaBytes": 1_000_000u64,
            "thumbnailMaxBytes": 131_072u64,
            "uploadConcurrency": 2,
            "downloadConcurrency": 2,
            "uploadIdleTimeoutSecs": 30,
            "uploadTotalTimeoutSecs": 120,
            "dbQueueCapacity": 8,
            "jsonBodyLimit": 8_388_608,
            "leaseTtlSecs": 900,
            "leaseHeartbeatSecs": 450,
            "controlQueueCapacity": 8,
        },
        "protocolVersion": 1,
        "checksumAlgorithm": "SHA256",
        "historyRetention": "all",
        "recordFieldTypes": ["STRING", "INT64", "TIMESTAMP", "REFERENCE", "ASSETID"],
        "syncLimits": {"maxPage": 200, "maxZones": 20},
        "recordLimits": {"maxOperations": 400, "maxRecordBytes": 1_000_000, "maxResultBytes": 8_000_000},
    })
}

async fn start_mock() -> (Arc<Mock>, String) {
    let mock = Arc::new(Mock::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let origin = format!("http://{address}");
    *mock.origin.lock().unwrap() = origin.clone();
    let app = Router::new().fallback(handle).with_state(Arc::clone(&mock));
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (mock, origin)
}

async fn login_and_connect(origin: &str) -> (Client, Connection) {
    let client = Client::builder(origin).unwrap().build().unwrap();
    client.ready().await.unwrap();
    let session = client.login("alice", "secret").await.unwrap();
    assert_eq!(session.access_token, "session-token");
    assert_eq!(session.user.username, "alice");
    assert!(!session.is_expired(1));
    let connection = client.connect().await.unwrap();
    (client, connection)
}

async fn lease_at(
    connection: &Connection,
    zone: &sync_client::ZoneId,
    phase: UploadPhase,
) -> UploadLease {
    let lease = UploadLease::acquire(connection, "device-1", zone)
        .await
        .unwrap();
    let check = lease.check("sync-0", true).await.unwrap();
    assert!(check.is_current);
    assert!(check.may_upload);
    assert_eq!(check.phase, "delete");
    while lease.phase() != Some(phase) {
        lease.advance_phase().await.unwrap();
    }
    lease
}

fn stable_descriptor(bytes: &[u8], mime: &str, id: &str) -> Value {
    json!({
        "assetId": id,
        "size": bytes.len(),
        "fileChecksum": checksum(bytes),
        "checksumAlgorithm": "SHA256",
        "mimeType": mime,
        "width": 4,
        "height": 3,
    })
}

fn fixture_records() -> (Value, Value) {
    let original = b"fixture-original";
    let thumbnail = b"fixture-thumbnail";
    let original_id = Uuid::new_v4().to_string();
    let thumbnail_id = Uuid::new_v4().to_string();
    let generation = Uuid::new_v4().to_string();
    let master = json!({
        "recordName": "photo-1-master",
        "recordType": "CPLMaster",
        "zoneID": zone_json(),
        "recordChangeTag": "master-tag",
        "fields": {
            "photoId": {"type": "STRING", "value": "photo-1"},
            "contentGeneration": {"type": "STRING", "value": generation},
            "thumbnailSourceOriginalSHA256": {"type": "STRING", "value": hex(&Sha256::digest(original))},
            "resOriginalRes": {"type": "ASSETID", "value": stable_descriptor(original, "image/heic", &original_id)},
            "resJPEGThumbRes": {"type": "ASSETID", "value": stable_descriptor(thumbnail, "image/jpeg", &thumbnail_id)},
        },
        "_syncserver": {},
    });
    let asset = json!({
        "recordName": "photo-1",
        "recordType": "CPLAsset",
        "zoneID": zone_json(),
        "recordChangeTag": "asset-tag",
        "fields": {
            "masterRef": {"type": "REFERENCE", "value": {"recordName": "photo-1-master", "action": "NONE"}},
            "contentGeneration": {"type": "STRING", "value": generation},
            "relativePath": {"type": "STRING", "value": "2024/IMG_0001.HEIC"},
            "isFavorite": {"type": "INT64", "value": 0},
            "isHidden": {"type": "INT64", "value": 0},
            "assetDate": {"type": "TIMESTAMP", "value": 1_700_000_000_000i64},
        },
        "_syncserver": {},
    });
    (master, asset)
}

#[tokio::test]
async fn authentication_bootstrap_changes_and_downloads() {
    let (mock, origin) = start_mock().await;
    let (master, asset) = fixture_records();
    mock.records
        .lock()
        .unwrap()
        .insert("photo-1-master".into(), master);
    mock.records.lock().unwrap().insert("photo-1".into(), asset);

    let (client, connection) = login_and_connect(&origin).await;
    assert_eq!(connection.scope().namespace.container, CONTAINER);

    let bootstrap = connection
        .bootstrap(&connection.primary_zone().unwrap())
        .await
        .unwrap();
    assert_eq!(bootstrap.photos.len(), 1);
    assert_eq!(bootstrap.photos[0].relative_path, "2024/IMG_0001.HEIC");
    assert!(!bootstrap.photos[0].is_favorite);
    assert_eq!(bootstrap.sync_token, "sync-7");
    // The master query paged twice; the asset query once.
    assert_eq!(mock.query_calls.lock().unwrap()["CPLMaster"], 2);

    let zone = connection.primary_zone().unwrap();
    let photo = connection.fetch_photo(&zone, "photo-1").await.unwrap();
    assert_eq!(photo.master_id, "photo-1-master");
    assert_eq!(photo.metadata().relative_path, "2024/IMG_0001.HEIC");

    let database = connection.changes_database(None, None).await.unwrap();
    assert_eq!(database.zones.len(), 1);
    assert_eq!(database.sync_token, "db-1");
    let page = connection
        .changes_zone(&ZoneChangesRequest::new(zone).token("db-1"))
        .await
        .unwrap();
    assert_eq!(page.records.len(), 1);
    assert_eq!(page.sync_token, "sync-7");

    let usage = connection.storage_usage().await.unwrap();
    assert_eq!(usage.quota_bytes, 1_000_000);
    assert_eq!(usage.image_bytes, 4_096);

    assert!(matches!(
        connection.operation_status("op-1").await.unwrap(),
        OperationState::Committed(_)
    ));
    assert_eq!(
        connection.operation_status("missing").await.unwrap(),
        OperationState::Unknown
    );

    client.logout().await.unwrap();
    assert!(client.access_token().is_none());
}

#[tokio::test]
async fn photo_lifecycle_reserves_uploads_mutates_and_deletes() {
    let (mock, origin) = start_mock().await;
    let (_client, connection) = login_and_connect(&origin).await;
    let zone = connection.primary_zone().unwrap();

    let lease = lease_at(&connection, &zone, UploadPhase::Create).await;
    let original_bytes = b"first original bytes".to_vec();
    let thumbnail_bytes = b"first thumbnail".to_vec();
    let upload = PhotoUpload::new(
        PhotoMetadata {
            relative_path: "2024/IMG_0002.HEIC".into(),
            is_favorite: false,
            asset_date_ms: Some(1_700_000_000_000),
            is_hidden: false,
        },
        AssetUpload::from_bytes(original_bytes.clone(), "image/heic").with_dimensions(4, 3),
        AssetUpload::from_bytes(thumbnail_bytes.clone(), "image/jpeg"),
    );
    let photo = connection.create_photo(&lease, upload).await.unwrap();
    assert_eq!(photo.relative_path, "2024/IMG_0002.HEIC");
    assert_eq!(photo.content_generation.len(), 36);
    assert_eq!(photo.original.size, original_bytes.len() as u64);

    let keys = mock.modify_keys.lock().unwrap().clone();
    assert!(keys.last().unwrap().starts_with("photo-create-"));

    // Fresh capabilities for a download, as a real client would request.
    let described = connection
        .describe_asset(&photo.original.asset_id, AssetRole::Original)
        .await
        .unwrap();
    let downloaded = connection
        .download_asset(&described, &DownloadOptions::new())
        .await
        .unwrap();
    assert_eq!(downloaded.bytes, original_bytes);
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("original.heic");
    connection
        .download_to_file(&described, &target)
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), original_bytes);

    let mut metadata = photo.metadata();
    metadata.is_favorite = true;
    metadata.relative_path = "2024/renamed.heic".into();
    metadata.asset_date_ms = None;
    assert!(matches!(
        connection
            .update_photo_metadata(&lease, &photo, metadata.clone())
            .await,
        Err(sync_client::Error::PhaseMismatch(_))
    ));
    lease.release().await.unwrap();

    let lease = lease_at(&connection, &zone, UploadPhase::Modify).await;
    let updated = connection
        .update_photo_metadata(&lease, &photo, metadata.clone())
        .await
        .unwrap();
    assert!(updated.is_favorite);
    assert_eq!(updated.relative_path, "2024/renamed.heic");
    assert_eq!(updated.asset_date_ms, None);
    assert_eq!(updated.content_generation, photo.content_generation);
    assert!(
        mock.modify_keys
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("photo-metadata-")
    );

    let new_original = b"second original bytes".to_vec();
    let new_thumbnail = b"second thumbnail".to_vec();
    let update = PhotoContentUpdate::new(
        AssetUpload::from_bytes(new_original.clone(), "image/heic"),
        AssetUpload::from_bytes(new_thumbnail, "image/jpeg"),
    );
    let updated = connection
        .update_photo_content(&lease, &updated, update)
        .await
        .unwrap();
    assert_ne!(updated.content_generation, photo.content_generation);
    assert_ne!(updated.original.asset_id, photo.original.asset_id);
    assert_eq!(updated.relative_path, "2024/renamed.heic");

    let described = connection
        .describe_asset(&updated.original.asset_id, AssetRole::Original)
        .await
        .unwrap();
    let downloaded = connection
        .download_asset(&described, &DownloadOptions::new())
        .await
        .unwrap();
    assert_eq!(downloaded.bytes, new_original);
    lease.release().await.unwrap();

    let lease = lease_at(&connection, &zone, UploadPhase::Delete).await;
    connection.delete_photo(&lease, &updated).await.unwrap();
    assert!(
        mock.modify_keys
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .starts_with("photo-delete-")
    );
    assert_eq!(mock.records.lock().unwrap().len(), 0);
    lease.release().await.unwrap();

    // A tombstoned pair is no longer a photo.
    assert!(connection.fetch_photo(&zone, "photo-1").await.is_err());
}
