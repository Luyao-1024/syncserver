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
