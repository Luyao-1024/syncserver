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
