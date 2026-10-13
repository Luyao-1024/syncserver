mod common;
use common::{API_KEY, Running};
use serde_json::{Value, json};

const BASE: &str = "/database/1/com.apple.photos.cloud/production/private";
async fn post(app: &Running, operation: &str, body: Value, key: Option<&str>) -> (u16, Value) {
    let mut request = app
        .client
        .post(format!("{}{BASE}/{operation}", app.base))
        .bearer_auth(API_KEY)
        .header("Content-Type", "text/plain")
        .body(body.to_string());
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = request.send().await.unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
fn create(name: &str) -> Value {
    json!({"operationType":"create", "record":{"recordName":name, "recordType":"Note", "fields":{
        "title":{"type":"STRING","value":"initial"}, "count":{"type":"INT64","value":1}
    }}})
}
fn update(name: &str, revision: &Value) -> Value {
    json!({"operationType":"update", "record":{"recordName":name, "recordChangeTag":revision,
        "fields":{"title":{"type":"STRING","value":"updated"},"count":null}}})
}
fn delete(name: &str, revision: &Value) -> Value {
    json!({"operationType":"delete", "record":{"recordName":name,"recordChangeTag":revision}})
}
async fn lookup(app: &Running, name: &str) -> Value {
    let (status, body) = post(
        app,
        "records/lookup",
        json!({"records":[{"recordName":name}]}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    body["records"][0].clone()
}

#[tokio::test]
async fn generic_crud_patches_fields_checks_revisions_and_retires_names() {
    let app = Running::new().await;
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[create("note")]}),
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let record = body["records"][0].clone();
    let revision = &record["recordChangeTag"];
    assert!(uuid::Uuid::parse_str(revision.as_str().unwrap()).is_ok());
    assert_eq!(record["zoneID"]["zoneName"], "PrimarySync");
    assert_eq!(lookup(&app, "note").await, record);
    let (status, _) = post(
        &app,
        "records/modify",
        json!({"operations":[create("note")]}),
        None,
    )
    .await;
    assert_eq!(status, 409);
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[update("note", &json!("stale"))]}),
        None,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["serverErrorCode"], "CONFLICT");
    assert_eq!(body["serverRecord"], record);
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[update("note", revision)]}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let updated = body["records"][0].clone();
    assert_ne!(updated["recordChangeTag"], *revision);
    assert_eq!(updated["fields"]["title"]["value"], "updated");
    assert!(updated["fields"].get("count").is_none());
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[delete("note", &updated["recordChangeTag"])]}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(body["records"][0]["deleted"], true);
    assert!(body["records"][0]["recordType"].is_null());
    assert_ne!(
        body["records"][0]["recordChangeTag"],
        updated["recordChangeTag"]
    );
    assert_eq!(
        lookup(&app, "note").await["serverErrorCode"],
        "UNKNOWN_ITEM"
    );
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[create("note")]}),
        None,
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["serverErrorCode"], "RECORD_NAME_RETIRED");
    app.stop().await;
}

#[tokio::test]
async fn atomic_failures_roll_back_every_operation_and_non_atomic_returns_ordered_items() {
    let app = Running::new().await;
    let (status, _) = post(
        &app,
        "records/modify",
        json!({"operations":[create("new"), update("missing", &json!("tag"))]}),
        Some("failed-batch"),
    )
    .await;
    assert_eq!(status, 404);
    assert_eq!(lookup(&app, "new").await["serverErrorCode"], "UNKNOWN_ITEM");
    let (status, body) = post(&app, "records/modify", json!({"atomic":false,"operations":[create("new"),update("missing", &json!("tag")),create("second")]}), Some("partial-batch")).await;
    assert_eq!(status, 200);
    assert_eq!(body["records"][0]["recordName"], "new");
    assert_eq!(body["records"][1]["serverErrorCode"], "UNKNOWN_ITEM");
    assert_eq!(body["records"][2]["recordName"], "second");
    let (status, replay) = post(&app, "records/modify", json!({"atomic":false,"operations":[create("new"),update("missing", &json!("tag")),create("second")]}), Some("partial-batch")).await;
    assert_eq!(status, 200);
    assert_eq!(replay, body);
    let (status, _) = post(
        &app,
        "records/modify",
        json!({"operations":[create("duplicate"),create("duplicate")]}),
        None,
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        lookup(&app, "duplicate").await["serverErrorCode"],
        "UNKNOWN_ITEM"
    );
    app.stop().await;
}

#[tokio::test]
async fn concurrent_update_and_delete_commit_exactly_one_and_expose_current_conflict() {
    let app = Running::new().await;
    let (_, body) = post(
        &app,
        "records/modify",
        json!({"operations":[create("race")]}),
        None,
    )
    .await;
    let tag = &body["records"][0]["recordChangeTag"];
    let (a, b) = tokio::join!(
        post(
            &app,
            "records/modify",
            json!({"operations":[update("race",tag)]}),
            None
        ),
        post(
            &app,
            "records/modify",
            json!({"operations":[delete("race",tag)]}),
            None
        )
    );
    assert!(
        (a.0 == 200 && b.0 == 409) || (a.0 == 409 && b.0 == 200),
        "{a:?} {b:?}"
    );
    let conflict = if a.0 == 409 { a.1 } else { b.1 };
    assert_eq!(conflict["serverErrorCode"], "CONFLICT");
    assert_ne!(conflict["serverRecord"]["recordChangeTag"], *tag);
    app.stop().await;
}

#[tokio::test]
async fn validation_scope_lookup_projection_and_capability_boundaries() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_ZONES", "PrimarySync,Other")],
    ))
    .await;
    for body in [
        json!({"operations":[]}),
        json!({"operations":[create("a")],"ignored":true}),
        json!({"operations":[create("a")],"zoneID":{"zoneName":"PrimarySync","ownerRecordName":"other"}}),
        json!({"operations":[update("a",&Value::Null)]}),
    ] {
        assert_eq!(post(&app, "records/modify", body, None).await.0, 400);
    }
    for (field, code) in [
        (json!({"type":"INT64","value":true}), "BAD_REQUEST"),
        (json!({"type":"TIMESTAMP","value":1.5}), "BAD_REQUEST"),
        (json!({"type":"BYTES","value":"not base64"}), "BAD_REQUEST"),
        (
            json!({"type":"LOCATION","value":{}}),
            "UNSUPPORTED_CAPABILITY",
        ),
        (
            json!({"type":"REFERENCE","value":{"recordName":"a","action":"NONE","zoneID":{"zoneName":"Other"}}}),
            "BAD_REQUEST",
        ),
    ] {
        let mut op = create("bad");
        op["record"]["fields"]["test"] = field;
        let (_, body) = post(&app, "records/modify", json!({"operations":[op]}), None).await;
        assert_eq!(body["serverErrorCode"], code);
    }
    let mut other = create("a");
    other["record"]["zoneID"] = json!({"zoneName":"Other"});
    assert_eq!(
        post(
            &app,
            "records/modify",
            json!({"operations":[create("a"),other.clone()]}),
            None
        )
        .await
        .0,
        400
    );
    assert_eq!(
        post(
            &app,
            "records/modify",
            json!({"atomic":false,"operations":[create("a"),other]}),
            None
        )
        .await
        .0,
        200
    );
    let (_, result)=post(&app,"records/lookup",json!({"records":[{"recordName":"a"},{"recordName":"a","zoneID":{"zoneName":"Other"}},{"recordName":"a","zoneID":{"zoneName":"Unknown"}}],"desiredKeys":["title"]}),None).await;
    assert_eq!(result["records"][0]["fields"].as_object().unwrap().len(), 1);
    assert_eq!(result["records"][1]["zoneID"]["zoneName"], "Other");
    assert_eq!(result["records"][2]["serverErrorCode"], "UNKNOWN_ITEM");
    let tag = lookup(&app, "a").await["recordChangeTag"].clone();
    let mut op = update("a", &tag);
    op["record"]["recordType"] = json!("CPLAsset");
    assert_eq!(
        post(&app, "records/modify", json!({"operations":[op]}), None)
            .await
            .1["serverErrorCode"],
        "RECORD_TYPE_IMMUTABLE"
    );
    let mut op = update("a", &tag);
    op["operationType"] = json!("forceUpdate");
    assert_eq!(
        post(&app, "records/modify", json!({"operations":[op]}), None)
            .await
            .1["serverErrorCode"],
        "UNSUPPORTED_CAPABILITY"
    );
    assert_eq!(
        post(
            &app,
            "records/lookup",
            json!({"records":[{"recordName":"a"}],"snapshotToken":"future"}),
            None
        )
        .await
        .0,
        400
    );
    let capabilities: Value = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(capabilities["stage"], "M2");
    assert_eq!(capabilities["capabilities"]["recordCrud"], true);
    assert_eq!(capabilities["capabilities"]["snapshots"], false);
    app.stop().await;
}

#[tokio::test]
async fn photo_pairs_cannot_be_split_or_publish_untrusted_descriptors() {
    let app = Running::new().await;
    let fixtures: Value =
        serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap();
    let request = &fixtures["createPhotoRequest"];
    for op in request["operations"].as_array().unwrap() {
        let (_, body) = post(&app, "records/modify", json!({"operations":[op]}), None).await;
        assert_eq!(body["serverErrorCode"], "PHOTO_PAIR_REQUIRED", "{body}");
    }
    let (status, body) = post(&app, "records/modify", request.clone(), None).await;
    assert_eq!(status, 501, "{body}");
    assert_eq!(body["serverErrorCode"], "UNSUPPORTED_CAPABILITY");
    assert_eq!(
        lookup(&app, "photo-001").await["serverErrorCode"],
        "UNKNOWN_ITEM"
    );
    let mut request = request.clone();
    request["atomic"] = json!(false);
    let (_, body) = post(&app, "records/modify", request, None).await;
    for record in body["records"].as_array().unwrap() {
        assert_eq!(record["serverErrorCode"], "PHOTO_PAIR_REQUIRED");
    }
    app.stop().await;
}

#[tokio::test]
async fn idempotency_is_canonical_concurrent_persistent_and_binds_complete_request() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config(root.path());
    let app = Running::start(config.clone()).await;
    let input = json!({"operations":[create("once")]});
    let (a, b) = tokio::join!(
        post(&app, "records/modify", input.clone(), Some("one-key")),
        post(&app, "records/modify", input.clone(), Some("one-key"))
    );
    assert_eq!(a.0, 200);
    assert_eq!(a, b);
    let tag = a.1["records"][0]["recordChangeTag"].clone();
    assert_eq!(
        post(
            &app,
            "records/modify",
            json!({"operations":[update("once",&tag)]}),
            None
        )
        .await
        .0,
        200
    );
    let (_,replay)=post(&app,"records/modify",json!({"atomic":true,"zoneID":{"zoneName":"PrimarySync","ownerRecordName":"_defaultOwner"},"operations":[create("once")]}),Some("one-key")).await;
    assert_eq!(replay, a.1);
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[create("changed")]}),
        Some("one-key"),
    )
    .await;
    assert_eq!(status, 409);
    assert_eq!(body["serverErrorCode"], "IDEMPOTENCY_CONFLICT");
    app.stop().await;
    let restarted = Running::start(config).await;
    assert_eq!(
        post(&restarted, "records/modify", input, Some("one-key")).await,
        (200, a.1)
    );
    restarted.stop().await;
}

#[tokio::test]
async fn duplicate_json_keys_invalid_idempotency_headers_and_batch_limits_are_rejected() {
    let app = Running::new().await;
    for body in [
        "{\"operations\":[],\"operations\":[]}",
        "{\"operations\":[{\"operationType\":\"create\",\"record\":{\"recordName\":\"dup\",\"recordType\":\"Note\",\"fields\":{\"a\":{\"type\":\"STRING\",\"value\":\"first\",\"value\":\"second\"}}}}]}",
    ] {
        let response = app
            .client
            .post(format!("{}{BASE}/records/modify", app.base))
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
        .post(format!("{}{BASE}/records/modify?force=true", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"operations":[create("query-param")]}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let body = json!({"operations":[create("headers")]});
    for key in ["", "bad key", &"a".repeat(129)] {
        assert_eq!(
            post(&app, "records/modify", body.clone(), Some(key))
                .await
                .0,
            400
        );
    }
    let response = app
        .client
        .post(format!("{}{BASE}/records/modify", app.base))
        .bearer_auth(API_KEY)
        .header("Idempotency-Key", "one")
        .header("Idempotency-Key", "two")
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
    let too_many =
        json!({"operations":(0..401).map(|n|create(&format!("note-{n}"))).collect::<Vec<_>>()});
    assert_eq!(post(&app, "records/modify", too_many, None).await.0, 413);
    assert_eq!(
        lookup(&app, "headers").await["serverErrorCode"],
        "UNKNOWN_ITEM"
    );
    app.stop().await;
}

#[tokio::test]
async fn record_size_limit_applies_to_accumulated_updates_before_any_new_version() {
    let app = Running::new().await;
    let mut initial = create("large");
    initial["record"]["fields"] = json!({"a":{"type":"STRING","value":"a".repeat(600_000)}});
    let (status, body) = post(
        &app,
        "records/modify",
        json!({"operations":[initial]}),
        None,
    )
    .await;
    assert_eq!(status, 200);
    let record = body["records"][0].clone();
    let operation = json!({"operationType":"update","record":{"recordName":"large","recordChangeTag":record["recordChangeTag"],"fields":{"b":{"type":"STRING","value":"b".repeat(600_000)}}}});
    assert_eq!(
        post(
            &app,
            "records/modify",
            json!({"operations":[operation]}),
            None
        )
        .await
        .0,
        413
    );
    assert_eq!(lookup(&app, "large").await, record);
    app.stop().await;
}

#[tokio::test]
async fn equivalent_reference_scope_is_normalized_for_idempotent_replays() {
    let app = Running::new().await;
    let mut op = create("reference");
    op["record"]["fields"]["parent"] =
        json!({"type":"REFERENCE","value":{"recordName":"parent","action":"NONE"}});
    let (status, original) = post(
        &app,
        "records/modify",
        json!({"operations":[op.clone()]}),
        Some("reference-key"),
    )
    .await;
    assert_eq!(status, 200);
    op["record"]["fields"]["parent"]["value"]["zoneID"] = json!({"zoneName":"PrimarySync"});
    assert_eq!(
        post(
            &app,
            "records/modify",
            json!({"operations":[op]}),
            Some("reference-key")
        )
        .await,
        (200, original)
    );
    app.stop().await;
}
