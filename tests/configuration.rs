mod common;
use common::Running;
use std::{
    fs,
    os::unix::fs::{PermissionsExt, symlink},
};
use syncserver::{Config, Error, LogFormat, LogLevel, Server};

fn pairs() -> Vec<(&'static str, String)> {
    vec![
        ("SYNC_DATA_DIR", "/unused/test".into()),
        ("SYNC_API_KEY_FILE", "/unused/api".into()),
        ("SYNC_SIGNING_KEY_FILE", "/unused/signing".into()),
    ]
}

#[test]
fn configuration_rejects_unsafe_urls_ranges_names_and_missing_keys() {
    assert!(Config::from_pairs(Vec::<(String, String)>::new()).is_err());
    for (key, invalid) in [
        ("SYNC_BIND", "example.org:8790"),
        ("SYNC_BIND", "127.0.0.1:65536"),
        ("SYNC_PUBLIC_BASE_URL", "http://example.org"),
        ("SYNC_PUBLIC_BASE_URL", "http://localhost:8790"),
        ("SYNC_PUBLIC_BASE_URL", "https://example.org/prefix"),
        ("SYNC_PUBLIC_BASE_URL", "https://user:secret@example.org"),
        ("SYNC_PUBLIC_BASE_URL", "https://example.org?token=secret"),
        ("SYNC_PUBLIC_BASE_URL", "https://example.org#fragment"),
        ("SYNC_PUBLIC_BASE_URL", "ftp://example.org"),
        ("SYNC_PUBLIC_BASE_URL", "https://example.org:0"),
        ("SYNC_CONTAINER", "../escape"),
        ("SYNC_CONTAINER", ""),
        ("SYNC_ZONES", "A,A"),
        ("SYNC_ZONES", "A,"),
        ("SYNC_ZONES", "../bad"),
        ("SYNC_ZONES", ""),
        ("SYNC_ZONES", " A"),
        ("SYNC_USER_STORAGE_QUOTA_BYTES", "0"),
        ("SYNC_USER_STORAGE_QUOTA_BYTES", "-1"),
        ("SYNC_USER_STORAGE_QUOTA_BYTES", "20GB"),
        ("SYNC_USER_STORAGE_QUOTA_BYTES", "9223372036854775808"),
        ("SYNC_MAX_ASSET_BYTES", "0"),
        ("SYNC_MAX_ASSET_BYTES", "9223372036854775808"),
        ("SYNC_THUMBNAIL_MAX_BYTES", "2097153"),
        ("SYNC_UPLOAD_CONCURRENCY", "0"),
        ("SYNC_DOWNLOAD_CONCURRENCY", "-1"),
        ("SYNC_UPLOAD_IDLE_TIMEOUT_SECS", "3601"),
        ("SYNC_UPLOAD_TOTAL_TIMEOUT_SECS", "0"),
        ("SYNC_UPLOAD_TOTAL_TIMEOUT_SECS", "18446744073709551615"),
        ("SYNC_UPLOAD_IDLE_TIMEOUT_SECS", "18446744073709551615"),
        ("SYNC_DB_QUEUE_CAPACITY", "0"),
        ("SYNC_JSON_BODY_LIMIT", "1.5"),
        ("SYNC_JSON_BODY_LIMIT", "18446744073709551616"),
        ("SYNC_DB_QUEUE_CAPACITY", "+1"),
    ] {
        let mut input = pairs();
        input.push((key, invalid.into()));
        let error = Config::from_pairs(input).unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{key}");
        assert!(!error.to_string().contains("secret"));
    }
    let mut input = pairs();
    input.push(("SYNC_BIND", "0.0.0.0:8790".into()));
    assert!(Config::from_pairs(input.clone()).is_err());
    input.push(("SYNC_PUBLIC_BASE_URL", "https://sync.example.org".into()));
    assert!(Config::from_pairs(input).is_ok());
    let mut input = pairs();
    input.push(("SYNC_PUBLIC_BASE_URL", "http://[::1]:8790".into()));
    assert!(Config::from_pairs(input).is_ok());
    let mut input = pairs();
    input.push(("SYNC_DATA_DIR", "/duplicate".into()));
    assert!(Config::from_pairs(input).is_err());
}

#[tokio::test]
async fn private_credentials_are_required_and_never_leaked_in_errors() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config(root.path());
    let api = root.path().join("api.key");
    fs::set_permissions(&api, fs::Permissions::from_mode(0o644)).unwrap();
    let error = Server::open(config.clone()).await.err().unwrap();
    assert!(matches!(error, Error::Config(_)));
    assert!(!error.to_string().contains(common::API_KEY));
    assert!(!root.path().join("data").exists());
    fs::set_permissions(&api, fs::Permissions::from_mode(0o600)).unwrap();
    for invalid in [
        "short",
        "one-line\ntwo-lines",
        " spaces-must-not-be-accepted-0123456789",
        "non-ASCII-密钥-01234567890123456789012",
    ] {
        fs::write(&api, invalid).unwrap();
        assert!(Server::open(config.clone()).await.is_err());
    }
    fs::write(&api, common::API_KEY).unwrap();
    let signing = root.path().join("signing.key");
    for invalid in [
        "not-base64",
        "AQ==",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE",
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=\n\n",
    ] {
        fs::write(&signing, invalid).unwrap();
        assert!(Server::open(config.clone()).await.is_err());
    }
    fs::remove_file(&signing).unwrap();
    assert!(Server::open(config.clone()).await.is_err());
    fs::write(
        root.path().join("outside"),
        "AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE=",
    )
    .unwrap();
    symlink(root.path().join("outside"), &signing).unwrap();
    assert!(Server::open(config).await.is_err());
}

#[tokio::test]
async fn symlink_and_shared_data_directories_are_rejected_without_touching_targets() {
    let root = tempfile::tempdir().unwrap();
    let config = common::config(root.path());
    let outside = root.path().join("outside");
    fs::create_dir(&outside).unwrap();
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o700)).unwrap();
    symlink(&outside, root.path().join("data")).unwrap();
    assert!(Server::open(config.clone()).await.is_err());
    assert!(fs::read_dir(&outside).unwrap().next().is_none());
    fs::remove_file(root.path().join("data")).unwrap();
    fs::create_dir(root.path().join("data")).unwrap();
    fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o755)).unwrap();
    assert!(Server::open(config.clone()).await.is_err());
    fs::set_permissions(root.path().join("data"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(outside.join("sentinel"), b"do not modify").unwrap();
    symlink(
        outside.join("sentinel"),
        root.path().join("data/metadata.sqlite"),
    )
    .unwrap();
    assert!(Server::open(config).await.is_err());
    assert_eq!(
        fs::read(outside.join("sentinel")).unwrap(),
        b"do not modify"
    );
}

#[test]
fn upload_control_defaults_and_heartbeat_bounds_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let cfg = common::config(root.path());
    assert_eq!(cfg.limits().lease_ttl_secs, 90);
    assert_eq!(cfg.limits().lease_heartbeat_secs, 30);
    assert_eq!(cfg.limits().control_queue_capacity, 16);
    for pairs in [
        [
            ("SYNC_LEASE_TTL_SECS", "30"),
            ("SYNC_LEASE_HEARTBEAT_SECS", "30"),
        ],
        [
            ("SYNC_LEASE_TTL_SECS", "86401"),
            ("SYNC_LEASE_HEARTBEAT_SECS", "30"),
        ],
        [
            ("SYNC_LEASE_TTL_SECS", "0"),
            ("SYNC_LEASE_HEARTBEAT_SECS", "1"),
        ],
    ] {
        let mut config = vec![
            ("SYNC_DATA_DIR", "/unused/data"),
            ("SYNC_API_KEY_FILE", "/unused/api"),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing"),
        ];
        config.extend(pairs);
        assert!(syncserver::Config::from_pairs(config).is_err());
    }
}

#[test]
fn logging_configuration_defaults_bounds_and_detail_switch() {
    let default = Config::from_pairs(pairs()).unwrap();
    assert_eq!(default.logging().level, LogLevel::Info);
    assert_eq!(default.logging().persist_level, LogLevel::Info);
    assert_eq!(default.logging().format, LogFormat::Text);
    assert!(!default.logging().payload_detail);
    assert_eq!(default.logging().retention_raw_secs, 86_400);
    assert_eq!(default.logging().archive_max_bytes, 1_073_741_824);
    assert_eq!(default.logging().queue_capacity, 4096);
    assert!(default.logging().level.includes(LogLevel::Warn));
    assert!(!default.logging().level.includes(LogLevel::Debug));
    assert!(!LogLevel::Off.includes(LogLevel::Error));

    // Development profile: debug everywhere plus payload detail, persistence inherits.
    let mut input = pairs();
    input.extend([
        ("SYNC_LOG_LEVEL", "debug".to_owned()),
        ("SYNC_LOG_PAYLOAD", "on".to_owned()),
    ]);
    let development = Config::from_pairs(input).unwrap();
    assert_eq!(development.logging().level, LogLevel::Debug);
    assert_eq!(development.logging().persist_level, LogLevel::Debug);
    assert!(development.logging().payload_detail);

    // Persistence may stay narrower than the console and use a machine-readable format.
    let mut input = pairs();
    input.extend([
        ("SYNC_LOG_LEVEL", "debug".to_owned()),
        ("SYNC_LOG_PERSIST_LEVEL", "warn".to_owned()),
        ("SYNC_LOG_FORMAT", "json".to_owned()),
    ]);
    let split = Config::from_pairs(input).unwrap();
    assert_eq!(split.logging().level, LogLevel::Debug);
    assert_eq!(split.logging().persist_level, LogLevel::Warn);
    assert_eq!(split.logging().format, LogFormat::Json);

    let mut input = pairs();
    input.push(("SYNC_LOG_PERSIST_LEVEL", "off".to_owned()));
    assert_eq!(
        Config::from_pairs(input).unwrap().logging().persist_level,
        LogLevel::Off
    );

    for (key, invalid) in [
        ("SYNC_LOG_LEVEL", "verbose"),
        ("SYNC_LOG_PERSIST_LEVEL", ""),
        ("SYNC_LOG_FORMAT", "yaml"),
        ("SYNC_LOG_PAYLOAD", "yes"),
        ("SYNC_LOG_RETENTION_RAW_SECS", "0"),
        ("SYNC_LOG_RETENTION_RAW_SECS", "2592001"),
        ("SYNC_LOG_RETENTION_RAW_SECS", "1d"),
        ("SYNC_LOG_ARCHIVE_MAX_BYTES", "4095"),
        ("SYNC_LOG_ARCHIVE_MAX_BYTES", "1099511627777"),
        ("SYNC_LOG_QUEUE_CAPACITY", "15"),
        ("SYNC_LOG_QUEUE_CAPACITY", "1048577"),
    ] {
        let mut input = pairs();
        input.push((key, invalid.into()));
        let error = Config::from_pairs(input).unwrap_err();
        assert!(matches!(error, Error::Config(_)), "{key}");
    }
}

#[test]
fn user_storage_quota_defaults_to_20_decimal_gb_and_accepts_sqlite_boundaries() {
    assert_eq!(
        Config::from_pairs(pairs())
            .unwrap()
            .limits()
            .user_storage_quota_bytes,
        20_000_000_000
    );
    for bytes in ["1", "9223372036854775807"] {
        let mut input = pairs();
        input.push(("SYNC_USER_STORAGE_QUOTA_BYTES", bytes.into()));
        assert_eq!(
            Config::from_pairs(input)
                .unwrap()
                .limits()
                .user_storage_quota_bytes,
            bytes.parse::<u64>().unwrap()
        );
    }
}

#[tokio::test]
async fn the_console_is_served_from_the_binary_and_carries_no_account_data() {
    let app = Running::new().await;

    // The shell is reachable without a session, because it holds nothing but code.
    let response = app
        .client
        .get(format!("{}/console/", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    assert_eq!(response.headers()["cache-control"], "no-store");
    let policy = response.headers()["content-security-policy"]
        .to_str()
        .unwrap();
    assert!(policy.contains("default-src 'self'"), "{policy}");
    // Images are rendered from object URLs, so the policy must permit them explicitly.
    assert!(policy.contains("img-src 'self' blob:"), "{policy}");
    let html = response.text().await.unwrap();
    assert!(html.contains("<div id=\"root\">"), "{html}");
    assert!(
        !html.contains("Bearer"),
        "the shell must not carry a session"
    );

    // A client-side route resolves to the same shell, so a reload works.
    for path in ["/console", "/console/admin/users", "/console/albums"] {
        let response = app
            .client
            .get(format!("{base}{path}", base = app.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "{path}");
    }

    // A traversal attempt is not an asset and never escapes the embedded tree.
    let response = app
        .client
        .get(format!("{}/console/../../etc/passwd", app.base))
        .send()
        .await
        .unwrap();
    assert_ne!(response.status(), 200, "a traversal must not resolve");

    // The console never widens the API: its routes still demand a session.
    let anonymous = app
        .client
        .get(format!("{}/v1/console/photos", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status(), 401);
}
