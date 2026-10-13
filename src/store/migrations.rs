use crate::{Config, Error, Result};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use uuid::Uuid;

const APPLICATION_ID: i64 = 0x53594e43;
pub(crate) const SCHEMA_VERSION: i64 = 1;

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
    if !fresh && (application != APPLICATION_ID || version != SCHEMA_VERSION) {
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
            params![
                SCHEMA_VERSION,
                Uuid::new_v4().to_string(),
                fingerprint.as_slice()
            ],
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
        tx.pragma_update(None, "user_version", SCHEMA_VERSION)?;
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
    if stored_version != SCHEMA_VERSION {
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
    if seq != Some(0) {
        return Err(Error::RecoveryFailed);
    } // M1 has no record changes.
    tx.commit()?;
    Ok(generation)
}
