mod common;
use common::{API_KEY, Lease, Running};
use serde_json::{Value, json};
const CK: &str = "/database/1/com.apple.photos.cloud/production/private";
async fn snapshot(app: &Running, zone: &str) -> Value {
    app.client
        .post(format!("{}/v1/snapshots", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"zoneID":{"zoneName":zone}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
async fn modify(app: &Running, lease: Option<&Lease>, op: Value) -> (u16, Value) {
    let mut request = app
        .client
        .post(format!("{}{CK}/records/modify", app.base))
        .bearer_auth(API_KEY)
        .header("Idempotency-Key", "lease-test-create")
        .json(&json!({"operations":[op]}));
    if let Some(lease) = lease {
        request = lease.headers(request);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
fn create() -> Value {
    json!({"operationType":"create","record":{"recordName":"a","recordType":"Album","fields":{"title":{"type":"STRING","value":"a"}}}})
}
#[tokio::test]
async fn one_uploader_requires_current_progress_and_forward_phases_and_status_reads_need_no_lock() {
    let app = Running::new().await;
    assert_eq!(
        modify(&app, None, create()).await.1["serverErrorCode"],
        "UPLOAD_LEASE_LOST"
    );
    let zones = vec![json!({"zoneName":"PrimarySync"})];
    let (a, b) = tokio::join!(
        common::acquire(&app.client, &app.base, API_KEY, "a", zones.clone()),
        common::acquire(&app.client, &app.base, API_KEY, "b", zones)
    );
    assert!((a.0 == 200 && b.0 == 409) || (a.0 == 409 && b.0 == 200));
    let lease = Lease {
        body: if a.0 == 200 { a.1 } else { b.1 },
    };
    assert_eq!(
        modify(&app, Some(&lease), create()).await.1["serverErrorCode"],
        "SYNC_NOT_CURRENT"
    );
    let s = snapshot(&app, "PrimarySync").await;
    let check = json!({"zoneTokens":[{"zoneID":{"zoneName":"PrimarySync"},"syncToken":s["syncToken"]}],"localReady":false});
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "check", check.clone())
            .await
            .1["mayUpload"],
        false
    );
    let mut check = check;
    check["localReady"] = json!(true);
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "check", check)
            .await
            .1["mayUpload"],
        true
    );
    assert_eq!(
        modify(&app, Some(&lease), create()).await.1["serverErrorCode"],
        "UPLOAD_PHASE_MISMATCH"
    );
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
            .0,
        409
    );
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
    let first = modify(&app, Some(&lease), create()).await;
    assert_eq!(first.0, 200, "{}", first.1);
    assert_eq!(modify(&app, Some(&lease), create()).await.1, first.1);
    assert_eq!(
        lease
            .control(
                &app.client,
                &app.base,
                API_KEY,
                "phase",
                json!({"phase":"delete"})
            )
            .await
            .0,
        409
    );
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        200
    );
    assert_eq!(
        modify(&app, Some(&lease), create()).await.1["serverErrorCode"],
        "UPLOAD_LEASE_LOST"
    );
    let status: Value = app
        .client
        .get(format!("{}/v1/operations/lease-test-create", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["state"], "Committed");
    assert_eq!(status["result"], first.1);
    app.stop().await;
}
#[tokio::test]
async fn stale_progress_catches_up_while_holding_lease_and_other_zone_heads_do_not_block() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_ZONES", "PrimarySync,Other")],
    ))
    .await;
    let old = snapshot(&app, "PrimarySync").await;
    assert_eq!(
        common::fixture_modify(&app, json!({"operations":[create()]}), None)
            .await
            .0,
        200
    );
    let (_, body) = common::acquire(
        &app.client,
        &app.base,
        API_KEY,
        "catch-up",
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    let lease = Lease { body };
    let check = |token: Value| json!({"localReady":true,"zoneTokens":[{"zoneID":{"zoneName":"PrimarySync"},"syncToken":token}]});
    let caught = lease
        .control(
            &app.client,
            &app.base,
            API_KEY,
            "check",
            check(old["syncToken"].clone()),
        )
        .await;
    assert_eq!(caught.0, 200);
    assert_eq!(caught.1["isCurrent"], false);
    let response: Value = app
        .client
        .post(format!("{}{CK}/changes/zone", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"zones":[{"syncToken":old["syncToken"]}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        lease
            .control(&app.client, &app.base, API_KEY, "renew", json!({}))
            .await
            .0,
        200
    );
    assert_eq!(
        common::acquire(
            &app.client,
            &app.base,
            API_KEY,
            "competitor",
            vec![json!({"zoneName":"PrimarySync"})]
        )
        .await
        .0,
        409
    );
    assert_eq!(
        lease
            .control(
                &app.client,
                &app.base,
                API_KEY,
                "check",
                check(response["zones"][0]["syncToken"].clone())
            )
            .await
            .1["mayUpload"],
        true
    );
    lease
        .control(&app.client, &app.base, API_KEY, "release", json!({}))
        .await;
    let baseline = snapshot(&app, "PrimarySync").await;
    let mut other = create();
    other["record"]["zoneID"] = json!({"zoneName":"Other"});
    assert_eq!(
        common::fixture_modify(&app, json!({"operations":[other]}), None)
            .await
            .0,
        200
    );
    let (_, body) = common::acquire(
        &app.client,
        &app.base,
        API_KEY,
        "fresh",
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    let lease = Lease { body };
    assert_eq!(
        lease
            .control(
                &app.client,
                &app.base,
                API_KEY,
                "check",
                check(baseline["syncToken"].clone())
            )
            .await
            .1["mayUpload"],
        true
    );
    app.stop().await;
}
#[tokio::test]
async fn restart_invalidates_old_lease_and_advances_fence_but_preserves_metadata_tokens() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    let app = Running::start(cfg.clone()).await;
    let s = snapshot(&app, "PrimarySync").await;
    let (_, body) = common::acquire(
        &app.client,
        &app.base,
        API_KEY,
        "a",
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    let old = Lease { body };
    app.stop().await;
    let app = Running::start(cfg).await;
    assert_eq!(
        snapshot(&app, "PrimarySync").await["snapshotToken"],
        s["snapshotToken"]
    );
    assert_eq!(
        old.control(&app.client, &app.base, API_KEY, "renew", json!({}))
            .await
            .1["serverErrorCode"],
        "UPLOAD_LEASE_LOST"
    );
    let (_, body) = common::acquire(
        &app.client,
        &app.base,
        API_KEY,
        "b",
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    let new = Lease { body };
    assert!(
        new.body["fencingToken"]
            .as_str()
            .unwrap()
            .parse::<u64>()
            .unwrap()
            > old.body["fencingToken"]
                .as_str()
                .unwrap()
                .parse::<u64>()
                .unwrap()
    );
    assert_eq!(
        old.control(&app.client, &app.base, API_KEY, "release", json!({}))
            .await
            .0,
        409
    );
    assert_eq!(
        new.control(&app.client, &app.base, API_KEY, "renew", json!({}))
            .await
            .0,
        200
    );
    app.stop().await;
}
