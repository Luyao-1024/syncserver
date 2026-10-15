mod common;
use common::{API_KEY, Running};
use serde_json::{Value, json};
use std::time::Duration;

const CK: &str = "/database/1/com.apple.photos.cloud/production/private";

async fn metrics(app: &Running) -> Value {
    let response = app
        .client
        .get(format!("{}/v1/metrics", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.headers().contains_key("x-request-id"));
    response.json().await.unwrap()
}

#[tokio::test]
async fn metrics_require_bearer_and_poll_without_database_or_asset_work() {
    let app = Running::new().await;
    for key in [None, Some("wrong-key")] {
        let mut request = app.client.get(format!("{}/v1/metrics", app.base));
        if let Some(key) = key {
            request = request.bearer_auth(key);
        }
        assert_eq!(request.send().await.unwrap().status(), 401);
    }
    assert_eq!(
        app.client
            .get(format!("{}/v1/metrics?token=untrusted", app.base))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        app.client
            .get(format!("{}/v1/metrics", app.base))
            .bearer_auth(API_KEY)
            .header("Authorization", format!("Bearer {API_KEY}"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let first = metrics(&app).await;
    assert_eq!(first["version"], 1);
    assert_eq!(first["ready"], true);
    let completed = first["metrics"]["database"]["commandsCompleted"].clone();
    for _ in 0..20 {
        let current = metrics(&app).await;
        assert_eq!(current["runId"], first["runId"]);
        assert_eq!(
            current["metrics"]["database"]["commandsCompleted"],
            completed
        );
        for name in [
            "assetBytesRead",
            "assetBytesHashed",
            "uploadedBytes",
            "databaseChangePages",
            "zoneChangePages",
        ] {
            assert_eq!(current["metrics"][name], 0, "{name}");
        }
    }
    let caps: Value = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(caps["capabilities"]["operationalMetrics"], true);
    assert_eq!(caps["metrics"]["database"]["commandsCompleted"], completed);
    let wire = first.to_string();
    for secret in [
        API_KEY,
        app.base.as_str(),
        "leaseToken",
        "signing",
        "recordName",
        "syncToken",
    ] {
        assert!(!wire.contains(secret), "metrics disclosed {secret}");
    }
    app.stop().await;
}

#[tokio::test]
async fn change_page_counters_cover_aliases_and_exclude_per_zone_errors() {
    let app = Running::new().await;
    let operations: Vec<_> = ["a", "b", "c"].into_iter().map(|name| json!({
        "operationType":"create", "record":{"recordName":name, "recordType":"Album", "fields":{}}
    })).collect();
    assert_eq!(
        common::fixture_modify(&app, json!({"operations":operations}), None)
            .await
            .0,
        200
    );
    let mut token = None;
    for (index, alias) in ["changes/zone", "records/changes", "zones/changes"]
        .into_iter()
        .enumerate()
    {
        let mut zone = json!({"zoneID":{"zoneName":"PrimarySync"},"resultsLimit":1});
        if let Some(token) = token {
            zone["syncToken"] = token;
        }
        let body: Value = app
            .client
            .post(format!("{}{CK}/{alias}", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({"zones":[zone,{"zoneID":{"zoneName":"unknown"}}]}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(body["zones"][0]["records"].as_array().unwrap().len(), 1);
        assert_eq!(body["zones"][0]["moreComing"], index < 2);
        assert!(body["zones"][1].get("serverErrorCode").is_some());
        token = Some(body["zones"][0]["syncToken"].clone());
        assert_eq!(metrics(&app).await["metrics"]["zoneChangePages"], index + 1);
    }
    for _ in 0..2 {
        assert_eq!(
            app.client
                .post(format!("{}{CK}/changes/database", app.base))
                .bearer_auth(API_KEY)
                .json(&json!({}))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    assert_eq!(
        app.client
            .post(format!("{}{CK}/changes/database", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({"syncToken":"invalid"}))
            .send()
            .await
            .unwrap()
            .status(),
        400
    );
    let snapshot = metrics(&app).await;
    assert_eq!(snapshot["metrics"]["databaseChangePages"], 2);
    assert_eq!(snapshot["metrics"]["zoneChangePages"], 3);
    app.stop().await;
}

#[tokio::test]
async fn saturated_request_and_lease_admission_leave_monitoring_available() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[
            ("SYNC_DB_QUEUE_CAPACITY", "1"),
            ("SYNC_CONTROL_QUEUE_CAPACITY", "1"),
        ],
    ))
    .await;
    let mut held = Vec::new();
    let mut producers = Vec::new();
    for path in ["/v1/snapshots", "/v1/upload-leases"] {
        let (producer, receiver) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(1);
        producer.send(Ok("{".into())).await.unwrap();
        let stream = futures_util::stream::unfold(receiver, |mut receiver| async {
            receiver.recv().await.map(|chunk| (chunk, receiver))
        });
        let request = app
            .client
            .post(format!("{}{path}", app.base))
            .bearer_auth(API_KEY)
            .header("Content-Type", "application/json")
            .body(reqwest::Body::wrap_stream(stream));
        held.push(tokio::spawn(request.send()));
        producers.push(producer);
    }
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let value = metrics(&app).await;
            if value["metrics"]["httpAdmission"]["active"] == 1
                && value["metrics"]["httpAdmission"]["controlActive"] == 1
            {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        app.client
            .get(format!("{}/v1/capabilities", app.base))
            .bearer_auth(API_KEY)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        app.client
            .post(format!("{}/v1/upload-leases", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({}))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(metrics(&app).await["metrics"]["requestRejections"], 2);
    drop(producers);
    for task in held {
        assert_eq!(task.await.unwrap().unwrap().status(), 400);
    }
    app.stop().await;
}

#[tokio::test]
async fn restart_changes_metric_run_and_resets_process_counters() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config(root.path());
    let app = Running::start(config.clone()).await;
    let before = metrics(&app).await;
    app.client
        .post(format!("{}{CK}/changes/database", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    assert_eq!(metrics(&app).await["metrics"]["databaseChangePages"], 1);
    app.stop().await;
    let app = Running::start(config).await;
    let after = metrics(&app).await;
    assert_ne!(before["runId"], after["runId"]);
    assert_eq!(after["metrics"]["databaseChangePages"], 0);
    assert_eq!(after["metrics"]["requests"], 1);
    app.stop().await;
}

#[tokio::test]
async fn deployment_smoke_reads_real_metrics_for_each_configured_zone() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_ZONES", "PrimarySync,Other")],
    ))
    .await;
    let base = app.base.clone();
    let key_file = root.path().join("api.key");
    let result = tokio::task::spawn_blocking(move || {
        std::process::Command::new("python3")
            .arg(concat!(env!("CARGO_MANIFEST_DIR"), "/tools/smoke.py"))
            .arg(base)
            .arg("--api-key-file")
            .arg(key_file)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = String::from_utf8(result.stdout).unwrap();
    assert!(output.contains("operational metrics, 2 zone snapshots and delta reads"));
    assert!(!output.contains(API_KEY));
    app.stop().await;
}
