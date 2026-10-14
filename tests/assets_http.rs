mod common;
#[path = "../examples/support/receiver.rs"]
mod receiver;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::{API_KEY, Lease, Running};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
const CK: &str = "/database/1/com.apple.photos.cloud/production/private";
fn fixtures() -> Value {
    serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap()
}
fn local(app: &Running, signed: &str) -> String {
    let url = url::Url::parse(signed).unwrap();
    format!("{}{}?{}", app.base, url.path(), url.query().unwrap())
}
async fn ready_lease(app: &Running) -> Lease {
    let lease = common::ready(
        &app.client,
        &app.base,
        API_KEY,
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    for phase in ["modify", "create"] {
        assert_eq!(
            lease
                .control(
                    &app.client,
                    &app.base,
                    API_KEY,
                    "phase",
                    json!({"phase":phase})
                )
                .await
                .0,
            200
        );
    }
    lease
}
async fn reserve(
    app: &Running,
    lease: &Lease,
    role: &str,
    bytes: &[u8],
    sha: Option<&str>,
) -> Value {
    let field = if role == "thumbnail" {
        "resJPEGThumbRes"
    } else {
        "resOriginalRes"
    };
    let mut options = json!({"role":role,"intent":"create","expectedSize":bytes.len()});
    if let Some(sha) = sha {
        options["expectedSha256"] = json!(sha);
    }
    let response=lease.headers(app.client.post(format!("{}{CK}/assets/upload",app.base)).bearer_auth(API_KEY).json(&json!({"tokens":[{"recordName":uuid::Uuid::new_v4().to_string(),"recordType":"CPLMaster","fieldName":field,"_syncserver":options}]}))).send().await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body["tokens"][0].clone()
}
async fn upload(app: &Running, lease: &Lease, token: &Value, bytes: Vec<u8>) -> (u16, Value) {
    let response = lease
        .headers(
            app.client
                .post(local(app, token["url"].as_str().unwrap()))
                .header("Content-Type", "application/octet-stream")
                .body(bytes),
        )
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
async fn metrics(app: &Running) -> Value {
    app.client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
#[tokio::test]
async fn real_pair_upload_publish_snapshot_download_and_empty_polls_never_rehash_objects() {
    let server_root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config(server_root.path())).await;
    let lease = ready_lease(&app).await;
    let fixture = fixtures();
    let mut descriptions = Vec::new();
    let mut bodies = Vec::new();
    for item in fixture["binaryAssets"].as_array().unwrap() {
        let bytes = STANDARD
            .decode(item["bytesBase64"].as_str().unwrap())
            .unwrap();
        let token = reserve(
            &app,
            &lease,
            item["role"].as_str().unwrap(),
            &bytes,
            item["sha256"].as_str(),
        )
        .await;
        let (status, body) = upload(&app, &lease, &token, bytes.clone()).await;
        assert_eq!(status, 200, "{body}");
        assert_eq!(
            body["singleFile"]["fileChecksum"],
            item["value"]["fileChecksum"]
        );
        descriptions.push(body["singleFile"].clone());
        bodies.push(bytes);
        let (status, _) = upload(
            &app,
            &lease,
            &token,
            vec![0; item["value"]["size"].as_u64().unwrap() as usize],
        )
        .await;
        assert_eq!(status, 409);
        let status: Value = app
            .client
            .get(format!(
                "{}/v1/uploads/{}",
                app.base,
                token["_syncserver"]["sessionId"].as_str().unwrap()
            ))
            .bearer_auth(API_KEY)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["state"], "Published");
        assert_eq!(
            status["asset"]["assetId"],
            descriptions.last().unwrap()["assetId"]
        );
    }
    assert_eq!(descriptions[1]["width"], 1);
    assert_eq!(descriptions[1]["height"], 1);
    let mut request = fixture["createPhotoRequest"].clone();
    for op in request["operations"].as_array_mut().unwrap() {
        if op["record"]["recordType"] == "CPLMaster" {
            op["record"]["fields"]["resOriginalRes"]["value"] = descriptions[0].clone();
            op["record"]["fields"]["resJPEGThumbRes"]["value"] = descriptions[1].clone();
        }
    }
    let response = lease
        .headers(
            app.client
                .post(format!("{}{CK}/records/modify", app.base))
                .bearer_auth(API_KEY)
                .header("Idempotency-Key", "photo-create")
                .json(&request),
        )
        .send()
        .await
        .unwrap();
    let status = response.status();
    let saved: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{saved}");
    assert!(
        saved["records"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["recordType"] == "CPLMaster")
            .unwrap()["fields"]["resOriginalRes"]["value"]["downloadURL"]
            .is_string()
    );
    let receiver_root = tempfile::tempdir().unwrap();
    let receiver_path = receiver_root.path().join("receiver.sqlite");
    let mut receiver = receiver::Receiver::open(&receiver_path).unwrap();
    let page1: Value = app
        .client
        .post(format!("{}{CK}/changes/zone", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"zones":[{"resultsLimit":1}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let page1 = &page1["zones"][0];
    assert_eq!(page1["moreComing"], true);
    receiver.db.execute_batch("CREATE TRIGGER inject_receiver_failure BEFORE UPDATE ON progress BEGIN SELECT RAISE(ABORT,'receiver disk fault');END;").unwrap();
    assert!(receiver.receive(page1).is_err());
    assert!(receiver.ingest().unwrap().is_none());
    assert_eq!(
        receiver
            .db
            .query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    receiver
        .db
        .execute_batch("DROP TRIGGER inject_receiver_failure")
        .unwrap();
    receiver.receive(page1).unwrap();
    assert!(receiver.applied().unwrap().is_none());
    assert!(receiver.pending().unwrap().is_empty());
    assert_eq!(
        receiver
            .db
            .query_row("SELECT count(*) FROM projection", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
    drop(receiver);
    let mut receiver = receiver::Receiver::open(&receiver_path).unwrap();
    let page2: Value = app
        .client
        .post(format!("{}{CK}/changes/zone", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"zones":[{"syncToken":receiver.ingest().unwrap()}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    receiver.receive(&page2["zones"][0]).unwrap();
    assert_eq!(receiver.pending().unwrap().len(), 2);
    assert!(receiver.applied().unwrap().is_none());
    for (index, (id, role, descriptor)) in receiver.pending().unwrap().into_iter().enumerate() {
        let bytes = app
            .client
            .get(local(&app, descriptor["downloadURL"].as_str().unwrap()))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap();
        let checksum = STANDARD.encode(Sha256::digest(&bytes));
        receiver
            .resource_published(&id, &role, bytes.len() as u64, &checksum)
            .unwrap();
        if index == 0 {
            assert!(receiver.applied().unwrap().is_none());
            drop(receiver);
            receiver = receiver::Receiver::open(&receiver_path).unwrap();
        }
    }
    assert_eq!(
        receiver.applied().unwrap().as_deref(),
        page2["zones"][0]["syncToken"].as_str()
    );
    for (desc, bytes) in descriptions.iter().zip(&bodies) {
        let response = app
            .client
            .get(local(&app, desc["downloadURL"].as_str().unwrap()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
    }
    let before = metrics(&app).await;
    let initial: Value = app
        .client
        .post(format!("{}{CK}/changes/zone", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"zones":[{}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for _ in 0..100 {
        let delta: Value = app
            .client
            .post(format!("{}{CK}/changes/zone", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({"zones":[{"syncToken":initial["zones"][0]["syncToken"]}]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(delta["zones"][0]["records"], json!([]));
        let response = app
            .client
            .head(local(
                &app,
                descriptions[0]["downloadURL"].as_str().unwrap(),
            ))
            .header("Range", "bytes=0-0")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.headers()["content-length"],
            bodies[0].len().to_string()
        );
    }
    let after = metrics(&app).await;
    assert_eq!(
        after["metrics"]["assetBytesHashed"],
        before["metrics"]["assetBytesHashed"]
    );
    assert_eq!(
        after["metrics"]["assetBytesRead"],
        before["metrics"]["assetBytesRead"]
    );
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        200
    );
    let photo = saved["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["recordType"] == "CPLAsset")
        .unwrap();
    let(status,updated)=common::fixture_modify(&app,json!({"operations":[{"operationType":"update","record":{"recordName":photo["recordName"],"recordChangeTag":photo["recordChangeTag"],"fields":{"isFavorite":{"type":"INT64","value":1}}}}]}),Some("favorite")).await;
    assert_eq!(status, 200, "{updated}");
    assert_eq!(
        updated["records"][0]["_syncserver"]["contentEvent"],
        photo["_syncserver"]["contentEvent"]
    );
    let latest = &updated["records"][0];
    let(status,failed)=common::fixture_modify(&app,json!({"atomic":false,"operations":[{"operationType":"update","record":{"recordName":latest["recordName"],"recordChangeTag":latest["recordChangeTag"],"fields":{"isFavorite":{"type":"INT64","value":0}},"_syncserver":{"deletionEvent":{"editedAtMs":123,"source":"test","eventId":"wrong-category"}}}}]}),Some("failed-metadata-event")).await;
    assert_eq!(status, 200, "{failed}");
    assert_eq!(failed["records"][0]["serverErrorCode"], "BAD_REQUEST");
    let db = rusqlite::Connection::open_with_flags(
        server_root.path().join("data/metadata.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap();
    assert_eq!(
        db.query_row("SELECT count(*) FROM photo_bindings", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    let unchanged: Value = app
        .client
        .post(format!("{}{CK}/records/lookup", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"records":[{"recordName":latest["recordName"]}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        unchanged["records"][0]["recordChangeTag"],
        latest["recordChangeTag"]
    );
    assert_eq!(unchanged["records"][0]["fields"]["isFavorite"]["value"], 1);
    let final_metrics = metrics(&app).await;
    assert_eq!(
        final_metrics["metrics"]["assetBytesHashed"],
        after["metrics"]["assetBytesHashed"]
    );
    app.stop().await;
}
#[tokio::test]
async fn checksum_thumbnail_validation_dedup_and_conditional_range_downloads() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config(root.path())).await;
    let lease = ready_lease(&app).await;
    let bytes = b"original-opaque-test-body".to_vec();
    let wrong = reserve(&app, &lease, "original", &bytes, Some(&"0".repeat(64))).await;
    let (status, body) = upload(&app, &lease, &wrong, bytes.clone()).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(body["serverErrorCode"], "CHECKSUM_MISMATCH");
    let bad = reserve(&app, &lease, "thumbnail", &bytes, None).await;
    assert_eq!(
        upload(&app, &lease, &bad, bytes.clone()).await.1["serverErrorCode"],
        "INVALID_THUMBNAIL"
    );
    let token = reserve(&app, &lease, "original", &bytes, None).await;
    let (status, first) = upload(&app, &lease, &token, bytes.clone()).await;
    assert_eq!(status, 200, "{first}");
    let again = reserve(&app, &lease, "original", &bytes, None).await;
    let (_, second) = upload(&app, &lease, &again, bytes.clone()).await;
    assert_eq!(
        first["singleFile"]["assetId"],
        second["singleFile"]["assetId"]
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("data/objects"))
            .unwrap()
            .count(),
        1
    );
    let url = local(&app, first["singleFile"]["downloadURL"].as_str().unwrap());
    let etag = format!("\"sha256:{:x}\"", Sha256::digest(&bytes));
    for (range, status, expected) in [
        ("bytes=0-3", 206, b"orig".as_slice()),
        ("bytes=-4", 206, b"body".as_slice()),
        ("bytes=9-", 206, &bytes[9..]),
        ("bytes=999-", 416, b"".as_slice()),
        ("bytes=0-1,3-4", 416, b"".as_slice()),
        ("bytes=-0", 416, b"".as_slice()),
        ("bytes=+1-2", 416, b"".as_slice()),
        ("bytes=-+2", 416, b"".as_slice()),
    ] {
        let response = app
            .client
            .get(&url)
            .header("Range", range)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(response.bytes().await.unwrap().as_ref(), expected);
    }
    for path in ["/v1/assets/invalid?role=original", "/v1/uploads/invalid"] {
        let response = app
            .client
            .get(format!("{}{path}", app.base))
            .bearer_auth(API_KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
        assert_eq!(
            response.json::<Value>().await.unwrap()["serverErrorCode"],
            "BAD_REQUEST"
        );
    }
    let before = metrics(&app).await;
    assert_eq!(
        app.client
            .get(&url)
            .header("If-None-Match", &etag)
            .send()
            .await
            .unwrap()
            .status(),
        304
    );
    assert_eq!(
        app.client
            .get(&url)
            .header("If-Match", "\"wrong\"")
            .send()
            .await
            .unwrap()
            .status(),
        412
    );
    let response = app
        .client
        .get(&url)
        .header("Range", "bytes=0-3")
        .header("If-Range", "\"wrong\"")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.bytes().await.unwrap().as_ref(), bytes);
    let mut changed = url::Url::parse(&url).unwrap();
    changed.set_query(Some(
        &changed
            .query()
            .unwrap()
            .replace("role=original", "role=thumbnail"),
    ));
    assert_eq!(app.client.get(changed).send().await.unwrap().status(), 401);
    let after = metrics(&app).await;
    assert_eq!(
        before["metrics"]["assetBytesHashed"],
        after["metrics"]["assetBytesHashed"]
    );
    app.stop().await;
}

#[tokio::test]
async fn losing_lease_aborts_a_live_body_while_reads_and_new_holder_continue() {
    let app = Running::new().await;
    let lease = ready_lease(&app).await;
    let bytes = vec![7u8; 1024];
    let token = reserve(&app, &lease, "original", &bytes, None).await;
    let (sender, receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    sender.send(vec![7; 128]).await.unwrap();
    let body = reqwest::Body::wrap_stream(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move {
            let chunk = receiver.recv().await?;
            Some((Ok::<_, std::io::Error>(chunk), (receiver)))
        },
    ));
    let request = lease.headers(
        app.client
            .post(local(&app, token["url"].as_str().unwrap()))
            .header("Content-Type", "application/octet-stream")
            .body(body),
    );
    let task = tokio::spawn(async move { request.send().await.unwrap() });
    let id = token["_syncserver"]["sessionId"].as_str().unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            let state: Value = app
                .client
                .get(format!("{}/v1/uploads/{id}", app.base))
                .bearer_auth(API_KEY)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if state["state"] == "Receiving" {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        lease
            .control(
                &app.client,
                &app.base,
                API_KEY,
                "phase",
                json!({"phase":"create"})
            )
            .await
            .1["serverErrorCode"],
        "UPLOAD_BUSY"
    );
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        200
    );
    let response = tokio::time::timeout(std::time::Duration::from_secs(2), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(response.status(), 409);
    assert_eq!(
        response.json::<Value>().await.unwrap()["serverErrorCode"],
        "UPLOAD_LEASE_LOST"
    );
    drop(sender);
    let state: Value = app
        .client
        .get(format!("{}/v1/uploads/{id}", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(state["state"], "Failed");
    assert_eq!(state["serverErrorCode"], "UPLOAD_LEASE_LOST");
    let fresh = ready_lease(&app).await;
    assert_eq!(
        upload(&app, &lease, &token, bytes).await.1["serverErrorCode"],
        "UPLOAD_LEASE_LOST"
    );
    assert_eq!(
        fresh
            .control(&app.client, &app.base, API_KEY, "renew", json!({}))
            .await
            .0,
        200
    );
    app.stop().await;
}

#[tokio::test]
async fn stable_lease_identity_heartbeats_keep_a_body_alive_over_multiple_ttls() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[
            ("SYNC_LEASE_TTL_SECS", "2"),
            ("SYNC_LEASE_HEARTBEAT_SECS", "1"),
            ("SYNC_UPLOAD_IDLE_TIMEOUT_SECS", "5"),
        ],
    ))
    .await;
    let lease = ready_lease(&app).await;
    let bytes = vec![9u8; 6];
    let token = reserve(&app, &lease, "original", &bytes, None).await;
    let (sender, receiver) = tokio::sync::mpsc::channel::<Vec<u8>>(2);
    let body = reqwest::Body::wrap_stream(futures_util::stream::unfold(
        receiver,
        |mut receiver| async move {
            let chunk = receiver.recv().await?;
            Some((Ok::<_, std::io::Error>(chunk), receiver))
        },
    ));
    let request = lease.headers(
        app.client
            .post(local(&app, token["url"].as_str().unwrap()))
            .header("Content-Type", "application/octet-stream")
            .body(body),
    );
    let task = tokio::spawn(async move { request.send().await.unwrap() });
    for _ in 0..6 {
        assert_eq!(
            lease
                .control(&app.client, &app.base, API_KEY, "renew", json!({}))
                .await
                .0,
            200
        );
        sender.send(vec![9]).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
    }
    drop(sender);
    let response = task.await.unwrap();
    let status = response.status();
    let result: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{result}");
    assert_eq!(result["singleFile"]["size"], 6);
    app.stop().await;
}

#[tokio::test]
async fn later_thumbnail_validation_cannot_mutate_historical_fields_or_idempotency() {
    let app = Running::new().await;
    let lease = ready_lease(&app).await;
    let fixture = fixtures();
    let bytes = STANDARD
        .decode(fixture["binaryAssets"][0]["bytesBase64"].as_str().unwrap())
        .unwrap();
    let original = reserve(&app, &lease, "original", &bytes, None).await;
    let (status, uploaded) = upload(&app, &lease, &original, bytes.clone()).await;
    assert_eq!(status, 200);
    let original = &uploaded["singleFile"];
    assert!(original.get("width").is_none());
    let request = json!({"operations":[{"operationType":"create","record":{"recordName":"historical-resource","recordType":"Document","fields":{"resource":{"type":"ASSETID","value":original}}}}]});
    let send = || {
        lease.headers(
            app.client
                .post(format!("{}{CK}/records/modify", app.base))
                .bearer_auth(API_KEY)
                .header("Idempotency-Key", "stable-asset-reference")
                .json(&request),
        )
    };
    let response = send().send().await.unwrap();
    assert_eq!(response.status(), 200);
    let first: Value = response.json().await.unwrap();
    assert!(
        first["records"][0]["fields"]["resource"]["value"]
            .get("width")
            .is_none()
    );
    let snapshot: Value = app
        .client
        .post(format!("{}/v1/snapshots", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let thumbnail = reserve(&app, &lease, "thumbnail", &bytes, None).await;
    let (_, upgraded) = upload(&app, &lease, &thumbnail, bytes).await;
    assert_eq!(upgraded["singleFile"]["assetId"], original["assetId"]);
    assert_eq!(upgraded["singleFile"]["width"], 2);
    let replay = send().send().await.unwrap();
    let status = replay.status();
    let replay: Value = replay.json().await.unwrap();
    assert_eq!(status, 200, "{replay}");
    assert_eq!(
        replay["records"][0]["recordChangeTag"],
        first["records"][0]["recordChangeTag"]
    );
    assert!(
        replay["records"][0]["fields"]["resource"]["value"]
            .get("width")
            .is_none()
    );
    let old: Value = app
        .client
        .post(format!("{}{CK}/records/query", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"snapshotToken":snapshot["snapshotToken"],"query":{"recordType":"Document"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        old["records"][0]["fields"]["resource"]["value"]
            .get("width")
            .is_none()
    );
    let mut invalid = request;
    invalid["operations"][0]["record"]["recordName"] = json!("invalid-size-metadata");
    invalid["operations"][0]["record"]["fields"]["resource"]["value"]["mimeType"] =
        json!("text/html");
    let response = lease
        .headers(
            app.client
                .post(format!("{}{CK}/records/modify", app.base))
                .bearer_auth(API_KEY)
                .json(&invalid),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    app.stop().await;
}

#[tokio::test]
async fn concurrent_identical_original_and_thumbnail_publications_share_one_safe_object() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config(root.path())).await;
    let lease = ready_lease(&app).await;
    let bytes = STANDARD
        .decode(
            fixtures()["binaryAssets"][0]["bytesBase64"]
                .as_str()
                .unwrap(),
        )
        .unwrap();
    for _ in 0..8 {
        let original = reserve(&app, &lease, "original", &bytes, None).await;
        let thumbnail = reserve(&app, &lease, "thumbnail", &bytes, None).await;
        let (a, b) = tokio::join!(
            upload(&app, &lease, &original, bytes.clone()),
            upload(&app, &lease, &thumbnail, bytes.clone())
        );
        assert_eq!(a.0, 200, "{}", a.1);
        assert_eq!(b.0, 200, "{}", b.1);
        assert_eq!(a.1["singleFile"]["assetId"], b.1["singleFile"]["assetId"]);
    }
    assert_eq!(
        std::fs::read_dir(root.path().join("data/objects"))
            .unwrap()
            .count(),
        1
    );
    assert_eq!(
        std::fs::read_dir(root.path().join("data/staging"))
            .unwrap()
            .count(),
        0
    );
    app.stop().await;
}
