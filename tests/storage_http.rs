//! Real account-local quota admission, publication and recovery over HTTP.
mod common;
use common::{API_KEY, Lease, Running};
use serde_json::{Value, json};
use syncserver::users::Users;
const CK: &str = "/database/1/com.apple.photos.cloud/production/private";

async fn get(app: &Running, auth: &str, path: &str) -> Value {
    let response = app
        .client
        .get(format!("{}{path}", app.base))
        .bearer_auth(auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
async fn usage(app: &Running, auth: &str) -> Value {
    let response = app
        .client
        .get(format!("{}/v1/storage", app.base))
        .bearer_auth(auth)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let value: Value = response.json().await.unwrap();
    assert_eq!(
        value["imageBytes"].as_u64().unwrap()
            + value["videoBytes"].as_u64().unwrap()
            + value["otherBytes"].as_u64().unwrap(),
        value["usedBytes"].as_u64().unwrap()
    );
    assert!(value.get("thumbnailBytes").is_none());
    value
}
async fn ready(app: &Running, auth: &str) -> Lease {
    let lease = common::ready(
        &app.client,
        &app.base,
        auth,
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    for phase in ["modify", "create"] {
        assert_eq!(
            lease
                .control(
                    &app.client,
                    &app.base,
                    auth,
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
fn request(sizes: &[u64]) -> Value {
    json!({"tokens": sizes.iter().enumerate().map(|(index, size)| json!({
        "recordName":format!("master-{index}"), "recordType":"CPLMaster",
        "fieldName":"resOriginalRes", "_syncserver":{
            "role":"original", "intent":"create", "expectedSize":size
        }
    })).collect::<Vec<_>>()})
}
async fn reserve(
    app: &Running,
    auth: &str,
    lease: &Lease,
    body: &Value,
    key: &str,
) -> (u16, Value) {
    let response = lease
        .headers(
            app.client
                .post(format!("{}{CK}/assets/upload", app.base))
                .bearer_auth(auth)
                .header("Idempotency-Key", key)
                .json(body),
        )
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
fn local(app: &Running, signed: &str) -> String {
    let url = url::Url::parse(signed).unwrap();
    format!("{}{}?{}", app.base, url.path(), url.query().unwrap())
}
async fn upload(
    app: &Running,
    auth: &str,
    lease: &Lease,
    reserved: &Value,
    bytes: &[u8],
) -> (u16, Value) {
    let response = lease
        .headers(
            app.client
                .post(local(app, reserved["tokens"][0]["url"].as_str().unwrap()))
                .bearer_auth(auth)
                .header("Content-Type", "application/octet-stream")
                .body(bytes.to_vec()),
        )
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
async fn snapshot(app: &Running) -> Value {
    app.client
        .post(format!("{}/v1/snapshots", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

fn file(name: &str, asset: &Value) -> Value {
    json!({"operationType":"create", "record":{"recordName":name,"recordType":"File",
        "fields":{"resOriginalRes":{"type":"ASSETID","value":asset}}}})
}
async fn modify(
    app: &Running,
    auth: &str,
    lease: &Lease,
    operations: Vec<Value>,
    key: &str,
    atomic: bool,
) -> (u16, Value) {
    let response = lease
        .headers(
            app.client
                .post(format!("{}{CK}/records/modify", app.base))
                .bearer_auth(auth)
                .header("Idempotency-Key", key)
                .json(&json!({"atomic":atomic,"operations":operations})),
        )
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

#[tokio::test]
async fn quota_query_requires_identity_has_no_scope_override_and_advertises_20gb() {
    let app = Running::new().await;
    assert_eq!(
        app.client
            .get(format!("{}/v1/storage", app.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        usage(&app, API_KEY).await,
        json!({
            "quotaBytes":20_000_000_000u64, "usedBytes":0, "reservedBytes":0,
            "availableBytes":20_000_000_000u64,
            "imageBytes":0,"videoBytes":0,"otherBytes":0
        })
    );
    for query in ["?user=bob", "?namespace=other", "?quotaBytes=999", "?"] {
        let response = app
            .client
            .get(format!("{}/v1/storage{query}", app.base))
            .bearer_auth(API_KEY)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{query}");
        assert_eq!(response.headers()["cache-control"], "no-store");
    }
    let capabilities = get(&app, API_KEY, "/v1/capabilities").await;
    assert_eq!(capabilities["capabilities"]["storageQuota"], true);
    assert_eq!(
        capabilities["capabilities"]["storageUsageByMediaType"],
        true
    );
    assert_eq!(
        capabilities["limits"]["userStorageQuotaBytes"],
        20_000_000_000u64
    );
    app.stop().await;
}

#[tokio::test]
async fn reservations_are_atomic_idempotent_and_concurrent_requests_cannot_oversell() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", "10")],
    ))
    .await;
    let lease = ready(&app, API_KEY).await;
    let baseline = snapshot(&app).await;
    let body = request(&[6]);
    let (a, b) = tokio::join!(
        reserve(&app, API_KEY, &lease, &body, "race-a"),
        reserve(&app, API_KEY, &lease, &body, "race-b")
    );
    let ((status, saved), key, loser) = if a.0 == 200 {
        (a, "race-a", b)
    } else {
        (b, "race-b", a)
    };
    assert_eq!(status, 200);
    assert_eq!(loser.0, 409);
    assert_eq!(loser.1["serverErrorCode"], "STORAGE_QUOTA_EXCEEDED");
    assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 6);
    let retry = reserve(&app, API_KEY, &lease, &body, key).await;
    assert_eq!(retry.0, 200);
    assert_eq!(
        retry.1["tokens"][0]["_syncserver"]["sessionId"],
        saved["tokens"][0]["_syncserver"]["sessionId"]
    );
    let batch = reserve(&app, API_KEY, &lease, &request(&[3, 2]), "batch").await;
    assert_eq!(batch.0, 409);
    assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 6);
    // A rejected batch did not persist its idempotency key or any partial session.
    assert_eq!(
        reserve(&app, API_KEY, &lease, &request(&[4]), "batch")
            .await
            .0,
        200
    );
    assert_eq!(usage(&app, API_KEY).await["availableBytes"], 0);
    assert_eq!(snapshot(&app).await, baseline);
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        200
    );
    assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 0);
    let next = ready(&app, API_KEY).await;
    assert_eq!(
        reserve(&app, API_KEY, &next, &request(&[10]), "new-lease")
            .await
            .0,
        200
    );
    app.stop().await;
}

#[tokio::test]
async fn identical_objects_charge_each_logical_file_and_survive_quota_reduction() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", "12")],
    ))
    .await;
    let lease = ready(&app, API_KEY).await;
    let mut first = Value::Null;
    for (index, key) in ["first", "duplicate"].into_iter().enumerate() {
        let (status, reserved) = reserve(&app, API_KEY, &lease, &request(&[4]), key).await;
        assert_eq!(status, 200);
        assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 4);
        let (status, published) = upload(&app, API_KEY, &lease, &reserved, b"same").await;
        assert_eq!(status, 200, "{published}");
        if first.is_null() {
            first = published["singleFile"].clone();
        }
        assert_eq!(first["assetId"], published["singleFile"]["assetId"]);
        let space = usage(&app, API_KEY).await;
        assert_eq!(space["usedBytes"], index as u64 * 4);
        assert_eq!(space["reservedBytes"], 0);
        assert_eq!(
            modify(&app, API_KEY, &lease, vec![file(key, &first)], key, true)
                .await
                .0,
            200
        );
        assert_eq!(
            usage(&app, API_KEY).await["usedBytes"],
            (index as u64 + 1) * 4
        );
    }
    // Declaring an existing hash still requires full upload reservation.
    let mut bad = request(&[4]);
    bad["tokens"][0]["_syncserver"]["expectedSha256"] = json!("0".repeat(64));
    let (_, reserved) = reserve(&app, API_KEY, &lease, &bad, "bad-hash").await;
    assert_ne!(
        upload(&app, API_KEY, &lease, &reserved, b"bad!").await.0,
        200
    );
    assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 0);
    let (_, reserved) = reserve(&app, API_KEY, &lease, &request(&[3]), "wrong-size").await;
    assert_ne!(
        upload(&app, API_KEY, &lease, &reserved, b"four").await.0,
        200
    );
    assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 0);
    let (_, reserved) = reserve(&app, API_KEY, &lease, &request(&[4]), "fill").await;
    let (status, published) = upload(&app, API_KEY, &lease, &reserved, b"more").await;
    assert_eq!(status, 200);
    assert_eq!(
        modify(
            &app,
            API_KEY,
            &lease,
            vec![file("third", &published["singleFile"])],
            "third",
            true
        )
        .await
        .0,
        200
    );
    let baseline = snapshot(&app).await;
    let full = usage(&app, API_KEY).await;
    assert_eq!(full["usedBytes"], 12);
    assert_eq!(full["availableBytes"], 0);
    assert_eq!(
        reserve(&app, API_KEY, &lease, &request(&[1]), "over")
            .await
            .0,
        409
    );
    let before = get(&app, API_KEY, "/v1/metrics").await;
    for _ in 0..10 {
        assert_eq!(usage(&app, API_KEY).await, full);
    }
    let after = get(&app, API_KEY, "/v1/metrics").await;
    for field in ["assetBytesRead", "assetBytesHashed"] {
        assert_eq!(before["metrics"][field], after["metrics"][field]);
    }
    assert_eq!(snapshot(&app).await, baseline);
    app.stop().await;
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", "4")],
    ))
    .await;
    let space = usage(&app, API_KEY).await;
    assert_eq!(space["quotaBytes"], 4);
    assert_eq!(space["usedBytes"], 12);
    assert_eq!(space["availableBytes"], 0);
    assert_eq!(snapshot(&app).await, baseline);
    let lease = ready(&app, API_KEY).await;
    assert_eq!(
        reserve(&app, API_KEY, &lease, &request(&[1]), "still-over")
            .await
            .0,
        409
    );
    let downloaded = app
        .client
        .get(local(&app, first["downloadURL"].as_str().unwrap()))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(downloaded.status(), 200);
    assert_eq!(downloaded.bytes().await.unwrap().as_ref(), b"same");
    app.stop().await;
}

#[tokio::test]
async fn restart_releases_unpublished_reservations() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config_with(root.path(), &[("SYNC_USER_STORAGE_QUOTA_BYTES", "4")]);
    let app = Running::start(config.clone()).await;
    let lease = ready(&app, API_KEY).await;
    assert_eq!(
        reserve(&app, API_KEY, &lease, &request(&[4]), "pending")
            .await
            .0,
        200
    );
    assert_eq!(usage(&app, API_KEY).await["availableBytes"], 0);
    app.stop().await;
    let app = Running::start(config).await;
    assert_eq!(usage(&app, API_KEY).await["availableBytes"], 4);
    let lease = ready(&app, API_KEY).await;
    assert_eq!(
        reserve(&app, API_KEY, &lease, &request(&[4]), "replacement")
            .await
            .0,
        200
    );
    app.stop().await;
}

#[tokio::test]
async fn users_have_independent_quotas_and_identical_content_is_charged_to_each() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config_with(root.path(), &[("SYNC_USER_STORAGE_QUOTA_BYTES", "4")]);
    const PASSWORD: &str = "synthetic-storage-password";
    let users = Users::open(&root.path().join("data")).unwrap();
    for name in ["alice", "bob"] {
        users.create(name, PASSWORD, false).unwrap();
    }
    drop(users);
    let app = Running::start(config).await;
    let mut descriptors = vec![];
    let mut accounts = vec![];
    for name in ["alice", "bob"] {
        let login: Value = app
            .client
            .post(format!("{}/v1/auth/login", app.base))
            .json(&json!({"username":name, "password":PASSWORD}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let token = login["accessToken"].as_str().unwrap();
        assert_eq!(usage(&app, token).await["availableBytes"], 4);
        let lease = ready(&app, token).await;
        let (status, reserved) =
            reserve(&app, token, &lease, &request(&[4]), "same-operation").await;
        assert_eq!(status, 200);
        let (status, published) = upload(&app, token, &lease, &reserved, b"same").await;
        assert_eq!(status, 200, "{published}");
        descriptors.push(published["singleFile"]["assetId"].clone());
        assert_eq!(usage(&app, token).await["usedBytes"], 0);
        let mut operation = file("same-file", &published["singleFile"]);
        let (path, category) = if name == "alice" {
            ("clip.MP4", "videoBytes")
        } else {
            ("photo.HEIC", "imageBytes")
        };
        operation["record"]["fields"]["relativePath"] = json!({"type":"STRING","value":path});
        accounts.push((token.to_owned(), category));
        assert_eq!(
            modify(&app, token, &lease, vec![operation], "create", true)
                .await
                .0,
            200
        );
        assert_eq!(usage(&app, token).await["usedBytes"], 4);
        assert_eq!(
            reserve(&app, token, &lease, &request(&[1]), "over").await.0,
            409
        );
    }
    assert_ne!(descriptors[0], descriptors[1]);
    for (token, category) in accounts {
        let value = usage(&app, &token).await;
        assert_eq!(value[category], 4);
        assert_eq!(value["otherBytes"], 0);
    }
    app.stop().await;
}

#[tokio::test]
async fn photo_pair_charges_both_roles_and_deletion_releases_logical_usage() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let fixture: Value =
        serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap();
    let quota: u64 = fixture["binaryAssets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["value"]["size"].as_u64().unwrap())
        .sum();
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", &quota.to_string())],
    ))
    .await;
    let lease = ready(&app, API_KEY).await;
    let mut descriptors = vec![];
    for (index, item) in fixture["binaryAssets"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let bytes = STANDARD
            .decode(item["bytesBase64"].as_str().unwrap())
            .unwrap();
        let mut body = request(&[bytes.len() as u64]);
        if item["role"] == "thumbnail" {
            body["tokens"][0]["fieldName"] = json!("resJPEGThumbRes");
            body["tokens"][0]["_syncserver"]["role"] = json!("thumbnail");
        }
        let (status, reserved) =
            reserve(&app, API_KEY, &lease, &body, &format!("pair-{index}")).await;
        assert_eq!(status, 200);
        let (status, published) = upload(&app, API_KEY, &lease, &reserved, &bytes).await;
        assert_eq!(status, 200, "{published}");
        descriptors.push(published["singleFile"].clone());
        assert_eq!(usage(&app, API_KEY).await["usedBytes"], 0);
    }
    let mut create = fixture["createPhotoRequest"].clone();
    for operation in create["operations"].as_array_mut().unwrap() {
        if operation["record"]["recordType"] == "CPLMaster" {
            operation["record"]["fields"]["resOriginalRes"]["value"] = descriptors[0].clone();
            operation["record"]["fields"]["resJPEGThumbRes"]["value"] = descriptors[1].clone();
        }
    }
    let response = lease
        .headers(
            app.client
                .post(format!("{}{CK}/records/modify", app.base))
                .bearer_auth(API_KEY)
                .header("Idempotency-Key", "pair-create")
                .json(&create),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let created: Value = response.json().await.unwrap();
    assert_eq!(usage(&app, API_KEY).await["availableBytes"], 0);
    assert_eq!(usage(&app, API_KEY).await["imageBytes"], quota);
    assert_eq!(usage(&app, API_KEY).await["videoBytes"], 0);
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        200
    );
    let deletion = json!({"atomic":true,"operations":created["records"].as_array().unwrap().iter().map(|record| json!({
        "operationType":"delete", "record":{"recordName":record["recordName"], "recordChangeTag":record["recordChangeTag"]}
    })).collect::<Vec<_>>()});
    let (status, deleted) = common::fixture_modify(&app, deletion, Some("pair-delete")).await;
    assert_eq!(status, 200, "{deleted}");
    assert!(
        deleted["records"]
            .as_array()
            .unwrap()
            .iter()
            .all(|record| record["deleted"] == true)
    );
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 0);
    assert_eq!(usage(&app, API_KEY).await["availableBytes"], quota);
    let bytes = app
        .client
        .get(local(&app, descriptors[0]["downloadURL"].as_str().unwrap()))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(bytes.status(), 200);
    app.stop().await;
}

#[tokio::test]
async fn direct_reuse_obeys_atomic_and_concurrent_record_quota_and_partial_errors() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", "10")],
    ))
    .await;
    let lease = ready(&app, API_KEY).await;
    let (_, reserved) = reserve(&app, API_KEY, &lease, &request(&[6]), "object").await;
    let (status, published) = upload(&app, API_KEY, &lease, &reserved, b"shared").await;
    assert_eq!(status, 200);
    let asset = &published["singleFile"];
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 0);
    let baseline = snapshot(&app).await;
    let (status, rejected) = modify(
        &app,
        API_KEY,
        &lease,
        vec![file("batch-a", asset), file("batch-b", asset)],
        "batch",
        true,
    )
    .await;
    assert_eq!(status, 409, "{rejected}");
    assert_eq!(rejected["serverErrorCode"], "STORAGE_QUOTA_EXCEEDED");
    assert_eq!(snapshot(&app).await, baseline);
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 0);
    let (a, b) = tokio::join!(
        modify(&app, API_KEY, &lease, vec![file("a", asset)], "a", true),
        modify(&app, API_KEY, &lease, vec![file("b", asset)], "b", true)
    );
    let ((status, winner), name, loser) = if a.0 == 200 { (a, "a", b) } else { (b, "b", a) };
    assert_eq!(status, 200);
    assert_eq!(loser.0, 409);
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 6);
    let retry = modify(&app, API_KEY, &lease, vec![file(name, asset)], name, true).await;
    assert_eq!(retry.0, 200);
    assert_eq!(retry.1, winner);
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 6);
    let (status, partial) = modify(&app, API_KEY, &lease, vec![
        file("overflow", asset),
        json!({"operationType":"create","record":{"recordName":"album","recordType":"Album","fields":{}}}),
    ], "partial", false).await;
    assert_eq!(status, 200);
    assert_eq!(
        partial["records"][0]["serverErrorCode"],
        "STORAGE_QUOTA_EXCEEDED"
    );
    assert_eq!(partial["records"][1]["recordName"], "album");
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 6);
    assert_eq!(
        std::fs::read_dir(root.path().join("data/objects"))
            .unwrap()
            .count(),
        1
    );
    app.stop().await;
}

#[tokio::test]
async fn replacement_reserves_only_growth_and_over_quota_metadata_and_shrinking_remain_possible() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", "12")],
    ))
    .await;
    let lease = ready(&app, API_KEY).await;
    let mut descriptions = vec![];
    for (key, bytes) in [
        ("small", b"1234".as_slice()),
        ("large", b"12345678".as_slice()),
    ] {
        let (_, reserved) =
            reserve(&app, API_KEY, &lease, &request(&[bytes.len() as u64]), key).await;
        let (status, published) = upload(&app, API_KEY, &lease, &reserved, bytes).await;
        assert_eq!(status, 200);
        descriptions.push(published["singleFile"].clone());
    }
    let (status, created) = modify(
        &app,
        API_KEY,
        &lease,
        vec![
            file("small", &descriptions[0]),
            file("large", &descriptions[1]),
        ],
        "create",
        true,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 12);
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        200
    );
    let lease = common::ready(
        &app.client,
        &app.base,
        API_KEY,
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    assert_eq!(
        lease
            .control(
                &app.client,
                &app.base,
                API_KEY,
                "phase",
                json!({"phase":"modify"})
            )
            .await
            .0,
        200
    );
    let mut replace = request(&[8]);
    replace["tokens"][0]["recordName"] = json!("large");
    replace["tokens"][0]["_syncserver"]["intent"] = json!("update");
    let (status, reserved) = reserve(&app, API_KEY, &lease, &replace, "same-size").await;
    assert_eq!(status, 200, "{reserved}");
    assert_eq!(usage(&app, API_KEY).await["reservedBytes"], 0);
    let (status, published) = upload(&app, API_KEY, &lease, &reserved, b"87654321").await;
    assert_eq!(status, 200);
    let update = |record: &Value, asset: &Value| {
        json!({"operationType":"update","record":{
            "recordName":record["recordName"], "recordChangeTag":record["recordChangeTag"],
            "fields":{"resOriginalRes":{"type":"ASSETID","value":asset}}
        }})
    };
    let (status, same_size) = modify(
        &app,
        API_KEY,
        &lease,
        vec![update(&created["records"][1], &published["singleFile"])],
        "replace",
        true,
    )
    .await;
    assert_eq!(status, 200, "{same_size}");
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 12);
    let before = snapshot(&app).await;
    let (status, rejected) = modify(
        &app,
        API_KEY,
        &lease,
        vec![update(&created["records"][0], &descriptions[1])],
        "grow",
        true,
    )
    .await;
    assert_eq!(status, 409, "{rejected}");
    assert_eq!(snapshot(&app).await, before);
    let (status, shrunk) = modify(
        &app,
        API_KEY,
        &lease,
        vec![update(&same_size["records"][0], &descriptions[0])],
        "shrink",
        true,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 8);
    app.stop().await;
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", "6")],
    ))
    .await;
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 8);
    let (status, renamed) = common::fixture_modify(
        &app,
        json!({"operations":[{"operationType":"update","record":{
            "recordName":"large", "recordChangeTag":shrunk["records"][0]["recordChangeTag"],
            "fields":{"displayName":{"type":"STRING","value":"renamed"}}
        }}]}),
        Some("rename"),
    )
    .await;
    assert_eq!(status, 200, "{renamed}");
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 8);
    let (status, deleted) = common::fixture_modify(
        &app,
        json!({"operations":[{"operationType":"delete","record":{
            "recordName":"large", "recordChangeTag":renamed["records"][0]["recordChangeTag"]
        }}]}),
        Some("delete"),
    )
    .await;
    assert_eq!(status, 200, "{deleted}");
    assert_eq!(usage(&app, API_KEY).await["usedBytes"], 4);
    assert_eq!(usage(&app, API_KEY).await["availableBytes"], 2);
    app.stop().await;
}

fn media_pair(tag: &str, path: &str, original: &Value, thumbnail: &Value) -> Vec<Value> {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let fixture: Value =
        serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap();
    let generation = uuid::Uuid::new_v4().to_string();
    let master_name = format!("{tag}-master");
    let photo_name = format!("{tag}-photo");
    let digest: String = STANDARD
        .decode(original["fileChecksum"].as_str().unwrap())
        .unwrap()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let mut operations = fixture["createPhotoRequest"]["operations"]
        .as_array()
        .unwrap()
        .clone();
    for operation in &mut operations {
        let record = &mut operation["record"];
        record["fields"]["contentGeneration"]["value"] = json!(generation);
        if record["recordType"] == "CPLMaster" {
            record["recordName"] = json!(master_name);
            record["fields"]["photoId"]["value"] = json!(photo_name);
            record["fields"]["resOriginalRes"]["value"] = original.clone();
            record["fields"]["resJPEGThumbRes"]["value"] = thumbnail.clone();
            record["fields"]["thumbnailSourceOriginalSHA256"]["value"] = json!(digest);
        } else {
            record["recordName"] = json!(photo_name);
            record["fields"]["masterRef"]["value"]["recordName"] = json!(master_name);
            record["fields"]["relativePath"]["value"] = json!(path);
        }
    }
    operations
}

#[tokio::test]
async fn media_breakdown_attributes_shared_thumbnails_to_owners_and_reclassifies_metadata_only() {
    use base64::{Engine, engine::general_purpose::STANDARD};
    let root = tempfile::tempdir().unwrap();
    let config = common::config(root.path());
    let app = Running::start(config.clone()).await;
    let lease = ready(&app, API_KEY).await;
    let fixture: Value =
        serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap();
    let mut descriptions = vec![];
    for (index, item) in fixture["binaryAssets"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
    {
        let bytes = STANDARD
            .decode(item["bytesBase64"].as_str().unwrap())
            .unwrap();
        let mut body = request(&[bytes.len() as u64]);
        if item["role"] == "thumbnail" {
            body["tokens"][0]["fieldName"] = json!("resJPEGThumbRes");
            body["tokens"][0]["_syncserver"]["role"] = json!("thumbnail");
        }
        let (status, reserved) =
            reserve(&app, API_KEY, &lease, &body, &format!("media-{index}")).await;
        assert_eq!(status, 200);
        let (status, published) = upload(&app, API_KEY, &lease, &reserved, &bytes).await;
        assert_eq!(status, 200, "{published}");
        descriptions.push(published["singleFile"].clone());
    }
    let (_, reserved) = reserve(&app, API_KEY, &lease, &request(&[6]), "video-original").await;
    let (status, video) = upload(&app, API_KEY, &lease, &reserved, b"opaque").await;
    assert_eq!(status, 200);
    assert_eq!(video["singleFile"]["mimeType"], "application/octet-stream");
    let video = &video["singleFile"];
    let image_size = descriptions[0]["size"].as_u64().unwrap();
    let thumbnail_size = descriptions[1]["size"].as_u64().unwrap();
    let video_size = video["size"].as_u64().unwrap();
    // One shared image thumbnail belongs to an image, two video files and an
    // unknown file. Categories follow each owner, never the thumbnail's MIME.
    let mut operations = media_pair(
        "image",
        "pictures/照片.MOV",
        &descriptions[0],
        &descriptions[1],
    );
    operations.extend(media_pair(
        "video",
        "movies/片段.MP4",
        video,
        &descriptions[1],
    ));
    operations.extend(media_pair(
        "copy",
        "movies/copy.mkv",
        video,
        &descriptions[1],
    ));
    operations.extend(media_pair(
        "unknown",
        "misc/data.bin",
        video,
        &descriptions[1],
    ));
    let (status, created) = modify(&app, API_KEY, &lease, operations, "media-create", true).await;
    assert_eq!(status, 200, "{created}");
    let expected = usage(&app, API_KEY).await;
    assert_eq!(expected["imageBytes"], image_size + thumbnail_size);
    assert_eq!(expected["videoBytes"], 2 * (video_size + thumbnail_size));
    assert_eq!(expected["otherBytes"], video_size + thumbnail_size);
    let baseline = snapshot(&app).await;
    let before = get(&app, API_KEY, "/v1/metrics").await;
    for _ in 0..10 {
        assert_eq!(usage(&app, API_KEY).await, expected);
    }
    let after = get(&app, API_KEY, "/v1/metrics").await;
    for field in ["assetBytesRead", "assetBytesHashed"] {
        assert_eq!(before["metrics"][field], after["metrics"][field]);
    }
    assert_eq!(snapshot(&app).await, baseline);
    app.stop().await;
    let app = Running::start(config).await;
    assert_eq!(usage(&app, API_KEY).await, expected);
    assert_eq!(snapshot(&app).await, baseline);
    let photo = created["records"]
        .as_array()
        .unwrap()
        .iter()
        .find(|record| record["recordName"] == "video-photo")
        .unwrap();
    let (status, renamed) = common::fixture_modify(&app, json!({"operations":[{
        "operationType":"update", "record":{"recordName":"video-photo", "recordChangeTag":photo["recordChangeTag"],
            "fields":{"relativePath":{"type":"STRING","value":"pictures/reclassified.HEIC"}}}
    }]}), Some("reclassify")).await;
    assert_eq!(status, 200, "{renamed}");
    let changed = usage(&app, API_KEY).await;
    assert_eq!(changed["usedBytes"], expected["usedBytes"]);
    assert_eq!(
        changed["imageBytes"],
        image_size + video_size + 2 * thumbnail_size
    );
    assert_eq!(changed["videoBytes"], video_size + thumbnail_size);
    assert_eq!(changed["otherBytes"], video_size + thumbnail_size);
    app.stop().await;
}
