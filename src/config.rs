use crate::{Error, Result, domain::Namespace};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    net::SocketAddr,
    path::PathBuf,
};
use url::{Host, Url};

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Limits {
    pub max_asset_bytes: u64,
    pub user_storage_quota_bytes: u64,
    pub user_physical_storage_quota_bytes: u64,
    pub min_free_disk_bytes: u64,
    pub metadata_max_bytes: u64,
    pub thumbnail_max_bytes: u64,
    pub upload_concurrency: usize,
    pub download_concurrency: usize,
    pub upload_idle_timeout_secs: u64,
    pub upload_total_timeout_secs: u64,
    pub db_queue_capacity: usize,
    pub json_body_limit: usize,
    pub lease_ttl_secs: u64,
    pub lease_heartbeat_secs: u64,
    pub control_queue_capacity: usize,
}

#[derive(Clone, Debug)]
pub struct Config {
    pub(crate) bind: SocketAddr,
    pub(crate) trusted_proxy_ips: Vec<std::net::IpAddr>,
    pub(crate) data_dir: PathBuf,
    pub(crate) public_base_url: Url,
    pub(crate) api_key_file: PathBuf,
    pub(crate) signing_key_file: PathBuf,
    pub(crate) namespace: Namespace,
    pub(crate) zones: Vec<String>,
    pub(crate) limits: Limits,
    pub(crate) log: LogSettings,
}

const KEYS: &[&str] = &[
    "SYNC_BIND",
    "SYNC_TRUSTED_PROXY_IPS",
    "SYNC_DATA_DIR",
    "SYNC_PUBLIC_BASE_URL",
    "SYNC_API_KEY_FILE",
    "SYNC_SIGNING_KEY_FILE",
    "SYNC_CONTAINER",
    "SYNC_ZONES",
    "SYNC_MAX_ASSET_BYTES",
    "SYNC_USER_STORAGE_QUOTA_BYTES",
    "SYNC_USER_PHYSICAL_STORAGE_QUOTA_BYTES",
    "SYNC_MIN_FREE_DISK_BYTES",
    "SYNC_METADATA_MAX_BYTES",
    "SYNC_LOG_DATABASE_MAX_BYTES",
    "SYNC_THUMBNAIL_MAX_BYTES",
    "SYNC_UPLOAD_CONCURRENCY",
    "SYNC_DOWNLOAD_CONCURRENCY",
    "SYNC_UPLOAD_IDLE_TIMEOUT_SECS",
    "SYNC_UPLOAD_TOTAL_TIMEOUT_SECS",
    "SYNC_DB_QUEUE_CAPACITY",
    "SYNC_JSON_BODY_LIMIT",
    "SYNC_LEASE_TTL_SECS",
    "SYNC_LEASE_HEARTBEAT_SECS",
    "SYNC_CONTROL_QUEUE_CAPACITY",
    "SYNC_LOG_LEVEL",
    "SYNC_LOG_PERSIST_LEVEL",
    "SYNC_LOG_FORMAT",
    "SYNC_LOG_PAYLOAD",
    "SYNC_LOG_RETENTION_RAW_SECS",
    "SYNC_LOG_ARCHIVE_MAX_BYTES",
    "SYNC_LOG_QUEUE_CAPACITY",
];

impl Config {
    pub fn from_env() -> Result<Self> {
        let mut pairs = Vec::new();
        for key in KEYS {
            match std::env::var(key) {
                Ok(value) => pairs.push((*key, value)),
                Err(std::env::VarError::NotPresent) => {}
                Err(_) => return Err(Error::Config("configuration must be UTF-8")),
            }
        }
        Self::from_pairs(pairs)
    }

    /// Deterministic parser for embedding and tests; never mutates process environment.
    pub fn from_pairs<I, K, V>(pairs: I) -> Result<Self>
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut values = BTreeMap::new();
        for (key, value) in pairs {
            let key = key.into();
            if !KEYS.contains(&key.as_str()) || values.insert(key, value.into()).is_some() {
                return Err(Error::Config("unknown or duplicate configuration key"));
            }
        }
        let value =
            |key: &str, default: &str| values.get(key).cloned().unwrap_or_else(|| default.into());
        let required = |key: &str| -> Result<PathBuf> {
            values
                .get(key)
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .ok_or(Error::Config(
                    "data directory and signing key file path are required",
                ))
        };
        let bind: SocketAddr = value("SYNC_BIND", "127.0.0.1:8790")
            .parse()
            .map_err(|_| Error::Config("SYNC_BIND must be an IP socket address"))?;
        let public_base_url = Url::parse(&value("SYNC_PUBLIC_BASE_URL", "http://127.0.0.1:8790"))
            .map_err(|_| Error::Config("invalid public base URL"))?;
        let host_loopback = match public_base_url.host() {
            Some(Host::Ipv4(ip)) => ip.is_loopback(),
            Some(Host::Ipv6(ip)) => ip.is_loopback(),
            // Literal IP only for HTTP: avoids DNS/rebinding and ambiguous localhost aliases.
            _ => false,
        };
        if !matches!(public_base_url.scheme(), "http" | "https")
            || public_base_url.host().is_none()
            || !public_base_url.username().is_empty()
            || public_base_url.password().is_some()
            || public_base_url.query().is_some()
            || public_base_url.fragment().is_some()
            || public_base_url.path() != "/"
            || public_base_url.port() == Some(0)
            || (public_base_url.scheme() == "http" && (!host_loopback || !bind.ip().is_loopback()))
        {
            return Err(Error::Config(
                "public base URL requires HTTPS outside loopback and cannot contain credentials, path, query or fragment",
            ));
        }
        let container = value("SYNC_CONTAINER", "com.apple.photos.cloud");
        if !safe_name(&container) {
            return Err(Error::Config("invalid container name"));
        }
        let mut zones = BTreeSet::new();
        for zone in value("SYNC_ZONES", "PrimarySync").split(',') {
            if !safe_name(zone) || !zones.insert(zone.to_owned()) {
                return Err(Error::Config(
                    "zones must be unique comma-separated ASCII identifiers",
                ));
            }
        }
        let number = |key: &str, default: u64| -> Result<u64> {
            let text = value(key, &default.to_string());
            if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
                return Err(Error::Config("limits must be positive integers"));
            }
            text.parse::<u64>()
                .ok()
                .filter(|n| *n > 0)
                .ok_or(Error::Config("limits must be positive integers"))
        };
        let sized = |key: &str, default: u64| -> Result<usize> {
            let n = number(key, default)?;
            usize::try_from(n)
                .ok()
                .filter(|n| *n <= u32::MAX as usize)
                .ok_or(Error::Config(
                    "queue, concurrency and body limits exceed implementation bounds",
                ))
        };
        let limits = Limits {
            max_asset_bytes: number("SYNC_MAX_ASSET_BYTES", 536870912)?,
            user_storage_quota_bytes: number("SYNC_USER_STORAGE_QUOTA_BYTES", 20_000_000_000)?,
            user_physical_storage_quota_bytes: number(
                "SYNC_USER_PHYSICAL_STORAGE_QUOTA_BYTES",
                40_000_000_000,
            )?,
            min_free_disk_bytes: number("SYNC_MIN_FREE_DISK_BYTES", 1_073_741_824)?,
            metadata_max_bytes: number("SYNC_METADATA_MAX_BYTES", 1_073_741_824)?,
            thumbnail_max_bytes: number("SYNC_THUMBNAIL_MAX_BYTES", 2097152)?,
            upload_concurrency: sized("SYNC_UPLOAD_CONCURRENCY", 2)?,
            download_concurrency: sized("SYNC_DOWNLOAD_CONCURRENCY", 8)?,
            upload_idle_timeout_secs: number("SYNC_UPLOAD_IDLE_TIMEOUT_SECS", 30)?,
            upload_total_timeout_secs: number("SYNC_UPLOAD_TOTAL_TIMEOUT_SECS", 3600)?,
            db_queue_capacity: sized("SYNC_DB_QUEUE_CAPACITY", 128)?,
            json_body_limit: sized("SYNC_JSON_BODY_LIMIT", 1048576)?,
            lease_ttl_secs: number("SYNC_LEASE_TTL_SECS", 90)?,
            lease_heartbeat_secs: number("SYNC_LEASE_HEARTBEAT_SECS", 30)?,
            control_queue_capacity: sized("SYNC_CONTROL_QUEUE_CAPACITY", 16)?,
        };
        if limits.lease_heartbeat_secs >= limits.lease_ttl_secs || limits.lease_ttl_secs > 86400 {
            return Err(Error::Config(
                "lease heartbeat must be below TTL and TTL must not exceed 24 hours",
            ));
        }
        if limits.upload_idle_timeout_secs > u32::MAX as u64
            || limits.upload_total_timeout_secs > u32::MAX as u64
        {
            return Err(Error::Config(
                "upload timeouts exceed implementation bounds",
            ));
        }
        if limits.thumbnail_max_bytes > 2097152
            || limits.max_asset_bytes > i64::MAX as u64
            || limits.user_storage_quota_bytes > i64::MAX as u64
            || limits.upload_total_timeout_secs < limits.upload_idle_timeout_secs
        {
            return Err(Error::Config(
                "thumbnail limit exceeds 2 MiB, asset size or storage quota overflows SQLite, or total timeout is below idle timeout",
            ));
        }
        if limits.user_physical_storage_quota_bytes > i64::MAX as u64
            || limits.min_free_disk_bytes > i64::MAX as u64
            || !(1_048_576..=1_099_511_627_776).contains(&limits.metadata_max_bytes)
        {
            return Err(Error::Config(
                "physical quota, free-space reserve or metadata cap is out of bounds",
            ));
        }
        let database_max_bytes = number("SYNC_LOG_DATABASE_MAX_BYTES", 268_435_456)?;
        if !(1_048_576..=1_099_511_627_776).contains(&database_max_bytes) {
            return Err(Error::Config(
                "log database cap must be between 1 MiB and 1 TiB",
            ));
        }
        let level = LogLevel::parse(&value("SYNC_LOG_LEVEL", "info"))?;
        // Persistence may be narrower than the console; "off" disables the log database.
        let persist_level = match values.get("SYNC_LOG_PERSIST_LEVEL") {
            Some(raw) => LogLevel::parse(raw)?,
            None => level,
        };
        let payload_text = value("SYNC_LOG_PAYLOAD", "off").trim().to_ascii_lowercase();
        let payload_detail = match payload_text.as_str() {
            "off" => false,
            "on" => true,
            _ => return Err(Error::Config("SYNC_LOG_PAYLOAD must be off or on")),
        };
        let retention_raw_secs = number("SYNC_LOG_RETENTION_RAW_SECS", 86_400)?;
        let archive_max_bytes = number("SYNC_LOG_ARCHIVE_MAX_BYTES", 1_073_741_824)?;
        let queue_capacity = sized("SYNC_LOG_QUEUE_CAPACITY", 4096)?;
        if !(1..=2_592_000).contains(&retention_raw_secs) {
            return Err(Error::Config(
                "raw log retention must be between one second and 30 days",
            ));
        }
        if !(4096..=1_099_511_627_776).contains(&archive_max_bytes) {
            return Err(Error::Config(
                "compressed log archive must be between 4 KiB and 1 TiB",
            ));
        }
        if !(16..=1_048_576).contains(&queue_capacity) {
            return Err(Error::Config(
                "log queue capacity must be between 16 and 1048576 records",
            ));
        }
        let log = LogSettings {
            level,
            persist_level,
            format: LogFormat::parse(&value("SYNC_LOG_FORMAT", "text"))?,
            payload_detail,
            retention_raw_secs,
            archive_max_bytes,
            queue_capacity,
            database_max_bytes,
        };
        Ok(Self {
            bind,
            trusted_proxy_ips: value("SYNC_TRUSTED_PROXY_IPS", "")
                .split(',')
                .filter(|s| !s.is_empty())
                .map(|s| {
                    s.parse()
                        .map_err(|_| Error::Config("trusted proxy addresses must be literal IPs"))
                })
                .collect::<Result<Vec<_>>>()?,
            data_dir: required("SYNC_DATA_DIR")?,
            public_base_url,
            api_key_file: values
                .get("SYNC_API_KEY_FILE")
                .map(PathBuf::from)
                .unwrap_or_default(),
            signing_key_file: required("SYNC_SIGNING_KEY_FILE")?,
            namespace: Namespace::configured(container),
            zones: zones.into_iter().collect(),
            limits,
            log,
        })
    }

    pub fn limits(&self) -> &Limits {
        &self.limits
    }

    pub fn logging(&self) -> &LogSettings {
        &self.log
    }

    pub fn bind_address(&self) -> SocketAddr {
        self.bind
    }
    pub fn data_directory(&self) -> &std::path::Path {
        &self.data_dir
    }
}

/// Severity threshold shared by the console writer and the persisted log database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }
    fn rank(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Error => 1,
            Self::Warn => 2,
            Self::Info => 3,
            Self::Debug => 4,
            Self::Trace => 5,
        }
    }
    /// A threshold emits a record only when that record is at least as severe as it.
    pub fn includes(self, record: Self) -> bool {
        self != Self::Off && record != Self::Off && record.rank() <= self.rank()
    }
    pub fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "off" => Ok(Self::Off),
            "error" => Ok(Self::Error),
            "warn" => Ok(Self::Warn),
            "info" => Ok(Self::Info),
            "debug" => Ok(Self::Debug),
            "trace" => Ok(Self::Trace),
            _ => Err(Error::Config(
                "log level must be off, error, warn, info, debug or trace",
            )),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LogFormat {
    Text,
    Json,
}

impl LogFormat {
    fn parse(value: &str) -> Result<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "text" => Ok(Self::Text),
            "json" => Ok(Self::Json),
            _ => Err(Error::Config("SYNC_LOG_FORMAT must be text or json")),
        }
    }
}

/// Log pipeline settings; deliberately holds no credential or path material.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSettings {
    pub level: LogLevel,
    pub persist_level: LogLevel,
    pub format: LogFormat,
    pub payload_detail: bool,
    pub retention_raw_secs: u64,
    pub archive_max_bytes: u64,
    pub queue_capacity: usize,
    pub database_max_bytes: u64,
}

fn safe_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !matches!(value, "." | "..")
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'))
}
