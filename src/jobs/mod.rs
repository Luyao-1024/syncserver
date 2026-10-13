use crate::{Error, Result};
use rusqlite::{Connection, TransactionBehavior, params};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoveryReport {
    pub run_id: Uuid,
    pub interrupted_starts: u64,
    pub asset_bytes_read: u64,
    pub asset_bytes_hashed: u64,
}

// Metadata recovery checks the database and settles interrupted starts. M4 will add
// durable asset intents here; never scan/re-hash all published objects.
pub(crate) fn recover(connection: &mut Connection) -> Result<RecoveryReport> {
    let integrity: String = connection.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(Error::RecoveryFailed);
    }
    let foreign_key_failure = connection.prepare("PRAGMA foreign_key_check")?.exists([])?;
    if foreign_key_failure {
        return Err(Error::RecoveryFailed);
    }
    let run_id = Uuid::new_v4();
    let tx = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let interrupted = tx.execute(
        "UPDATE service_starts SET state='Interrupted' WHERE state IN ('Recovering','Ready')",
        [],
    )?;
    tx.execute(
        "INSERT INTO service_starts (run_id, started_at_ms, state) VALUES (?1, ?2, 'Recovering')",
        params![run_id.to_string(), now_ms()?],
    )?;
    tx.commit()?;
    // Recovery phase boundary is durable before the service becomes Ready.
    connection.execute("UPDATE service_starts SET state='Ready', recovered_at_ms=?2 WHERE run_id=?1 AND state='Recovering'", params![run_id.to_string(), now_ms()?])?;
    Ok(RecoveryReport {
        run_id,
        interrupted_starts: interrupted as u64,
        asset_bytes_read: 0,
        asset_bytes_hashed: 0,
    })
}

pub(crate) fn finish(connection: &mut Connection, run_id: Uuid) -> Result<()> {
    let changed = connection.execute("UPDATE service_starts SET state='Stopped', stopped_at_ms=?2 WHERE run_id=?1 AND state='Ready'", params![run_id.to_string(), now_ms()?])?;
    if changed != 1 {
        return Err(Error::RecoveryFailed);
    }
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    Ok(())
}

fn now_ms() -> Result<i64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|time| i64::try_from(time.as_millis()).ok())
        .ok_or(Error::RecoveryFailed)
}
