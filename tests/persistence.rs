mod common;
use common::{API_KEY, Running, config, config_with};
use serde_json::Value;
use std::{fs, os::unix::fs::PermissionsExt};
use syncserver::{Error, Server};

#[tokio::test]
async fn directory_is_exclusive_and_generation_survives_restart() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    let first = Server::open(config.clone()).await.unwrap();
    let metadata = first.metadata().clone();
    assert_eq!(metadata.recovery.interrupted_starts, 0);
    assert!(matches!(
        Server::open(config.clone()).await.err().unwrap(),
        Error::DataDirLocked
    ));
    let store = first.store();
    store.ping().await.unwrap();
    first.shutdown().await.unwrap();
    assert!(!store.accepting());
    assert!(matches!(store.ping().await, Err(Error::Unavailable)));
    let second = Server::open(config).await.unwrap();
    assert_eq!(
        metadata.database_generation,
        second.metadata().database_generation
    );
    assert_ne!(metadata.recovery.run_id, second.metadata().recovery.run_id);
    assert_eq!(second.metadata().recovery.interrupted_starts, 0);
    second.shutdown().await.unwrap();
    let db = rusqlite::Connection::open(root.path().join("data/metadata.sqlite")).unwrap();
    let mode: String = db
        .pragma_query_value(None, "journal_mode", |row| row.get(0))
        .unwrap();
    assert_eq!(mode, "wal");
    let stopped: i64 = db.query_row("SELECT count(*) FROM service_starts WHERE state='Stopped' AND stopped_at_ms IS NOT NULL", [], |row| row.get(0)).unwrap();
    assert_eq!(stopped, 2);
    for name in ["data", "data/objects", "data/staging"] {
        assert_eq!(
            fs::metadata(root.path().join(name))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    assert_eq!(
        fs::metadata(root.path().join("data/metadata.sqlite"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn scope_is_pinned_zone_order_is_irrelevant_and_keys_cannot_rotate_accidentally() {
    let root = tempfile::tempdir().unwrap();
    let first = Server::open(config_with(
        root.path(),
        &[("SYNC_ZONES", "Zeta,PrimarySync")],
    ))
    .await
    .unwrap();
    let generation = first.metadata().database_generation;
    first.shutdown().await.unwrap();
    for overrides in [
        &[("SYNC_CONTAINER", "other-container")][..],
        &[("SYNC_ZONES", "PrimarySync")][..],
        &[("SYNC_ZONES", "PrimarySync,Zeta,Other")][..],
    ] {
        assert!(matches!(
            Server::open(config_with(root.path(), overrides))
                .await
                .err()
                .unwrap(),
            Error::ScopeMismatch
        ));
    }
    let second = Server::open(config_with(
        root.path(),
        &[("SYNC_ZONES", "PrimarySync,Zeta")],
    ))
    .await
    .unwrap();
    assert_eq!(second.metadata().database_generation, generation);
    second.shutdown().await.unwrap();
    fs::write(
        root.path().join("signing.key"),
        "AgICAgICAgICAgICAgICAgICAgICAgICAgICAgICAgI=\n",
    )
    .unwrap();
    assert!(matches!(
        Server::open(config_with(
            root.path(),
            &[("SYNC_ZONES", "Zeta,PrimarySync")]
        ))
        .await
        .err()
        .unwrap(),
        Error::SigningKeyMismatch
    ));
}

#[tokio::test]
async fn independent_directories_and_credentials_are_isolated() {
    let first_root = tempfile::tempdir().unwrap();
    let second_root = tempfile::tempdir().unwrap();
    let first_config = config(first_root.path());
    let second_config = config(second_root.path());
    let other_key = "different-key-012345678901234567890123456789";
    fs::write(second_root.path().join("api.key"), other_key).unwrap();
    let first = Running::start(first_config).await;
    let second = Running::start(second_config).await;
    let first_value: Value = first
        .client
        .get(format!("{}/v1/capabilities", first.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_value: Value = second
        .client
        .get(format!("{}/v1/capabilities", second.base))
        .bearer_auth(other_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(
        first_value["databaseGeneration"],
        second_value["databaseGeneration"]
    );
    let response = first
        .client
        .get(format!("{}/v1/capabilities", second.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 401);
    first.stop().await;
    second.stop().await;
}

#[tokio::test]
async fn unknown_schema_and_corrupt_metadata_fail_closed() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    Server::open(config.clone())
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    let path = root.path().join("data/metadata.sqlite");
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 99).unwrap();
    }
    assert!(matches!(
        Server::open(config.clone()).await.err().unwrap(),
        Error::SchemaUnsupported
    ));
    {
        let db = rusqlite::Connection::open(&path).unwrap();
        db.pragma_update(None, "user_version", 2).unwrap();
        db.execute("UPDATE server_meta SET database_generation='corrupt'", [])
            .unwrap();
    }
    assert!(matches!(
        Server::open(config).await.err().unwrap(),
        Error::RecoveryFailed
    ));
}

#[tokio::test]
async fn startup_does_not_scan_or_hash_managed_objects() {
    let root = tempfile::tempdir().unwrap();
    let config = config(root.path());
    Server::open(config.clone())
        .await
        .unwrap()
        .shutdown()
        .await
        .unwrap();
    // An unreadable synthetic object makes accidental body access observable.
    let object = root.path().join("data/objects/unreadable-test-object");
    fs::write(
        &object,
        b"synthetic content that must never be read during metadata startup",
    )
    .unwrap();
    fs::set_permissions(&object, fs::Permissions::from_mode(0o000)).unwrap();
    let server = Server::open(config).await.unwrap();
    assert_eq!(server.metadata().recovery.asset_bytes_read, 0);
    assert_eq!(server.metadata().recovery.asset_bytes_hashed, 0);
    server.shutdown().await.unwrap();
}
