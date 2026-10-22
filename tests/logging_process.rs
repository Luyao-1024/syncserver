mod common;

use common::server_process::{ServerProcess, run_cli};
use serde_json::{Value, json};
use std::{fs, net::TcpListener, os::unix::fs::PermissionsExt, path::Path, time::Duration};

const KEY: &str = "logging-test-api-key-012345678901234567890";
const PASSWORD: &str = "never-log-this-password";

fn write_keys(root: &Path) {
    fs::write(root.join("api.key"), KEY).unwrap();
    fs::set_permissions(root.join("api.key"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        root.join("signing.key"),
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\n",
    )
    .unwrap();
    fs::set_permissions(root.join("signing.key"), fs::Permissions::from_mode(0o600)).unwrap();
}

struct Instance {
    /// Owned through the shared harness: the process group and the
    /// kernel-level parent-death backstop make sure the server cannot outlive
    /// the test, even when the test binary is killed outright.
    child: ServerProcess,
    base: String,
}

impl Drop for Instance {
    fn drop(&mut self) {
        self.child.stop();
    }
}

/// Starts the real binary with an explicit log profile, exactly like an operator would.
async fn start(root: &Path, level: &str, payload: &str) -> Instance {
    write_keys(root);
    let address = {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().to_string()
    };
    let data = root.join("data");
    let api_key = root.join("api.key");
    let signing_key = root.join("signing.key");
    let base = format!("http://{address}");
    let mut child = ServerProcess::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_syncserver")),
        &[
            ("SYNC_BIND", address.as_str()),
            ("SYNC_DATA_DIR", data.to_str().expect("utf-8 path")),
            ("SYNC_PUBLIC_BASE_URL", base.as_str()),
            ("SYNC_API_KEY_FILE", api_key.to_str().expect("utf-8 path")),
            (
                "SYNC_SIGNING_KEY_FILE",
                signing_key.to_str().expect("utf-8 path"),
            ),
            ("SYNC_LOG_LEVEL", level),
            ("SYNC_LOG_PAYLOAD", payload),
            ("SYNC_LOG_RETENTION_RAW_SECS", "1"),
            ("SYNC_LOG_ARCHIVE_MAX_BYTES", "1048576"),
        ],
    )
    .unwrap();
    let client = reqwest::Client::new();
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "server terminated before ready"
            );
            if client
                .get(format!("{base}/v1/capabilities"))
                .bearer_auth(KEY)
                .send()
                .await
                .is_ok_and(|response| response.status() == 200)
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    Instance { child, base }
}

/// Returns the operational metrics document captured after the traffic burst.
async fn drive_traffic(instance: &Instance) -> Value {
    let client = reqwest::Client::new();
    // A probe, an authenticated read, a rejected login and a real metadata read.
    let _ = client
        .get(format!("{}/healthz", instance.base))
        .send()
        .await;
    let _ = client
        .post(format!("{}/v1/auth/login", instance.base))
        .json(&json!({"username": "photoviewer", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    let _ = client
        .post(format!(
            "{}/database/1/com.apple.photos.cloud/production/private/records/query",
            instance.base
        ))
        .bearer_auth(KEY)
        .json(&json!({"query":{"recordType":"Album"},"resultsLimit":5}))
        .send()
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1200)).await;
    client
        .get(format!("{}/v1/metrics", instance.base))
        .bearer_auth(KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

/// Stops the service through the normal signal path so shutdown also flushes the logs.
fn stop(mut instance: Instance) {
    instance.child.signal(libc::SIGTERM).unwrap();
    let status = instance.child.wait().unwrap();
    assert!(status.success(), "graceful shutdown must exit successfully");
}

fn read_logs(root: &Path, arguments: &[&str]) -> Vec<Value> {
    let signing_key = root.parent().unwrap().join("signing.key");
    let mut argv = vec!["logs", "--json", "--limit", "2000"];
    argv.extend_from_slice(arguments);
    let output = run_cli(
        std::path::Path::new(env!("CARGO_BIN_EXE_syncserver")),
        &argv,
        &[
            ("SYNC_DATA_DIR", root.to_str().expect("utf-8 path")),
            (
                "SYNC_SIGNING_KEY_FILE",
                signing_key.to_str().expect("utf-8 path"),
            ),
        ],
        None,
    )
    .unwrap();
    assert!(
        output.status.success(),
        "logs command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

fn find<'a>(rows: &'a [Value], message: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| row["message"].as_str() == Some(message))
}

#[tokio::test]
async fn development_profile_persists_debug_detail_and_production_does_not() {
    let development = tempfile::tempdir().unwrap();
    let instance = start(development.path(), "debug", "on").await;
    let metrics = drive_traffic(&instance).await;
    assert_eq!(metrics["metrics"]["logging"]["persistLevel"], "debug");
    assert_eq!(metrics["metrics"]["logging"]["activated"], true);
    assert!(metrics["metrics"]["logging"]["persisted"].as_u64().unwrap() > 0);
    assert_eq!(metrics["metrics"]["logging"]["droppedQueueFull"], 0);
    assert_eq!(metrics["metrics"]["logging"]["writeFailures"], 0);
    assert_eq!(metrics["metrics"]["logging"]["archiveMaxBytes"], 1_048_576);
    assert!(metrics["metrics"]["logging"]["rawRows"].as_u64().unwrap() > 0);
    let wire = metrics.to_string();
    assert!(
        !wire.contains(KEY) && !wire.contains(PASSWORD),
        "metrics leaked credentials"
    );
    stop(instance);
    assert!(development.path().join("data/logs.sqlite").exists());

    let rows = read_logs(
        &development.path().join("data"),
        &["--level", "debug,info,warn,error", "--archive"],
    );
    assert!(!rows.is_empty(), "development run must persist records");
    let ready = find(&rows, "service ready").expect("startup record");
    assert_eq!(ready["fields"]["log_persist_level"], "debug");
    assert!(find(&rows, "startup recovery completed").is_some());
    assert!(find(&rows, "service stopped after draining queued work").is_some());

    let rejected = find(&rows, "authentication rejected").expect("rejected login record");
    assert_eq!(rejected["level"], "WARN");
    // Detail mode shows the account name but never the password or credentials.
    assert_eq!(rejected["fields"]["username"], json!("photoviewer"));
    let wire = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        !wire.contains(PASSWORD),
        "password reached the log database"
    );
    assert!(!wire.contains(KEY), "API key reached the log database");

    // Development keeps the debug level: probes and metadata reads are recorded.
    assert!(
        rows.iter()
            .any(|row| row["level"] == "DEBUG" && row["message"] == "request completed"),
        "probe traffic stayed at debug"
    );
    assert!(find(&rows, "metadata read completed").is_some());
    let completed = rows
        .iter()
        .filter(|row| row["message"] == "request completed")
        .collect::<Vec<_>>();
    assert!(
        completed
            .iter()
            .any(|row| row["fields"]["route"] == "/healthz" && row["fields"]["status"] == 200),
        "health probe was not recorded"
    );
    assert!(
        completed
            .iter()
            .any(|row| row["fields"]["error_code"] == "AUTHENTICATION_FAILED"),
        "the rejected login lost its error code"
    );
    assert!(
        completed
            .iter()
            .all(|row| row["fields"]["duration_ms"].is_number()),
        "request timing is part of the operational record"
    );
    let correlated = completed
        .iter()
        .filter(|row| row["request_id"].is_string())
        .count();
    assert_eq!(
        correlated,
        completed.len(),
        "every request record needs a request id"
    );

    // Filters work against the live database, including archive-inclusive reads.
    let warns = read_logs(&development.path().join("data"), &["--level", "warn"]);
    assert!(
        warns
            .iter()
            .all(|row| row["level"] == "WARN" || row["level"] == "ERROR"),
        "level filter leaked lower severities"
    );
}

#[tokio::test]
async fn production_profile_keeps_info_level_and_suppresses_library_data() {
    let production = tempfile::tempdir().unwrap();
    let instance = start(production.path(), "info", "off").await;
    let metrics = drive_traffic(&instance).await;
    assert_eq!(metrics["metrics"]["logging"]["persistLevel"], "info");
    assert_eq!(metrics["metrics"]["logging"]["queueCapacity"], 4096);
    stop(instance);
    let database = production.path().join("data");
    assert!(database.join("logs.sqlite").exists());

    let rows = read_logs(&database, &[]);
    assert!(!rows.is_empty());
    assert!(
        rows.iter().all(|row| row["level"] != "DEBUG"),
        "info profile must not persist debug records"
    );
    // The health probe is debug-only, so it is absent from the persisted stream.
    assert!(
        !rows.iter().any(
            |row| row["fields"]["route"] == "/healthz" && row["message"] == "request completed"
        ),
        "probe traffic leaked into the production stream"
    );
    let rejected = find(&rows, "authentication rejected").expect("rejected login record");
    assert_eq!(
        rejected["fields"]["username"],
        json!("[detail-off]"),
        "library data leaked without detail logging"
    );
    let wire = rows
        .iter()
        .map(Value::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(!wire.contains(PASSWORD));
    assert!(!wire.contains(KEY));
    assert!(
        !wire.contains("photoviewer"),
        "username leaked in production"
    );
}

/// Runs a real server in account mode, so a request can be authenticated as an account
/// rather than with the legacy key.
///
/// This is the only test that can prove the middleware wiring is right. Everything else
/// exercises storage or the query, and all of them would still pass if `request
/// completed` were emitted without ever learning who made the request.
async fn start_with_account(root: &Path) -> (Instance, String) {
    // The same key material the other profiles use. Only the signing key matters here:
    // an account deployment omits the API key entirely.
    write_keys(root);
    fs::remove_file(root.join("api.key")).unwrap();
    // The account has to exist before the server starts: the CLI needs the data
    // directory lock, and the running service holds it.
    let created = run_cli(
        std::path::Path::new(env!("CARGO_BIN_EXE_syncserver")),
        &["users", "create", "photoviewer", "--password-stdin"],
        &[
            (
                "SYNC_DATA_DIR",
                root.join("data").to_str().expect("utf-8 path"),
            ),
            (
                "SYNC_SIGNING_KEY_FILE",
                root.join("signing.key").to_str().expect("utf-8 path"),
            ),
        ],
        Some(PASSWORD),
    )
    .unwrap();
    assert!(
        created.status.success(),
        "account creation failed: {}",
        String::from_utf8_lossy(&created.stderr)
    );

    let address = {
        let socket = TcpListener::bind("127.0.0.1:0").unwrap();
        socket.local_addr().unwrap().to_string()
    };
    let base = format!("http://{address}");
    let mut child = ServerProcess::spawn(
        std::path::Path::new(env!("CARGO_BIN_EXE_syncserver")),
        &[
            ("SYNC_BIND", address.as_str()),
            (
                "SYNC_DATA_DIR",
                root.join("data").to_str().expect("utf-8 path"),
            ),
            ("SYNC_PUBLIC_BASE_URL", base.as_str()),
            (
                "SYNC_SIGNING_KEY_FILE",
                root.join("signing.key").to_str().expect("utf-8 path"),
            ),
            ("SYNC_LOG_LEVEL", "info"),
            ("SYNC_LOG_PERSIST_LEVEL", "info"),
        ],
    )
    .unwrap();
    let client = reqwest::Client::new();
    let token = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            assert!(
                child.try_wait().unwrap().is_none(),
                "server terminated before ready"
            );
            if let Ok(response) = client
                .post(format!("{base}/v1/auth/login"))
                .json(&json!({"username": "photoviewer", "password": PASSWORD}))
                .send()
                .await
                && response.status() == 200
            {
                let body: Value = response.json().await.unwrap();
                return body["accessToken"].as_str().unwrap().to_owned();
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap();
    (Instance { child, base }, token)
}

#[tokio::test]
async fn an_authenticated_request_is_attributed_to_its_account() {
    let root = tempfile::tempdir().unwrap();
    let (instance, token) = start_with_account(root.path()).await;
    let client = reqwest::Client::new();

    // A real session, then a real metadata read made with it.
    let response = client
        .post(format!(
            "{}/database/1/com.apple.photos.cloud/production/private/records/query",
            instance.base
        ))
        .bearer_auth(&token)
        .json(&json!({"query":{"recordType":"Album"},"resultsLimit":5}))
        .send()
        .await
        .unwrap();
    let request_id = response
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .unwrap()
        .to_owned();
    assert_eq!(response.status().as_u16(), 200);

    // An unauthenticated request to the same endpoint, as a control.
    let anonymous = client
        .post(format!(
            "{}/database/1/com.apple.photos.cloud/production/private/records/query",
            instance.base
        ))
        .json(&json!({"query":{"recordType":"Album"},"resultsLimit":5}))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 401);

    tokio::time::sleep(Duration::from_millis(1200)).await;
    stop(instance);

    let rows = read_logs(&root.path().join("data"), &[]);
    let issued = find(&rows, "session issued").expect("a session was issued");
    let account = issued["user_id"]
        .as_str()
        .expect("the session record names its account")
        .to_owned();

    let completed = rows
        .iter()
        .find(|row| {
            row["message"] == "request completed" && row["request_id"] == request_id.as_str()
        })
        .expect("the authenticated request is logged");
    assert_eq!(
        completed["user_id"].as_str(),
        Some(account.as_str()),
        "the request record names the same account the session did"
    );

    // The 401 went through the same middleware and the same code path, so it must be
    // recorded — attributed to nobody, which is what a rejected request is.
    let rejected = rows
        .iter()
        .find(|row| row["message"] == "request completed" && row["fields"]["status"] == 401)
        .expect("a rejected request is still logged");
    assert!(
        rejected.get("user_id").is_none_or(Value::is_null),
        "a request that never authenticated is attributed to no one: {rejected}"
    );

    // And the operator-facing filter agrees with what the records say.
    let mine = read_logs(&root.path().join("data"), &["--user", &account]);
    assert!(
        mine.iter().all(|row| row["user_id"] == account.as_str()),
        "filtering by account returns only that account"
    );
    assert!(!mine.is_empty());
    assert!(
        mine.iter()
            .any(|row| row["request_id"] == request_id.as_str()),
        "the authenticated request is among them"
    );
    let others = read_logs(
        &root.path().join("data"),
        &["--user", "00000000-0000-4000-8000-000000000000"],
    );
    assert!(others.is_empty(), "an account nobody has matches nothing");
}
