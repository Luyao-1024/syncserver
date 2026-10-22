#![allow(dead_code)]
pub mod server_process;

use std::{fs, os::unix::fs::PermissionsExt, path::Path};
use syncserver::{Config, Server};
use tempfile::TempDir;
use tokio::{net::TcpListener, sync::oneshot, task::JoinHandle};

pub const API_KEY: &str = "test-only-key-012345678901234567890123456789";

pub fn config(root: &Path) -> Config {
    config_with(root, &[])
}

#[allow(dead_code)]
pub fn config_with(root: &Path, overrides: &[(&str, &str)]) -> Config {
    let api = root.join("api.key");
    let signing = root.join("signing.key");
    if !api.exists() {
        fs::write(&api, format!("{API_KEY}\n")).unwrap();
        fs::set_permissions(&api, fs::Permissions::from_mode(0o600)).unwrap();
        // Synthetic credential, not a production example.
        fs::write(&signing, "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\n").unwrap();
        fs::set_permissions(&signing, fs::Permissions::from_mode(0o600)).unwrap();
    }
    let mut pairs: std::collections::BTreeMap<String, String> = [
        ("SYNC_BIND".into(), "127.0.0.1:0".into()),
        (
            "SYNC_PUBLIC_BASE_URL".into(),
            "http://127.0.0.1:8790".into(),
        ),
        (
            "SYNC_DATA_DIR".into(),
            root.join("data").display().to_string(),
        ),
        ("SYNC_API_KEY_FILE".into(), api.display().to_string()),
        (
            "SYNC_SIGNING_KEY_FILE".into(),
            signing.display().to_string(),
        ),
    ]
    .into();
    for (key, value) in overrides {
        pairs.insert((*key).into(), (*value).into());
    }
    Config::from_pairs(pairs).unwrap()
}

pub struct Running {
    pub base: String,
    pub client: reqwest::Client,
    pub fixture_cycles: tokio::sync::Mutex<()>,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<syncserver::Result<()>>,
    _root: Option<TempDir>,
}

impl Running {
    #[allow(dead_code)]
    pub async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let mut instance = Self::start(config(root.path())).await;
        instance._root = Some(root);
        instance
    }

    pub async fn start(config: Config) -> Self {
        let server = Server::open(config).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let (shutdown, receiver) = oneshot::channel();
        let task = tokio::spawn(server.serve(listener, async {
            let _ = receiver.await;
        }));
        Self {
            base,
            client: reqwest::Client::new(),
            fixture_cycles: tokio::sync::Mutex::new(()),
            shutdown: Some(shutdown),
            task,
            _root: None,
        }
    }

    pub async fn stop(mut self) {
        self.shutdown.take().unwrap().send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }
}

#[derive(Clone, Debug)]
pub struct Lease {
    pub body: serde_json::Value,
}
impl Lease {
    pub fn headers(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("X-Sync-Lease-ID", self.body["leaseId"].as_str().unwrap())
            .header(
                "X-Sync-Lease-Token",
                self.body["leaseToken"].as_str().unwrap(),
            )
            .header(
                "X-Sync-Lease-Fence",
                self.body["fencingToken"].as_str().unwrap(),
            )
    }
    #[allow(dead_code)]
    pub async fn control(
        &self,
        client: &reqwest::Client,
        base: &str,
        key: &str,
        op: &str,
        body: serde_json::Value,
    ) -> (u16, serde_json::Value) {
        let response = self
            .headers(
                client
                    .post(format!(
                        "{base}/v1/upload-leases/{}/{op}",
                        self.body["leaseId"].as_str().unwrap()
                    ))
                    .bearer_auth(key)
                    .json(&body),
            )
            .send()
            .await
            .unwrap();
        (response.status().as_u16(), response.json().await.unwrap())
    }
}
#[allow(dead_code)]
pub async fn acquire(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    device: &str,
    zones: Vec<serde_json::Value>,
) -> (u16, serde_json::Value) {
    let response = client
        .post(format!("{base}/v1/upload-leases"))
        .bearer_auth(key)
        .json(&serde_json::json!({"deviceId":device,"zoneIDs":zones}))
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}
#[allow(dead_code)]
pub async fn ready(
    client: &reqwest::Client,
    base: &str,
    key: &str,
    zones: Vec<serde_json::Value>,
) -> Lease {
    let (status, body) = acquire(client, base, key, "test-fixture", zones.clone()).await;
    assert_eq!(status, 200, "{body}");
    let lease = Lease { body };
    let mut tokens = Vec::new();
    for zone in zones {
        let snapshot: serde_json::Value = client
            .post(format!("{base}/v1/snapshots"))
            .bearer_auth(key)
            .json(&serde_json::json!({"zoneID":zone}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        tokens.push(serde_json::json!({"zoneID":zone,"syncToken":snapshot["syncToken"]}));
    }
    let (status, body) = lease
        .control(
            client,
            base,
            key,
            "check",
            serde_json::json!({"zoneTokens":tokens,"localReady":true}),
        )
        .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["mayUpload"], true, "{body}");
    lease
}
#[allow(dead_code)]
pub async fn fixture_modify(
    app: &Running,
    body: serde_json::Value,
    key: Option<&str>,
) -> (u16, serde_json::Value) {
    // Fixtures serialize only their setup cycles. Concurrency/lease tests use
    // explicit shared or competing leases and real simultaneous HTTP requests.
    let _cycle = app.fixture_cycles.lock().await;
    let default_zone = body
        .get("zoneID")
        .cloned()
        .unwrap_or_else(|| serde_json::json!({"zoneName":"PrimarySync"}));
    let names: std::collections::BTreeSet<String> = body["operations"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|op| {
            op["record"].get("zoneID").unwrap_or(&default_zone)["zoneName"]
                .as_str()
                .unwrap_or("PrimarySync")
                .to_owned()
        })
        .collect();
    let capabilities: serde_json::Value = app
        .client
        .get(format!("{}/v1/capabilities", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let available = capabilities["zones"].as_array().unwrap();
    let mut zones: Vec<_> = names
        .into_iter()
        .filter(|name| available.contains(&serde_json::json!(name)))
        .map(|name| serde_json::json!({"zoneName":name}))
        .collect();
    if zones.is_empty() {
        zones.push(serde_json::json!({"zoneName":available[0]}));
    }
    let lease = ready(&app.client, &app.base, API_KEY, zones).await;
    let kind = body["operations"][0]["operationType"]
        .as_str()
        .unwrap_or("delete");
    if kind == "update" || kind == "create" {
        assert_eq!(
            lease
                .control(
                    &app.client,
                    &app.base,
                    API_KEY,
                    "phase",
                    serde_json::json!({"phase":"modify"})
                )
                .await
                .0,
            200
        );
    }
    if kind == "create" {
        assert_eq!(
            lease
                .control(
                    &app.client,
                    &app.base,
                    API_KEY,
                    "phase",
                    serde_json::json!({"phase":"create"})
                )
                .await
                .0,
            200
        );
    }
    let mut request = lease.headers(
        app.client
            .post(format!(
                "{}/database/1/com.apple.photos.cloud/production/private/records/modify",
                app.base
            ))
            .bearer_auth(API_KEY)
            .json(&body),
    );
    if let Some(key) = key {
        request = request.header("Idempotency-Key", key);
    }
    let response = request.send().await.unwrap();
    let result = (response.status().as_u16(), response.json().await.unwrap());
    assert_eq!(
        lease
            .control(
                &app.client,
                &app.base,
                API_KEY,
                "release",
                serde_json::json!({})
            )
            .await
            .0,
        200
    );
    result
}
