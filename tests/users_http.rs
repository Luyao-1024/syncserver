//! Real CLI + HTTP isolation; all passwords/data are synthetic and temporary.
mod common;
use common::{Lease, Running, server_process::run_cli_inherited};
use serde_json::{Value, json};
use std::path::Path;
use syncserver::users::Users;
use zeroize::Zeroizing;
const PASSWORD: &str = "synthetic-password-for-tests";
const CK: &str = "/database/1/com.apple.photos.cloud/production/private";
fn cli(root: &Path, args: &[&str], password: Option<&str>) -> std::process::Output {
    let _config = common::config(root);
    let data = root.join("data");
    let api_key = root.join("api.key");
    let signing_key = root.join("signing.key");
    run_cli_inherited(
        std::path::Path::new(env!("CARGO_BIN_EXE_syncserver")),
        args,
        &[
            ("SYNC_DATA_DIR", data.to_str().expect("utf-8 path")),
            ("SYNC_API_KEY_FILE", api_key.to_str().expect("utf-8 path")),
            (
                "SYNC_SIGNING_KEY_FILE",
                signing_key.to_str().expect("utf-8 path"),
            ),
        ],
        password,
    )
    .unwrap()
}
fn create(root: &Path, name: &str, adopt: bool) {
    let mut args = vec!["users", "create", name, "--password-stdin"];
    if adopt {
        args.push("--adopt-legacy")
    }
    let output = cli(root, &args, Some(PASSWORD));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8_lossy(&output.stdout).contains(PASSWORD));
    assert!(!String::from_utf8_lossy(&output.stderr).contains(PASSWORD));
}
async fn login(app: &Running, name: &str) -> Value {
    let response = app
        .client
        .post(format!("{}/v1/auth/login", app.base))
        .json(&json!({"username":name,"password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    response.json().await.unwrap()
}
async fn capability(app: &Running, token: &str) -> Value {
    let response = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    response.json().await.unwrap()
}
async fn create_phase(app: &Running, token: &str) -> Lease {
    let lease = common::ready(
        &app.client,
        &app.base,
        token,
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    for phase in ["modify", "create"] {
        assert_eq!(
            lease
                .control(
                    &app.client,
                    &app.base,
                    token,
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
async fn write(app: &Running, token: &str, lease: &Lease, label: &str) -> Value {
    let response=lease.headers(app.client.post(format!("{}{CK}/records/modify",app.base)).bearer_auth(token).header("Idempotency-Key","same-operation-id").json(&json!({"operations":[{"operationType":"create","record":{"recordName":"same-record-id","recordType":"Album","fields":{"label":{"type":"STRING","value":label}}}}]}))).send().await.unwrap();
    let status = response.status();
    let body: Value = response.json().await.unwrap();
    assert_eq!(status, 200, "{body}");
    body
}
async fn lookup(app: &Running, token: &str) -> Value {
    app.client
        .post(format!("{}{CK}/records/lookup", app.base))
        .bearer_auth(token)
        .json(&json!({"records":[{"recordName":"same-record-id"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn local(app: &Running, url: &str) -> String {
    let url = url::Url::parse(url).unwrap();
    format!("{}{}?{}", app.base, url.path(), url.query().unwrap())
}

#[tokio::test]
async fn cli_login_and_same_ids_have_independent_records_tokens_leases_and_receipts() {
    let root = tempfile::tempdir().unwrap();
    create(root.path(), "Alice", false);
    create(root.path(), "bob", false);
    let duplicate = cli(
        root.path(),
        &["users", "create", "alice", "--password-stdin"],
        Some(PASSWORD),
    );
    assert!(!duplicate.status.success());
    let app = Running::start(common::config(root.path())).await;
    let a = login(&app, "ALICE").await;
    let b = login(&app, "bob").await;
    let at = a["accessToken"].as_str().unwrap();
    let bt = b["accessToken"].as_str().unwrap();
    let ac = capability(&app, at).await;
    let bc = capability(&app, bt).await;
    assert_ne!(ac["namespace"]["tenant"], bc["namespace"]["tenant"]);
    assert_ne!(ac["databaseGeneration"], bc["databaseGeneration"]);
    assert_eq!(ac["authentication"]["user"]["id"], a["user"]["id"]);
    assert_eq!(ac["authentication"]["resourceBearerRequired"], true);
    assert_eq!(
        app.client
            .get(format!("{}/v1/capabilities", app.base))
            .bearer_auth(common::API_KEY)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let al = create_phase(&app, at).await;
    let bl = create_phase(&app, bt).await;
    write(&app, at, &al, "alice private").await;
    write(&app, bt, &bl, "bob private").await;
    assert_eq!(
        lookup(&app, at).await["records"][0]["fields"]["label"]["value"],
        "alice private"
    );
    assert_eq!(
        lookup(&app, bt).await["records"][0]["fields"]["label"]["value"],
        "bob private"
    );
    assert_eq!(
        al.control(&app.client, &app.base, bt, "renew", json!({}))
            .await
            .0,
        409
    );
    let snapshot: Value = app
        .client
        .post(format!("{}/v1/snapshots", app.base))
        .bearer_auth(at)
        .json(&json!({"zoneID":{"zoneName":"PrimarySync"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let crossed = app
        .client
        .post(format!("{}{CK}/records/query", app.base))
        .bearer_auth(bt)
        .json(&json!({"query":{"recordType":"Album"},"snapshotToken":snapshot["snapshotToken"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(crossed.status(), 400);
    let receipt: Value = app
        .client
        .get(format!("{}/v1/operations/same-operation-id", app.base))
        .bearer_auth(bt)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(!receipt.to_string().contains("alice private"));
    let lists = cli(root.path(), &["users", "list"], None);
    assert!(lists.status.success());
    let names = String::from_utf8(lists.stdout).unwrap();
    assert!(names.contains("alice") && names.contains("bob"));
    assert!(!names.contains("argon2"));
    app.stop().await;
    let app = Running::start(common::config(root.path())).await;
    assert_eq!(
        lookup(&app, at).await["records"][0]["fields"]["label"]["value"],
        "alice private"
    );
    assert_eq!(
        lookup(&app, bt).await["records"][0]["fields"]["label"]["value"],
        "bob private"
    );
    let auth = root.path().join("data/.authentication/users.sqlite");
    let conn = rusqlite::Connection::open(auth).unwrap();
    let encoded: String = conn
        .query_row(
            "SELECT password_hash FROM users WHERE username='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(encoded.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"));
    let stored: Vec<u8> = conn
        .query_row("SELECT digest FROM sessions LIMIT 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(stored.len(), 32);
    assert_ne!(stored, at.as_bytes());
    app.stop().await;
}

#[tokio::test]
async fn account_assets_require_matching_session_and_are_physically_separate() {
    let root = tempfile::tempdir().unwrap();
    create(root.path(), "alice", false);
    create(root.path(), "bob", false);
    let app = Running::start(common::config(root.path())).await;
    let a = login(&app, "alice").await;
    let b = login(&app, "bob").await;
    let at = a["accessToken"].as_str().unwrap();
    let bt = b["accessToken"].as_str().unwrap();
    let al = create_phase(&app, at).await;
    let bl = create_phase(&app, bt).await;
    let bytes = b"same authorized original content".to_vec();
    let mut descriptors = vec![];
    for (token, lease) in [(at, &al), (bt, &bl)] {
        let reserved:Value=lease.headers(app.client.post(format!("{}{CK}/assets/upload",app.base)).bearer_auth(token).json(&json!({"tokens":[{"recordName":"same-master-id","recordType":"CPLMaster","fieldName":"resOriginalRes","_syncserver":{"role":"original","intent":"create","expectedSize":bytes.len()}}]}))).send().await.unwrap().json().await.unwrap();
        let url = reserved["tokens"][0]["url"].as_str().unwrap();
        assert_eq!(
            lease
                .headers(
                    app.client
                        .post(local(&app, url))
                        .header("Content-Type", "application/octet-stream")
                        .body(bytes.clone())
                )
                .send()
                .await
                .unwrap()
                .status(),
            401
        );
        let response = lease
            .headers(
                app.client
                    .post(local(&app, url))
                    .bearer_auth(token)
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes.clone()),
            )
            .send()
            .await
            .unwrap();
        let status = response.status();
        let result: Value = response.json().await.unwrap();
        assert_eq!(status, 200, "{result}");
        descriptors.push(result["singleFile"].clone());
    }
    let aid = descriptors[0]["assetId"].as_str().unwrap();
    let bid = descriptors[1]["assetId"].as_str().unwrap();
    assert_ne!(aid, bid);
    let url = local(&app, descriptors[0]["downloadURL"].as_str().unwrap());
    assert_eq!(app.client.get(&url).send().await.unwrap().status(), 401);
    assert_eq!(
        app.client
            .get(&url)
            .bearer_auth(bt)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert_eq!(
        app.client
            .get(&url)
            .bearer_auth(at)
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap()
            .as_ref(),
        bytes
    );
    assert_eq!(
        app.client
            .get(format!("{}/v1/assets/{aid}?role=original", app.base))
            .bearer_auth(bt)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    assert_eq!(
        app.client
            .get(format!(
                "{}/v1/uploads/{}",
                app.base,
                al.body["leaseId"].as_str().unwrap()
            ))
            .bearer_auth(bt)
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let left = root
        .path()
        .join("data/users")
        .join(a["user"]["id"].as_str().unwrap())
        .join("objects");
    let right = root
        .path()
        .join("data/users")
        .join(b["user"]["id"].as_str().unwrap())
        .join("objects");
    assert!(left.is_dir() && right.is_dir());
    assert_ne!(left.canonicalize().unwrap(), right.canonicalize().unwrap());
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::MetadataExt;
    let name = format!("{:x}", Sha256::digest(&bytes));
    let left_file = left.join(&name);
    let right_file = right.join(&name);
    assert_eq!(std::fs::read(&left_file).unwrap(), bytes);
    assert_eq!(std::fs::read(&right_file).unwrap(), bytes);
    assert_ne!(
        std::fs::metadata(left_file).unwrap().ino(),
        std::fs::metadata(right_file).unwrap().ino()
    );
    app.stop().await;
}

#[tokio::test]
async fn revocation_failed_login_and_expiry_are_not_authorization() {
    let root = tempfile::tempdir().unwrap();
    create(root.path(), "alice", false);
    let app = Running::start(common::config(root.path())).await;
    for (user, password) in [("unknown", PASSWORD), ("alice", "wrong password")] {
        let response = app
            .client
            .post(format!("{}/v1/auth/login", app.base))
            .json(&json!({"username":user,"password":password}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["serverErrorCode"], "AUTHENTICATION_FAILED");
    }
    let session = login(&app, "alice").await;
    let token = session["accessToken"].as_str().unwrap();
    assert_eq!(
        app.client
            .post(format!("{}/v1/auth/logout", app.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert_eq!(
        app.client
            .get(format!("{}/v1/capabilities", app.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let session = login(&app, "alice").await;
    let token = session["accessToken"].as_str().unwrap();
    let output = cli(
        root.path(),
        &["users", "password", "alice", "--password-stdin"],
        Some(PASSWORD),
    );
    assert!(output.status.success());
    assert_eq!(
        app.client
            .get(format!("{}/v1/capabilities", app.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    let session = login(&app, "alice").await;
    let token = session["accessToken"].as_str().unwrap();
    assert!(
        cli(root.path(), &["users", "disable", "alice"], None)
            .status
            .success()
    );
    assert_eq!(
        app.client
            .get(format!("{}/v1/capabilities", app.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    assert!(
        cli(root.path(), &["users", "enable", "alice"], None)
            .status
            .success()
    );
    assert_eq!(
        app.client
            .get(format!("{}/v1/capabilities", app.base))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    app.stop().await;
}

#[tokio::test]
async fn only_an_explicit_owner_can_adopt_the_legacy_store() {
    let root = tempfile::tempdir().unwrap();
    let app = Running::start(common::config(root.path())).await;
    let lease = create_phase(&app, common::API_KEY).await;
    write(&app, common::API_KEY, &lease, "existing private data").await;
    create(root.path(), "owner", true);
    create(root.path(), "fresh", false);
    let owner = login(&app, "owner").await;
    let fresh = login(&app, "fresh").await;
    assert_eq!(
        lookup(&app, owner["accessToken"].as_str().unwrap()).await["records"][0]["fields"]["label"]
            ["value"],
        "existing private data"
    );
    assert_eq!(
        lookup(&app, fresh["accessToken"].as_str().unwrap()).await["records"][0]["serverErrorCode"],
        "UNKNOWN_ITEM"
    );
    assert!(
        !cli(
            root.path(),
            &[
                "users",
                "create",
                "second-owner",
                "--password-stdin",
                "--adopt-legacy"
            ],
            Some(PASSWORD)
        )
        .status
        .success()
    );
    let users = Users::open(&root.path().join("data")).unwrap();
    assert!(
        users
            .login("fresh", Zeroizing::new(PASSWORD.into()))
            .unwrap()
            .is_some()
    );
    app.stop().await;
}

#[tokio::test]
async fn a_users_storage_open_failure_does_not_block_another_user() {
    let root = tempfile::tempdir().unwrap();
    create(root.path(), "broken", false);
    create(root.path(), "healthy", false);
    let app = Running::start(common::config(root.path())).await;
    let broken = login(&app, "broken").await;
    let healthy = login(&app, "healthy").await;
    let users = root.path().join("data/users");
    std::fs::create_dir(&users).unwrap();
    use std::os::unix::fs::{PermissionsExt, symlink};
    std::fs::set_permissions(&users, std::fs::Permissions::from_mode(0o700)).unwrap();
    let unrelated = tempfile::tempdir().unwrap();
    symlink(
        unrelated.path(),
        users.join(broken["user"]["id"].as_str().unwrap()),
    )
    .unwrap();
    let failed = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(broken["accessToken"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(failed.status(), 500);
    capability(&app, healthy["accessToken"].as_str().unwrap()).await;
    assert_eq!(
        app.client
            .get(format!("{}/readyz", app.base))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    app.stop().await;
}

#[tokio::test]
async fn login_limits_are_bounded_and_do_not_block_another_accounts_metadata() {
    let root = tempfile::tempdir().unwrap();
    create(root.path(), "alice", false);
    create(root.path(), "bob", false);
    let app = Running::start(common::config(root.path())).await;
    let malformed = app
        .client
        .post(format!("{}/v1/auth/login", app.base))
        .json(&json!({"username":"alice","password":PASSWORD,"userId":"not-an-identity-selector"}))
        .send()
        .await
        .unwrap();
    assert_eq!(malformed.status(), 400);
    assert_eq!(malformed.headers()["cache-control"], "no-store");
    for _ in 0..8 {
        let failed = app
            .client
            .post(format!("{}/v1/auth/login", app.base))
            .json(&json!({"username":"alice","password":"wrong synthetic password"}))
            .send()
            .await
            .unwrap();
        assert_eq!(failed.status(), 401);
    }
    let limited = app
        .client
        .post(format!("{}/v1/auth/login", app.base))
        .json(&json!({"username":"alice","password":PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(limited.status(), 429);
    assert_eq!(limited.headers()["retry-after"], "60");
    let bob = login(&app, "bob").await;
    capability(&app, bob["accessToken"].as_str().unwrap()).await;
    app.stop().await;
}

#[tokio::test]
async fn an_adopted_owners_stopped_executor_does_not_stop_other_users() {
    let root = tempfile::tempdir().unwrap();
    create(root.path(), "owner", true);
    create(root.path(), "healthy", false);
    let server = syncserver::Server::open(common::config(root.path()))
        .await
        .unwrap();
    let owner_store = server.store();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let (send, receive) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(server.serve(listener, async {
        let _ = receive.await;
    }));
    owner_store.shutdown().await.unwrap();
    let client = reqwest::Client::new();
    for (username, status) in [("owner", 503), ("healthy", 200)] {
        let session: Value = client
            .post(format!("{base}/v1/auth/login"))
            .json(&json!({"username":username,"password":PASSWORD}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let response = client
            .get(format!("{base}/v1/capabilities"))
            .bearer_auth(session["accessToken"].as_str().unwrap())
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), status);
    }
    assert_eq!(
        client
            .get(format!("{base}/readyz"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    send.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), task)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}
