mod common;
use common::{API_KEY, Running};
use serde_json::{Value, json};
const CK: &str = "/database/1/com.apple.photos.cloud/production/private";

async fn post(app: &Running, op: &str, value: Value) -> (u16, Value) {
    if op == "records/modify" {
        return common::fixture_modify(app, value, None).await;
    }
    let path = if op.starts_with('/') {
        op.to_owned()
    } else {
        format!("{CK}/{op}")
    };
    let response = app
        .client
        .post(format!("{}{path}", app.base))
        .bearer_auth(API_KEY)
        .header("Content-Type", "text/plain; charset=UTF-8")
        .body(value.to_string())
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
fn create(name: &str, kind: &str, zone: &str) -> Value {
    json!({"operationType":"create","record":{"recordName":name,"recordType":kind,"zoneID":{"zoneName":zone},"fields":{"title":{"type":"STRING","value":name}}}})
}
async fn modify(app: &Running, operations: Vec<Value>) -> Value {
    let (status, body) = post(app, "records/modify", json!({"operations":operations})).await;
    assert_eq!(status, 200, "{body}");
    body
}
async fn zone(app: &Running, token: Option<&Value>, limit: usize) -> Value {
    let mut request = json!({"zones":[{"zoneID":{"zoneName":"PrimarySync"},"resultsLimit":limit}]});
    if let Some(token) = token {
        request["zones"][0]["syncToken"] = token.clone();
    }
    let (status, body) = post(app, "changes/zone", request).await;
    assert_eq!(status, 200, "{body}");
    body["zones"][0].clone()
}

#[tokio::test]
async fn shared_snapshot_keeps_history_during_mutations_and_resumes_at_its_watermark() {
    let app = Running::new().await;
    let first = modify(
        &app,
        vec![
            create("a", "Album", "PrimarySync"),
            create("b", "Album", "PrimarySync"),
            create("master", "Other", "PrimarySync"),
        ],
    )
    .await;
    let (status, snapshot) = post(&app, "/v1/snapshots", json!({})).await;
    assert_eq!(status, 200, "{snapshot}");
    let query = json!({"snapshotToken":snapshot["snapshotToken"],"query":{"recordType":"Album"},"resultsLimit":1});
    let (status, page) = post(&app, "records/query", query.clone()).await;
    assert_eq!(status, 200, "{page}");
    assert_eq!(page["records"][0]["recordName"], "a");
    modify(&app,vec![json!({"operationType":"delete","record":{"recordName":"b","recordChangeTag":first["records"][1]["recordChangeTag"]}})]).await;
    modify(&app,vec![json!({"operationType":"update","record":{"recordName":"master","recordChangeTag":first["records"][2]["recordChangeTag"],"fields":{"title":{"type":"STRING","value":"new"}}}})]).await;
    modify(&app, vec![create("c", "Album", "PrimarySync")]).await;
    let mut next = query;
    next["continuationMarker"] = page["continuationMarker"].clone();
    next["resultsLimit"] = json!(200);
    let (_, page2) = post(&app, "records/query", next).await;
    assert_eq!(page2["records"].as_array().unwrap().len(), 1);
    assert_eq!(page2["records"][0]["recordName"], "b");
    assert!(page2.get("continuationMarker").is_none());
    assert_eq!(page2["syncToken"], snapshot["syncToken"]);
    let (_, other) = post(
        &app,
        "records/query",
        json!({"snapshotToken":snapshot["snapshotToken"],"query":{"recordType":"Other"}}),
    )
    .await;
    assert_eq!(other["records"][0]["fields"]["title"]["value"], "master");
    assert_eq!(other["syncToken"], page2["syncToken"]);
    let (_, old) = post(&app,"records/lookup",json!({"snapshotToken":snapshot["snapshotToken"],"records":[{"recordName":"b"},{"recordName":"c"}]})).await;
    assert_eq!(old["records"][0]["recordName"], "b");
    assert_eq!(old["records"][1]["serverErrorCode"], "UNKNOWN_ITEM");
    let delta = zone(&app, Some(&snapshot["syncToken"]), 200).await;
    assert_eq!(delta["records"].as_array().unwrap().len(), 3);
    assert_eq!(delta["records"][0]["deleted"], true);
    assert!(delta["records"][0]["_syncserver"]["deletionEvent"]["editedAtMs"].is_i64());
    let (_, current) = post(
        &app,
        "records/query",
        json!({"query":{"recordType":"Album"}}),
    )
    .await;
    assert_eq!(
        current["records"]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["recordName"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["a", "c"]
    );
    app.stop().await;
}

#[tokio::test]
async fn delta_pages_keep_fixed_head_across_restart_and_new_atomic_writes() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    let app = Running::start(cfg.clone()).await;
    let initial = zone(&app, None, 1).await;
    assert_eq!(initial["records"], json!([]));
    modify(
        &app,
        vec![
            create("a", "Album", "PrimarySync"),
            create("b", "Album", "PrimarySync"),
        ],
    )
    .await;
    let page = zone(&app, Some(&initial["syncToken"]), 1).await;
    assert_eq!(page["moreComing"], true);
    modify(&app, vec![create("c", "Album", "PrimarySync")]).await;
    app.stop().await;
    let app = Running::start(cfg).await;
    let second = zone(&app, Some(&page["syncToken"]), 200).await;
    assert_eq!(second["moreComing"], false);
    assert_eq!(second["records"].as_array().unwrap().len(), 1);
    assert_eq!(second["records"][0]["recordName"], "b");
    let third = zone(&app, Some(&second["syncToken"]), 200).await;
    assert_eq!(third["records"][0]["recordName"], "c");
    for _ in 0..100 {
        let empty = zone(&app, Some(&third["syncToken"]), 200).await;
        assert_eq!(empty["records"], json!([]));
        assert_eq!(empty["syncToken"], third["syncToken"]);
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
    assert_eq!(caps["stage"], "M4");
    assert_eq!(caps["metrics"]["assetBytesRead"], 0);
    assert_eq!(caps["metrics"]["assetBytesHashed"], 0);
    assert_eq!(caps["capabilities"]["uploadLeases"], true);
    app.stop().await;
}

#[tokio::test]
async fn database_and_empty_zone_progress_do_not_omit_repeated_zones() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_ZONES", "PrimarySync,Other")],
    ))
    .await;
    let (_, db) = post(&app, "changes/database", json!({})).await;
    assert_eq!(db["zones"].as_array().unwrap().len(), 2);
    let empty = zone(&app, None, 200).await;
    modify(
        &app,
        vec![
            create("a", "Album", "PrimarySync"),
            create("b", "Album", "PrimarySync"),
        ],
    )
    .await;
    modify(&app, vec![create("c", "Album", "Other")]).await;
    let (_, page) = post(
        &app,
        "changes/database",
        json!({"syncToken":db["syncToken"],"resultsLimit":1}),
    )
    .await;
    assert_eq!(page["moreComing"], true);
    modify(&app, vec![create("d", "Album", "Other")]).await;
    let (_, page2) = post(
        &app,
        "changes/database",
        json!({"syncToken":page["syncToken"],"resultsLimit":1}),
    )
    .await;
    assert_eq!(page2["zones"][0]["zoneID"]["zoneName"], "PrimarySync");
    assert_eq!(page2["moreComing"], true);
    let (_, page3) = post(
        &app,
        "changes/database",
        json!({"syncToken":page2["syncToken"],"resultsLimit":200}),
    )
    .await;
    assert_eq!(page3["moreComing"], false);
    assert_eq!(page3["zones"][0]["zoneID"]["zoneName"], "Other");
    let (_, page4) = post(
        &app,
        "changes/database",
        json!({"syncToken":page3["syncToken"]}),
    )
    .await;
    assert_eq!(page4["zones"][0]["zoneID"]["zoneName"], "Other");
    let caught = zone(&app, Some(&empty["syncToken"]), 200).await;
    assert_eq!(caught["records"].as_array().unwrap().len(), 2);
    modify(&app, vec![create("e", "Album", "Other")]).await;
    let advanced = zone(&app, Some(&caught["syncToken"]), 200).await;
    assert_eq!(advanced["records"], json!([]));
    assert_eq!(advanced["moreComing"], false);
    let unchanged = zone(&app, Some(&advanced["syncToken"]), 200).await;
    assert_eq!(unchanged["syncToken"], advanced["syncToken"]);
    app.stop().await;
}

#[tokio::test]
async fn tokens_bind_kind_zone_scope_and_query_parameters() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_ZONES", "PrimarySync,Other")],
    ))
    .await;
    modify(
        &app,
        vec![
            create("a", "Album", "PrimarySync"),
            create("b", "Album", "PrimarySync"),
        ],
    )
    .await;
    let (_, s) = post(&app, "/v1/snapshots", json!({})).await;
    let query = json!({"snapshotToken":s["snapshotToken"],"query":{"recordType":"Album"},"resultsLimit":1,"desiredKeys":["title"]});
    let (_, q) = post(&app, "records/query", query.clone()).await;
    for body in [
        json!({"zones":[{"syncToken":s["snapshotToken"]}]}),
        json!({"zones":[{"zoneID":{"zoneName":"Other"},"syncToken":s["syncToken"]}]}),
        json!({"zones":[{"syncToken":q["continuationMarker"]}]}),
    ] {
        let (_, v) = post(&app, "changes/zone", body).await;
        assert_eq!(v["zones"][0]["serverErrorCode"], "BAD_REQUEST", "{v}");
    }
    let mut bad = query.clone();
    bad["continuationMarker"] = q["continuationMarker"].clone();
    bad["desiredKeys"] = json!([]);
    assert_eq!(post(&app, "records/query", bad).await.0, 400);
    let mut changed = query.clone();
    changed["continuationMarker"] = q["continuationMarker"].clone();
    changed["query"]["recordType"] = json!("Other");
    assert_eq!(post(&app, "records/query", changed).await.0, 400);
    let mut damaged = s["snapshotToken"].as_str().unwrap().as_bytes().to_vec();
    damaged[10] = if damaged[10] == b'A' { b'B' } else { b'A' };
    assert_eq!(post(&app,"records/query",json!({"snapshotToken":String::from_utf8(damaged).unwrap(),"query":{"recordType":"Album"}})).await.0,400);
    assert_eq!(
        post(
            &app,
            "changes/database",
            json!({"syncToken":s["syncToken"]})
        )
        .await
        .0,
        400
    );
    let (_, filtered) = post(
        &app,
        "records/query",
        json!({"query":{"recordType":"Album"},"desiredKeys":[]}),
    )
    .await;
    assert_eq!(filtered["records"][0]["fields"], json!({}));
    assert!(filtered["records"][0]["_syncserver"].is_object());
    app.stop().await;
}

#[tokio::test]
async fn event_times_survive_metadata_changes_and_tombstones_without_using_upload_time() {
    let app = Running::new().await;
    let event = json!({"editedAtMs":123,"source":"device-a","eventId":"edit-1"});
    let mut op = create("a", "Album", "PrimarySync");
    op["record"]["_syncserver"] = json!({"metadataEvent":event});
    let first = modify(&app, vec![op]).await;
    assert_eq!(first["records"][0]["_syncserver"]["metadataEvent"], event);
    let deleted=modify(&app,vec![json!({"operationType":"delete","record":{"recordName":"a","recordChangeTag":first["records"][0]["recordChangeTag"],"_syncserver":{"deletionEvent":{"editedAtMs":100,"source":"device-b","eventId":"delete-1"}}}})]).await;
    assert_eq!(deleted["records"][0]["_syncserver"]["metadataEvent"], event);
    assert_eq!(
        deleted["records"][0]["_syncserver"]["deletionEvent"]["editedAtMs"],
        100
    );
    let all = zone(&app, None, 200).await;
    assert_eq!(all["records"][0]["_syncserver"]["metadataEvent"], event);
    assert_eq!(all["records"][1], deleted["records"][0]);
    let (_,bad)=post(&app,"records/modify",json!({"operations":[{"operationType":"create","record":{"recordName":"bad","recordType":"Album","_syncserver":{"metadataEvent":{"editedAtMs":-1,"source":"a","eventId":"b"}}}}]})).await;
    assert_eq!(bad["serverErrorCode"], "BAD_REQUEST");
    app.stop().await;
}

#[tokio::test]
async fn malformed_requests_and_unknown_query_features_are_explicit() {
    let app = Running::new().await;
    for (op, body, status) in [
        (
            "records/query",
            json!({"query":{"recordType":"Album","filterBy":[{"fieldName":"title"}]}}),
            501,
        ),
        (
            "records/query",
            json!({"query":{"recordType":"Album","sortBy":[{"fieldName":"title"}]}}),
            501,
        ),
        (
            "records/query",
            json!({"query":{"recordType":"Album"},"resultsLimit":201}),
            400,
        ),
        (
            "records/query",
            json!({"query":{"recordType":"Album"},"resultsLimit":0}),
            400,
        ),
        ("changes/zone", json!({"zones":[]}), 400),
        ("changes/zone", json!({"zones":[{},{}]}), 400),
        ("changes/database", json!({"resultsLimit":0}), 400),
        (
            "/v1/snapshots",
            json!({"zoneID":{"zoneName":"unknown"}}),
            404,
        ),
    ] {
        let result = post(&app, op, body).await;
        assert_eq!(result.0, status, "{}", result.1);
    }
    let (_, body) = post(
        &app,
        "changes/zone",
        json!({"zones":[{"zoneID":{"zoneName":"unknown"}},{}]}),
    )
    .await;
    assert_eq!(body["zones"][0]["serverErrorCode"], "UNKNOWN_ITEM");
    assert_eq!(body["zones"][1]["moreComing"], false);
    app.stop().await;
}

#[tokio::test]
async fn aliases_batches_and_documented_hints_share_fixed_scope_without_type_filtering() {
    let app = Running::new().await;
    modify(
        &app,
        vec![
            create("a", "Album", "PrimarySync"),
            create("b", "Other", "PrimarySync"),
        ],
    )
    .await;
    let (_, snapshot) = post(&app, "/v1/snapshots", json!({})).await;
    let (status,batch)=post(&app,"internal/records/query/batch",json!({"batch":[{"snapshotToken":snapshot["snapshotToken"],"query":{"recordType":"Album"}},{"snapshotToken":snapshot["snapshotToken"],"query":{"recordType":"Other"}}],"clientId":"test-device","remapEnums":true})).await;
    assert_eq!(status, 200, "{batch}");
    assert_eq!(
        batch["batch"][0]["syncToken"],
        batch["batch"][1]["syncToken"]
    );
    assert_eq!(batch["batch"][1]["records"][0]["recordName"], "b");
    for path in ["changes/zone", "records/changes", "zones/changes"] {
        let(status,result)=post(&app,path,json!({"zones":[{"desiredRecordTypes":["Album"]}],"clientId":"test-device","remapEnums":false})).await;
        assert_eq!(status, 200, "{result}");
        assert_eq!(result["zones"][0]["records"].as_array().unwrap().len(), 2);
    }
    app.stop().await;
}

#[tokio::test]
async fn response_byte_budget_shortens_pages_without_omitting_records() {
    let app = Running::new().await;
    let text = "x".repeat(900_000);
    for index in 0..10 {
        let mut op = create(&format!("large-{index:02}"), "Album", "PrimarySync");
        op["record"]["fields"]["title"]["value"] = json!(text);
        modify(&app, vec![op]).await;
    }
    let (status, first) = post(
        &app,
        "records/query",
        json!({"query":{"recordType":"Album"},"resultsLimit":200}),
    )
    .await;
    assert_eq!(status, 200, "{first}");
    assert!(first["records"].as_array().unwrap().len() < 10);
    assert!(first.to_string().len() <= 8_388_608);
    let(status,last)=post(&app,"records/query",json!({"query":{"recordType":"Album"},"continuationMarker":first["continuationMarker"],"resultsLimit":200})).await;
    assert_eq!(status, 200);
    assert_eq!(
        first["records"].as_array().unwrap().len() + last["records"].as_array().unwrap().len(),
        10
    );
    assert!(last.get("continuationMarker").is_none());
    app.stop().await;
}
