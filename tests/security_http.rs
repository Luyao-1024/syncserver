mod common;
use common::{API_KEY, Running};
use serde_json::{Value, json};
#[tokio::test]
async fn unreferenced_objects_are_bounded_independently_of_logical_usage() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[
            ("SYNC_USER_STORAGE_QUOTA_BYTES", "10"),
            ("SYNC_USER_PHYSICAL_STORAGE_QUOTA_BYTES", "24"),
        ],
    ))
    .await;
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
    for i in 0..3 {
        let response=lease.headers(app.client.post(format!("{}/database/1/com.apple.photos.cloud/production/private/assets/upload",app.base)).bearer_auth(API_KEY).json(&json!({"tokens":[{"recordName":format!("unbound-{i}"),"recordType":"CPLMaster","fieldName":"resOriginalRes","_syncserver":{"role":"original","intent":"create","expectedSize":8}}]}))).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let reservation: Value = response.json().await.unwrap();
        let url = url::Url::parse(reservation["tokens"][0]["url"].as_str().unwrap()).unwrap();
        let response = lease
            .headers(
                app.client
                    .post(format!(
                        "{}{}?{}",
                        app.base,
                        url.path(),
                        url.query().unwrap()
                    ))
                    .bearer_auth(API_KEY)
                    .header("Content-Type", "application/octet-stream")
                    .body(vec![i as u8; 8]),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
    }
    let response=lease.headers(app.client.post(format!("{}/database/1/com.apple.photos.cloud/production/private/assets/upload",app.base)).bearer_auth(API_KEY).json(&json!({"tokens":[{"recordName":"over-physical-limit","recordType":"CPLMaster","fieldName":"resOriginalRes","_syncserver":{"role":"original","intent":"create","expectedSize":1}}]}))).send().await.unwrap();
    assert_eq!(response.status(), 507);
    let body: Value = response.json().await.unwrap();
    assert_eq!(body["serverErrorCode"], "PHYSICAL_STORAGE_QUOTA_EXCEEDED");
    let usage: Value = app
        .client
        .get(format!("{}/v1/storage", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let bytes: u64 = std::fs::read_dir(root.path().join("data/objects"))
        .unwrap()
        .map(|entry| entry.unwrap().metadata().unwrap().len())
        .sum();
    println!(
        "PHYSICAL_QUOTA_EVIDENCE configured_quota=10 physical_object_bytes={bytes} used={} reserved={}",
        usage["usedBytes"], usage["reservedBytes"]
    );
    app.stop().await;
    assert_eq!(bytes, 24);
    assert_eq!(usage["usedBytes"], 0);
    assert_eq!(usage["reservedBytes"], 0);
    let restarted = Running::start(common::config_with(
        root.path(),
        &[
            ("SYNC_USER_STORAGE_QUOTA_BYTES", "10"),
            ("SYNC_USER_PHYSICAL_STORAGE_QUOTA_BYTES", "24"),
        ],
    ))
    .await;
    let lease = upload_lease(&restarted).await;
    assert_eq!(
        reserve(&restarted, &lease, "after-restart", 1)
            .await
            .status(),
        507
    );
    restarted.stop().await;
}

#[tokio::test]
async fn slow_login_bodies_do_not_hold_password_verification_slots() {
    use tokio::io::AsyncWriteExt;
    let app = Running::new().await;
    let mut bodies = Vec::new();
    for _ in 0..2 {
        let mut stream = tokio::net::TcpStream::connect(app.base.trim_start_matches("http://"))
            .await
            .unwrap();
        stream.write_all(b"POST /v1/auth/login HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n{").await.unwrap();
        bodies.push(stream);
    }
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let response = app
        .client
        .post(format!("{}/v1/auth/login", app.base))
        .json(&json!({"username":"synthetic-missing","password":"synthetic-password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    drop(bodies);
    app.stop().await;
}
#[tokio::test]
async fn forwarded_source_is_only_accepted_from_explicitly_trusted_proxies() {
    for trusted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let app = Running::start(common::config_with(
            root.path(),
            &[(
                "SYNC_TRUSTED_PROXY_IPS",
                if trusted { "127.0.0.1" } else { "" },
            )],
        ))
        .await;
        for _ in 0..32 {
            app.client
                .post(format!("{}/v1/auth/login", app.base))
                .header("x-forwarded-for", "192.0.2.1")
                .body("{}")
                .header("content-type", "application/json")
                .send()
                .await
                .unwrap();
        }
        let r = app
            .client
            .post(format!("{}/v1/auth/login", app.base))
            .header("x-forwarded-for", "192.0.2.2")
            .body("{}")
            .header("content-type", "application/json")
            .send()
            .await
            .unwrap();
        assert_eq!(r.status().as_u16(), if trusted { 400 } else { 429 });
        app.stop().await;
    }
}

async fn upload_lease(app: &Running) -> common::Lease {
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
async fn reserve(app: &Running, lease: &common::Lease, name: &str, size: u64) -> reqwest::Response {
    lease.headers(app.client.post(format!("{}/database/1/com.apple.photos.cloud/production/private/assets/upload",app.base)).bearer_auth(API_KEY)
      .json(&json!({"tokens":[{"recordName":name,"recordType":"CPLMaster","fieldName":"resOriginalRes","_syncserver":{"role":"original","intent":"create","expectedSize":size}}]}))).send().await.unwrap()
}
#[tokio::test]
async fn physical_reservations_are_atomic_and_failed_bodies_release_capacity() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_PHYSICAL_STORAGE_QUOTA_BYTES", "16")],
    ))
    .await;
    let lease = upload_lease(&app).await;
    let (a, b, c) = tokio::join!(
        reserve(&app, &lease, "one", 8),
        reserve(&app, &lease, "two", 8),
        reserve(&app, &lease, "three", 8)
    );
    let mut accepted = Vec::new();
    let mut denied = 0;
    for response in [a, b, c] {
        if response.status() == 200 {
            accepted.push(response.json::<Value>().await.unwrap());
        } else {
            assert_eq!(response.status(), 507);
            denied += 1;
        }
    }
    assert_eq!(accepted.len(), 2);
    assert_eq!(denied, 1);
    let url = url::Url::parse(accepted[0]["tokens"][0]["url"].as_str().unwrap()).unwrap();
    let failed = lease
        .headers(
            app.client
                .post(format!(
                    "{}{}?{}",
                    app.base,
                    url.path(),
                    url.query().unwrap()
                ))
                .header("content-type", "application/octet-stream")
                .body(vec![1; 4]),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), 400);
    assert_eq!(reserve(&app, &lease, "replacement", 8).await.status(), 200);
    app.stop().await;
}
#[tokio::test]
async fn physical_admission_preserves_the_configured_free_space_reserve() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_MIN_FREE_DISK_BYTES", "9223372036854775807")],
    ))
    .await;
    let lease = upload_lease(&app).await;
    let response = reserve(&app, &lease, "no-space", 8).await;
    assert_eq!(response.status(), 507);
    assert_eq!(
        response.json::<Value>().await.unwrap()["serverErrorCode"],
        "INSUFFICIENT_STORAGE"
    );
    app.stop().await;
}
