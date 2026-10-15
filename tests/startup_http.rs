mod common;
use common::{API_KEY, Running};
use serde_json::{Value, json};

#[tokio::test]
async fn health_readiness_authentication_and_actual_capabilities() {
    let app = Running::new().await;
    let health = app
        .client
        .get(format!("{}/healthz", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(
        health.json::<Value>().await.unwrap(),
        json!({"status":"alive"})
    );
    let ready = app
        .client
        .get(format!("{}/readyz", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(ready.status(), 200);
    assert_eq!(ready.json::<Value>().await.unwrap()["status"], "ready");
    for auth in [
        None,
        Some("Bearer wrong"),
        Some("Basic irrelevant"),
        Some("Bearer "),
    ] {
        let mut request = app.client.get(format!("{}/v1/capabilities", app.base));
        if let Some(auth) = auth {
            request = request.header("Authorization", auth);
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["www-authenticate"], "Bearer");
        assert_eq!(
            response.json::<Value>().await.unwrap()["serverErrorCode"],
            "AUTHENTICATION_REQUIRED"
        );
    }
    let response = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    assert!(response.headers().contains_key("x-request-id"));
    let value: Value = response.json().await.unwrap();
    assert_eq!(value["stage"], "M4");
    assert_eq!(value["namespace"]["database"], "private");
    assert_eq!(value["zones"], json!(["PrimarySync"]));
    assert_eq!(value["capabilities"]["zoneListing"], true);
    assert_eq!(value["capabilities"]["recordCrud"], true);
    for feature in [
        "snapshots",
        "incrementalChanges",
        "uploadLeases",
        "fencing",
        "uploadPhases",
        "operationStatusRead",
        "assetUpload",
        "assetDownload",
        "photoPublication",
    ] {
        assert_eq!(value["capabilities"][feature], true, "{feature}");
    }
    assert_eq!(value["capabilities"]["appleAuthentication"], false);
    assert_eq!(value["limits"]["maxAssetBytes"], 536870912);
    assert_eq!(value["limits"]["thumbnailMaxBytes"], 2097152);
    assert_eq!(value["limits"]["dbQueueCapacity"], 128);
    assert_eq!(value["metrics"]["assetBytesRead"], 0);
    assert_eq!(value["metrics"]["assetBytesHashed"], 0);
    assert!(!value.to_string().contains(API_KEY));
    app.stop().await;
}

#[tokio::test]
async fn cloudkit_scope_json_content_types_and_explicit_unsupported_errors() {
    let app = Running::new().await;
    let path = "/database/1/com.apple.photos.cloud/production/private";
    for content_type in [
        "application/json",
        "text/plain",
        "text/plain; charset=UTF-8",
    ] {
        let response = app
            .client
            .post(format!("{}{path}/zones/list", app.base))
            .bearer_auth(API_KEY)
            .header("Content-Type", content_type)
            .body("{}")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(
            response.json::<Value>().await.unwrap(),
            json!({"zones":[{"zoneID":{"zoneName":"PrimarySync", "ownerRecordName":"_defaultOwner"}}]})
        );
    }
    for (suffix, code, status) in [
        ("other/production/private/zones/list", "UNKNOWN_ITEM", 404),
        (
            "com.apple.photos.cloud/development/private/zones/list",
            "UNSUPPORTED_CAPABILITY",
            501,
        ),
        (
            "com.apple.photos.cloud/production/public/zones/list",
            "UNSUPPORTED_CAPABILITY",
            501,
        ),
        (
            "com.apple.photos.cloud/production/shared/zones/list",
            "UNSUPPORTED_CAPABILITY",
            501,
        ),
        (
            "com.apple.photos.cloud/production/private/records/query",
            "BAD_REQUEST",
            400,
        ),
        (
            "com.apple.photos.cloud/production/private/internal/records/query/batch",
            "BAD_REQUEST",
            400,
        ),
    ] {
        let response = app
            .client
            .post(format!("{}/database/1/{suffix}", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
        assert_eq!(
            response.json::<Value>().await.unwrap()["serverErrorCode"],
            code
        );
    }
    for endpoint in ["/v1/uploads/future/unknown-operation"] {
        let response = app
            .client
            .post(format!("{}{endpoint}", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 501);
        assert_eq!(
            response.json::<Value>().await.unwrap()["serverErrorCode"],
            "UNSUPPORTED_CAPABILITY"
        );
    }
    for body in ["not-json", "[]", "{\"ignoredOption\":true}"] {
        let response = app
            .client
            .post(format!("{}{path}/zones/list", app.base))
            .bearer_auth(API_KEY)
            .header("Content-Type", "text/plain")
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400);
    }
    let response = app
        .client
        .post(format!("{}{path}/zones/list", app.base))
        .bearer_auth(API_KEY)
        .header("Content-Type", "image/png")
        .body("{}")
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 415);
    let response = app
        .client
        .post(format!("{}{path}/zones/list", app.base))
        .bearer_auth(API_KEY)
        .header("Content-Type", "text/plain")
        .body(" ".repeat(1048577))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 413);
    app.stop().await;
}

#[tokio::test]
async fn readiness_reflects_executor_shutdown_and_liveness_remains_available() {
    let root = tempfile::tempdir().unwrap();
    let server = syncserver::Server::open(common::config(root.path()))
        .await
        .unwrap();
    let store = server.store();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.serve(listener, async {
        let _ = stopped.await;
    }));
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    store.shutdown().await.unwrap();
    let diagnostic: Value = client
        .get(format!("{base}/v1/metrics"))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(diagnostic["ready"], false);
    assert_eq!(diagnostic["metrics"]["database"]["queueDepth"], 0);
    assert_eq!(
        client
            .get(format!("{base}/v1/metrics"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    assert_eq!(
        client
            .get(format!("{base}/healthz"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(format!("{base}/v1/capabilities"))
            .bearer_auth(API_KEY)
            .send()
            .await
            .unwrap()
            .status(),
        503
    );
    stop.send(()).unwrap();
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn configured_limits_apply_to_chunked_bodies_and_host_cannot_change_public_url() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[
            ("SYNC_JSON_BODY_LIMIT", "16"),
            ("SYNC_DB_QUEUE_CAPACITY", "2"),
        ],
    ))
    .await;
    let value: Value = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(API_KEY)
        .header("Host", "untrusted.example.org")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(value["limits"]["jsonBodyLimit"], 16);
    assert_eq!(value["limits"]["dbQueueCapacity"], 2);
    assert_eq!(value["publicBaseURL"], "http://127.0.0.1:8790/");
    let address = app.base.strip_prefix("http://").unwrap().to_owned();
    let response = tokio::task::spawn_blocking(move || {
        use std::io::{Read, Write};
        let mut stream = std::net::TcpStream::connect(address).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        // No Content-Length: exercise the actual streaming limit.
        let request = format!("POST /database/1/com.apple.photos.cloud/production/private/zones/list HTTP/1.1\r\nHost: local\r\nAuthorization: Bearer {API_KEY}\r\nContent-Type: text/plain\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n20\r\n{}\r\n0\r\n\r\n", " ".repeat(32));
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new(); stream.read_to_string(&mut response).unwrap(); response
    }).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 413"));
    assert!(response.contains("LIMIT_EXCEEDED"));
    app.stop().await;
}
