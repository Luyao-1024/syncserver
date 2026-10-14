mod common;
use common::Running;
use serde_json::{Value, json};
use syncserver::Server;
fn create(name: &str) -> Value {
    json!({"operationType":"create","record":{"recordName":name,"recordType":"Note","fields":{"value":{"type":"INT64","value":0}}}})
}
async fn modify(
    app: &Running,
    operations: Vec<Value>,
    atomic: bool,
    key: Option<&str>,
) -> (u16, Value) {
    common::fixture_modify(app, json!({"atomic":atomic,"operations":operations}), key).await
}
fn counts(path: &std::path::Path) -> (i64, i64, i64, i64, i64) {
    let db = rusqlite::Connection::open(path).unwrap();
    db.query_row("SELECT (SELECT count(*) FROM records),(SELECT count(*) FROM record_versions),(SELECT count(*) FROM changes),(SELECT last_committed_seq FROM namespace_counters WHERE namespace_id=1),(SELECT count(*) FROM operation_receipts)",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).unwrap()
}
#[tokio::test]
async fn history_and_changes_are_atomic_complete_and_replay_never_appends_again() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    let app = Running::start(cfg.clone()).await;
    let (_, first) = modify(&app, vec![create("history")], true, Some("history-key")).await;
    let tag = first["records"][0]["recordChangeTag"].clone();
    assert_eq!(
        modify(&app, vec![create("history")], true, Some("history-key"))
            .await
            .1,
        first
    );
    assert_eq!(modify(&app,vec![json!({"operationType":"update","record":{"recordName":"history","recordChangeTag":tag,"fields":{"value":{"type":"INT64","value":9}}}}),json!({"operationType":"update","record":{"recordName":"missing","recordChangeTag":"tag"}})],true,Some("rolled-back")).await.0,404);
    let (_,second)=modify(&app,vec![json!({"operationType":"update","record":{"recordName":"history","recordChangeTag":tag,"fields":{"value":{"type":"INT64","value":1}}}})],true,None).await;
    assert_eq!(modify(&app,vec![json!({"operationType":"delete","record":{"recordName":"history","recordChangeTag":second["records"][0]["recordChangeTag"]}})],true,None).await.0,200);
    app.stop().await;
    let path = root.path().join("data/metadata.sqlite");
    assert_eq!(counts(&path), (1, 3, 3, 3, 1));
    let db = rusqlite::Connection::open(&path).unwrap();
    let history:Vec<(i64,bool,String)>=db.prepare("SELECT seq,deleted,fields_json FROM record_versions WHERE namespace_id=1 AND zone_name='PrimarySync' AND record_name='history' ORDER BY seq").unwrap().query_map([],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?))).unwrap().collect::<std::result::Result<_,_>>().unwrap();
    assert_eq!(
        history.iter().map(|row| row.0).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(
        serde_json::from_str::<Value>(&history[0].2).unwrap()["value"]["value"],
        0
    );
    assert_eq!(
        serde_json::from_str::<Value>(&history[1].2).unwrap()["value"]["value"],
        1
    );
    assert!(history[2].1);
    assert_eq!(history[2].2, "{}");
    drop(db);
    let restarted = Running::start(cfg).await;
    assert_eq!(
        modify(
            &restarted,
            vec![create("history")],
            true,
            Some("history-key")
        )
        .await
        .1,
        first
    );
    restarted.stop().await;
    assert_eq!(counts(&path), (1, 3, 3, 3, 1));
}
#[tokio::test]
async fn sql_failure_rolls_back_projection_history_counter_and_receipt() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    Server::open(cfg.clone())
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let path = root.path().join("data/metadata.sqlite");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TRIGGER inject_append_failure BEFORE INSERT ON changes WHEN NEW.record_name='fail' BEGIN SELECT RAISE(ABORT,'synthetic fault'); END;").unwrap();
    }
    let app = Running::start(cfg.clone()).await;
    let (status, body) = modify(
        &app,
        vec![create("first"), create("fail")],
        true,
        Some("fault-key"),
    )
    .await;
    assert_eq!(status, 500);
    assert_eq!(body["serverErrorCode"], "INTERNAL_ERROR");
    assert!(!body.to_string().contains("synthetic fault"));
    app.stop().await;
    assert_eq!(counts(&path), (0, 0, 0, 0, 0));
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("DROP TRIGGER inject_append_failure")
            .unwrap();
    }
    let app = Running::start(cfg).await;
    assert_eq!(
        modify(
            &app,
            vec![create("first"), create("fail")],
            true,
            Some("fault-key")
        )
        .await
        .0,
        200
    );
    app.stop().await;
    assert_eq!(counts(&path), (2, 2, 2, 2, 1));
}
#[tokio::test]
async fn non_atomic_pending_batch_resumes_after_persistent_mid_batch_failure() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    Server::open(cfg.clone())
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let path = root.path().join("data/metadata.sqlite");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch("CREATE TRIGGER inject_mid_batch BEFORE INSERT ON changes WHEN NEW.record_name='second' BEGIN SELECT RAISE(ABORT,'synthetic fault'); END;").unwrap();
    }
    let app = Running::start(cfg.clone()).await;
    assert_eq!(
        modify(
            &app,
            vec![create("first"), create("second")],
            false,
            Some("resume-key")
        )
        .await
        .0,
        500
    );
    app.stop().await;
    assert_eq!(counts(&path), (1, 1, 1, 1, 2));
    let original_tag: String = {
        let db = rusqlite::Connection::open(&path).unwrap();
        let tag = db
            .query_row(
                "SELECT revision FROM records WHERE record_name='first'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        db.execute_batch("DROP TRIGGER inject_mid_batch").unwrap();
        tag
    };
    let app = Running::start(cfg).await;
    let (status, result) = modify(
        &app,
        vec![create("first"), create("second")],
        false,
        Some("resume-key"),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(result["records"][0]["recordChangeTag"], original_tag);
    app.stop().await;
    assert_eq!(counts(&path), (2, 2, 2, 2, 3));
}
#[tokio::test]
async fn change_and_version_queries_use_scope_indexes_with_real_history() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    let app = Running::start(cfg).await;
    let operations: Vec<_> = (0..400).map(|n| create(&format!("note-{n:04}"))).collect();
    let (status, result) = modify(&app, operations, true, None).await;
    assert_eq!(status, 200);
    for n in 0..2 {
        let operations:Vec<_>=result["records"].as_array().unwrap().iter().map(|record|json!({"operationType":"update","record":{"recordName":record["recordName"],"recordChangeTag":record["recordChangeTag"],"fields":{"value":{"type":"INT64","value":n+1}}}})).collect();
        let (status, next) = modify(&app, operations, true, None).await;
        if n == 0 {
            assert_eq!(status, 200);
        } else {
            assert_eq!(status, 409);
            assert_eq!(next["serverErrorCode"], "CONFLICT");
        }
    }
    app.stop().await;
    let path = root.path().join("data/metadata.sqlite");
    assert_eq!(counts(&path), (400, 800, 800, 800, 0));
    let db = rusqlite::Connection::open(&path).unwrap();
    for (query, index) in [
        (
            "EXPLAIN QUERY PLAN SELECT seq,zone_name,record_name FROM changes WHERE namespace_id=1 AND seq>100 ORDER BY seq LIMIT 200",
            "sqlite_autoindex_changes_1",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT seq,record_name FROM changes WHERE namespace_id=1 AND zone_name='PrimarySync' AND seq>100 ORDER BY seq LIMIT 200",
            "changes_zone",
        ),
        (
            "EXPLAIN QUERY PLAN SELECT fields_json FROM record_versions WHERE namespace_id=1 AND zone_name='PrimarySync' AND record_name='note-0001' AND seq<=600 ORDER BY seq DESC LIMIT 1",
            "record_versions_snapshot",
        ),
    ] {
        let details: Vec<String> = db
            .prepare(query)
            .unwrap()
            .query_map([], |row| row.get(3))
            .unwrap()
            .collect::<std::result::Result<_, _>>()
            .unwrap();
        assert!(
            details.iter().any(|detail| detail.contains(index)),
            "{details:?}"
        );
        assert!(
            details.iter().all(|detail| !detail.starts_with("SCAN ")),
            "{details:?}"
        );
    }
}
