use serde_json::Value;
use std::{
    fs,
    net::TcpListener,
    os::unix::fs::PermissionsExt,
    process::{Child, Command, Stdio},
    time::Duration,
};

const KEY: &str = "process-test-api-key-012345678901234567890";

struct Process {
    child: Child,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
fn launch(root: &std::path::Path, address: &str) -> Process {
    let child = Command::new(env!("CARGO_BIN_EXE_syncserver"))
        .env_clear()
        .env("SYNC_BIND", address)
        .env("SYNC_DATA_DIR", root.join("data"))
        .env("SYNC_PUBLIC_BASE_URL", format!("http://{address}"))
        .env("SYNC_API_KEY_FILE", root.join("api.key"))
        .env("SYNC_SIGNING_KEY_FILE", root.join("signing.key"))
        .env_remove("SYNC_CONTAINER")
        .env_remove("SYNC_ZONES")
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    Process { child }
}
async fn wait_ready(base: &str, process: &mut Process) -> Value {
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            assert!(
                process.child.try_wait().unwrap().is_none(),
                "server terminated before ready"
            );
            match client
                .get(format!("{base}/v1/capabilities"))
                .bearer_auth(KEY)
                .send()
                .await
            {
                Ok(response) if response.status() == 200 => return response.json().await.unwrap(),
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn sigkill_recovers_interrupted_start_and_sigterm_drains_and_releases_lock() {
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("api.key"), KEY).unwrap();
    fs::write(
        root.path().join("signing.key"),
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    )
    .unwrap();
    for name in ["api.key", "signing.key"] {
        fs::set_permissions(root.path().join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap().to_string();
    drop(socket);
    let base = format!("http://{address}");
    let mut first = launch(root.path(), &address);
    let original = wait_ready(&base, &mut first).await;
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let mut second = launch(root.path(), &address);
    let restarted = wait_ready(&base, &mut second).await;
    assert_eq!(
        original["databaseGeneration"],
        restarted["databaseGeneration"]
    );
    assert_eq!(restarted["recovery"]["interruptedStarts"], 1);
    assert_eq!(restarted["metrics"]["assetBytesHashed"], 0);
    // SAFETY: sends SIGTERM only to the test-owned child process.
    assert_eq!(
        unsafe { libc::kill(second.child.id() as i32, libc::SIGTERM) },
        0
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(status) = second.child.try_wait().unwrap() {
                assert!(status.success());
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    let mut third = launch(root.path(), &address);
    let clean = wait_ready(&base, &mut third).await;
    assert_eq!(clean["databaseGeneration"], original["databaseGeneration"]);
    assert_eq!(clean["recovery"]["interruptedStarts"], 0);
}

#[tokio::test]
async fn sigkill_during_non_atomic_writes_resumes_receipts_without_duplicate_changes() {
    use serde_json::json;
    let root = tempfile::tempdir().unwrap();
    fs::write(root.path().join("api.key"), KEY).unwrap();
    fs::write(
        root.path().join("signing.key"),
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    )
    .unwrap();
    for name in ["api.key", "signing.key"] {
        fs::set_permissions(root.path().join(name), fs::Permissions::from_mode(0o600)).unwrap();
    }
    let socket = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = socket.local_addr().unwrap().to_string();
    drop(socket);
    let base = format!("http://{address}");
    let mut first = launch(root.path(), &address);
    let original = wait_ready(&base, &mut first).await;
    let input = json!({"atomic":false,"operations":(0..400).map(|n|json!({"operationType":"create","record":{"recordName":format!("photo-metadata-{n:04}"),"recordType":"Note","fields":{"title":{"type":"STRING","value":"synthetic"}}}})).collect::<Vec<_>>()});
    let endpoint =
        format!("{base}/database/1/com.apple.photos.cloud/production/private/records/modify");
    let sending_input = input.clone();
    let sending_endpoint = endpoint.clone();
    let writing = tokio::spawn(async move {
        reqwest::Client::new()
            .post(sending_endpoint)
            .bearer_auth(KEY)
            .header("Idempotency-Key", "process-resume")
            .json(&sending_input)
            .send()
            .await
    });
    let db_path = root.path().join("data/metadata.sqlite");
    let observing_path = db_path.clone();
    let first_tag=tokio::task::spawn_blocking(move || {
        let db=rusqlite::Connection::open_with_flags(observing_path,rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
        let deadline=std::time::Instant::now()+Duration::from_secs(5);
        loop {
            let tag=db.query_row("SELECT revision FROM records WHERE namespace_id=1 AND zone_name='PrimarySync' AND record_name='photo-metadata-0000'",[],|row|row.get::<_,String>(0));
            if let Ok(tag)=tag {return tag;}
            assert!(std::time::Instant::now()<deadline,"first committed item not observed");
            std::thread::sleep(Duration::from_millis(1));
        }
    }).await.unwrap();
    first.child.kill().unwrap();
    first.child.wait().unwrap();
    let _ = writing.await.unwrap();
    let mut second = launch(root.path(), &address);
    let restarted = wait_ready(&base, &mut second).await;
    assert_eq!(
        original["databaseGeneration"],
        restarted["databaseGeneration"]
    );
    let response = reqwest::Client::new()
        .post(endpoint)
        .bearer_auth(KEY)
        .header("Idempotency-Key", "process-resume")
        .json(&input)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let result: Value = response.json().await.unwrap();
    assert_eq!(result["records"].as_array().unwrap().len(), 400);
    assert_eq!(result["records"][0]["recordChangeTag"], first_tag);
    let db =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let state:(i64,i64,i64,i64)=db.query_row("SELECT (SELECT count(*) FROM records),(SELECT count(*) FROM record_versions),(SELECT count(*) FROM changes),(SELECT last_committed_seq FROM namespace_counters WHERE namespace_id=1)",[],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
    assert_eq!(state, (400, 400, 400, 400));
}
