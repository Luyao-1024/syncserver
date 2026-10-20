use crate::{Config, Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

const APPLICATION_ID: i64 = 0x53594e43;
pub(crate) const SCHEMA_VERSION: i64 = 7;

pub(crate) fn initialize(
    connection: &mut Connection,
    config: &Config,
    fingerprint: &[u8; 32],
) -> Result<Uuid> {
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    // Identification before modifying journal mode or foreign database contents.
    let version: i64 = connection.pragma_query_value(None, "user_version", |row| row.get(0))?;
    let application: i64 =
        connection.pragma_query_value(None, "application_id", |row| row.get(0))?;
    let table_count: i64 = connection.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |row| row.get(0),
    )?;
    let fresh = version == 0 && application == 0 && table_count == 0;
    if !fresh && (application != APPLICATION_ID || !(1..=SCHEMA_VERSION).contains(&version)) {
        return Err(Error::SchemaUnsupported);
    }
    connection.pragma_update(None, "foreign_keys", "ON")?;
    let journal: String = connection.pragma_query_value(None, "journal_mode", |row| row.get(0))?;
    if journal != "wal" {
        let actual: String =
            connection.query_row("PRAGMA journal_mode=WAL", [], |row| row.get(0))?;
        if actual != "wal" {
            return Err(Error::RecoveryFailed);
        }
    }
    connection.pragma_update(None, "synchronous", "FULL")?;
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    if fresh {
        tx.execute_batch(include_str!("schema.sql"))?;
        tx.execute(
            "INSERT INTO server_meta VALUES (1, ?1, ?2, ?3)",
            params![1, Uuid::new_v4().to_string(), fingerprint.as_slice()],
        )?;
        tx.execute(
            "INSERT INTO namespaces VALUES (1, ?1, ?2, ?3, ?4)",
            params![
                config.namespace.tenant,
                config.namespace.container,
                config.namespace.environment,
                config.namespace.database
            ],
        )?;
        tx.execute("INSERT INTO namespace_counters VALUES (1, 0)", [])?;
        for zone in &config.zones {
            tx.execute("INSERT INTO zones VALUES (1, ?1, '_defaultOwner')", [zone])?;
        }
        tx.pragma_update(None, "application_id", APPLICATION_ID)?;
        tx.pragma_update(None, "user_version", 1)?;
    }
    let scope: (String, String, String, String) = tx.query_row(
        "SELECT tenant, container, environment, database_name FROM namespaces WHERE namespace_id=1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    if scope
        != (
            config.namespace.tenant.clone(),
            config.namespace.container.clone(),
            config.namespace.environment.clone(),
            config.namespace.database.clone(),
        )
    {
        return Err(Error::ScopeMismatch);
    }
    let zones = {
        let mut stmt =
            tx.prepare("SELECT zone_name FROM zones WHERE namespace_id=1 ORDER BY zone_name")?;
        stmt.query_map([], |row| row.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    if zones != config.zones {
        return Err(Error::ScopeMismatch);
    }
    let (stored_version, generation, saved_fingerprint): (i64, String, Vec<u8>) = tx.query_row(
        "SELECT schema_version, database_generation, signing_fingerprint FROM server_meta WHERE singleton=1", [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    if stored_version != if fresh { 1 } else { version } {
        return Err(Error::SchemaUnsupported);
    }
    if saved_fingerprint.as_slice() != fingerprint {
        return Err(Error::SigningKeyMismatch);
    }
    let generation = Uuid::parse_str(&generation).map_err(|_| Error::RecoveryFailed)?;
    if generation.is_nil() {
        return Err(Error::RecoveryFailed);
    }
    let seq: Option<i64> = tx
        .query_row(
            "SELECT last_committed_seq FROM namespace_counters WHERE namespace_id=1",
            [],
            |row| row.get(0),
        )
        .optional()?;
    if seq.is_none_or(|seq| seq < 0) {
        return Err(Error::RecoveryFailed);
    }
    if fresh || version == 1 {
        if seq != Some(0) {
            return Err(Error::RecoveryFailed);
        }
        tx.execute_batch(include_str!("migration_v2.sql"))?;
    }
    if fresh || version < 3 {
        tx.execute_batch(include_str!("migration_v3.sql"))?;
    }
    if fresh || version < 4 {
        tx.execute_batch(include_str!("migration_v4.sql"))?;
    }
    if fresh || version < 5 {
        tx.execute_batch(include_str!("migration_v5.sql"))?;
    }
    if fresh || version < 6 {
        tx.execute_batch(include_str!("migration_v6.sql"))?;
        // The new columns are derived, so an existing library is backfilled from the
        // same rules the write path uses rather than from a second SQL copy of them.
        super::storage::backfill_photo_index(&tx)?;
    }
    if fresh || version < 7 {
        tx.execute_batch(include_str!("migration_v7.sql"))?;
        // The recycle-bin column is derived from the committed CPLAsset fields, so the
        // same backfill keeps existing libraries consistent with the write path.
        super::storage::backfill_photo_index(&tx)?;
    }
    let last_version: i64 = tx.query_row(
        "SELECT coalesce(max(seq),0) FROM record_versions WHERE namespace_id=1",
        [],
        |row| row.get(0),
    )?;
    let last_change: i64 = tx.query_row(
        "SELECT coalesce(max(seq),0) FROM changes WHERE namespace_id=1",
        [],
        |row| row.get(0),
    )?;
    if seq != Some(last_version) || last_change != last_version {
        return Err(Error::RecoveryFailed);
    }
    tx.commit()?;
    Ok(generation)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config(container: &str) -> Config {
        Config::from_pairs([
            ("SYNC_DATA_DIR", "/unused/data"),
            ("SYNC_API_KEY_FILE", "/unused/api"),
            ("SYNC_SIGNING_KEY_FILE", "/unused/signing"),
            ("SYNC_CONTAINER", container),
        ])
        .unwrap()
    }
    fn v1(connection: &mut Connection, generation: Uuid) {
        // Frozen schema from M1 is a migration input, not an M2 terminal state.
        let tx = connection.transaction().unwrap();
        tx.execute_batch(include_str!("schema.sql")).unwrap();
        tx.execute(
            "INSERT INTO server_meta VALUES(1,1,?1,?2)",
            params![generation.to_string(), [1u8; 32].as_slice()],
        )
        .unwrap();
        tx.execute("INSERT INTO namespaces VALUES(1,'local','com.apple.photos.cloud','production','private')",[]).unwrap();
        tx.execute(
            "INSERT INTO zones VALUES(1,'PrimarySync','_defaultOwner')",
            [],
        )
        .unwrap();
        tx.execute("INSERT INTO namespace_counters VALUES(1,0)", [])
            .unwrap();
        tx.pragma_update(None, "application_id", APPLICATION_ID)
            .unwrap();
        tx.pragma_update(None, "user_version", 1).unwrap();
        tx.commit().unwrap();
    }
    #[test]
    fn upgrades_v1_without_changing_namespace_generation_or_signing_identity() {
        let root = tempfile::tempdir().unwrap();
        let mut db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
        let generation = Uuid::new_v4();
        v1(&mut db, generation);
        assert_eq!(
            initialize(&mut db, &config("com.apple.photos.cloud"), &[1; 32]).unwrap(),
            generation
        );
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        assert_eq!(
            db.query_row("SELECT count(*) FROM record_versions", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        assert_eq!(
            initialize(&mut db, &config("com.apple.photos.cloud"), &[1; 32]).unwrap(),
            generation
        );
    }
    #[test]
    fn rejected_scope_does_not_partially_upgrade_old_schema() {
        let root = tempfile::tempdir().unwrap();
        let mut db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
        v1(&mut db, Uuid::new_v4());
        assert!(matches!(
            initialize(&mut db, &config("other"), &[1; 32]),
            Err(Error::ScopeMismatch)
        ));
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            1
        );
        assert_eq!(
            db.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='records'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }

    /// A v6 library already contains trashed photos as committed record fields; the
    /// new column must be derived from those rows, not left at its 0 default.
    #[test]
    fn v7_backfills_the_recycle_bin_from_committed_fields() {
        let root = tempfile::tempdir().unwrap();
        let mut db = Connection::open(root.path().join("metadata.sqlite")).unwrap();
        let generation = Uuid::new_v4();
        v1(&mut db, generation);
        {
            let tx = db.transaction().unwrap();
            for script in [
                include_str!("migration_v2.sql"),
                include_str!("migration_v3.sql"),
                include_str!("migration_v4.sql"),
                include_str!("migration_v5.sql"),
                include_str!("migration_v6.sql"),
            ] {
                tx.execute_batch(script).unwrap();
            }
            let fields = r#"{"relativePath":{"type":"STRING","value":"Camera/IMG_1.png"},"isTrashed":{"type":"INT64","value":1}}"#;
            tx.execute(
                "INSERT INTO records(namespace_id,zone_name,record_name,record_type,revision,seq,fields_json,deleted)
                 VALUES(1,'PrimarySync','master-1','CPLMaster','rev-m',1,'{}',0)",
                [],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO records(namespace_id,zone_name,record_name,record_type,revision,seq,fields_json,deleted)
                 VALUES(1,'PrimarySync','photo-1','CPLAsset','rev-p',2,?1,0)",
                params![fields],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO record_versions(namespace_id,zone_name,record_name,seq,record_type,revision,fields_json,deleted)
                 VALUES(1,'PrimarySync','master-1',1,'CPLMaster','rev-m','{}',0)",
                [],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO record_versions(namespace_id,zone_name,record_name,seq,record_type,revision,fields_json,deleted)
                 VALUES(1,'PrimarySync','photo-1',2,'CPLAsset','rev-p',?1,0)",
                params![fields],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO changes VALUES(1,1,'PrimarySync','master-1')",
                [],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO changes VALUES(1,2,'PrimarySync','photo-1')",
                [],
            )
            .unwrap();
            tx.execute(
                "INSERT INTO photo_bindings(namespace_id,zone_name,photo_id,master_record_name,
                    relative_path,content_generation,original_asset_id,thumbnail_asset_id)
                 VALUES(1,'PrimarySync','photo-1','master-1','Camera/IMG_1.png',
                    '11111111-1111-4111-8111-111111111111',
                    'aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa',
                    'bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb')",
                [],
            )
            .unwrap();
            tx.execute(
                "UPDATE namespace_counters SET last_committed_seq=2 WHERE namespace_id=1",
                [],
            )
            .unwrap();
            tx.commit().unwrap();
        }
        assert_eq!(
            initialize(&mut db, &config("com.apple.photos.cloud"), &[1; 32]).unwrap(),
            generation
        );
        assert_eq!(
            db.pragma_query_value::<i64, _>(None, "user_version", |row| row.get(0))
                .unwrap(),
            SCHEMA_VERSION
        );
        let trashed: i64 = db
            .query_row(
                "SELECT trashed FROM photo_bindings WHERE photo_id='photo-1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(trashed, 1);
    }
}
