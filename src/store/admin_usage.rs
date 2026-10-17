//! Cross-account accounting for the administration console.
//!
//! An account's own store is opened by [`crate::store::Store`], which takes that
//! directory's exclusive `.owner.lock` and runs migrations and recovery. This
//! module therefore never opens a second writer for a foreign account: it reads
//! the account's metadata database through one connection that takes no lock, runs
//! no migration and enqueues no work on that account's database executor.
//!
//! Every read here is metadata only. No object body is opened, read or hashed, and
//! nothing here advances an account's synchronization generation or commit sequence.
use super::{files::open_private, storage::Usage};
use crate::Result;
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;

/// Usage of one account, plus whether the reservation figure is exact.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountUsage {
    pub usage: Usage,
    /// An account with no store on disk yet has consumed nothing.
    pub has_store: bool,
    /// Bytes actually held on disk for this account, distinct from logical usage:
    /// objects are deduplicated by content, so two files with identical bytes are
    /// charged twice against the quota but stored once.
    pub physical_bytes: u64,
}

impl AccountUsage {
    fn empty(quota_bytes: u64) -> Self {
        Self {
            usage: Usage::zero(quota_bytes),
            has_store: false,
            physical_bytes: 0,
        }
    }
}

/// Reads one account's logical usage from its own directory.
///
/// # Safety-relevant invariants
/// The database file is opened through [`open_private`], so a symlink or a file
/// that is not privately owned and single-linked is refused rather than followed.
pub fn account_usage(directory: &Path, quota_bytes: u64) -> Result<AccountUsage> {
    let database = directory.join("metadata.sqlite");
    if database.symlink_metadata().is_err() {
        return Ok(AccountUsage::empty(quota_bytes));
    }
    // The descriptor is validated first, so a planted symlink is refused before it
    // can be handed to SQLite.
    let _guard = open_private(&database, false)?;
    let connection = Connection::open(&database)?;
    connection.busy_timeout(std::time::Duration::from_secs(5))?;
    let usage = super::storage::usage_foreign(&connection, quota_bytes)?;
    drop(connection);
    Ok(AccountUsage {
        usage,
        has_store: true,
        physical_bytes: owned_bytes(directory, false)?,
    })
}

/// Total bytes held under a directory tree, by `stat` only.
pub fn directory_bytes(root: &Path) -> Result<u64> {
    let mut total: u64 = 0;
    let mut pending = vec![root.to_path_buf()];
    while let Some(current) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            // A symlink is never followed: only regular files and real directories
            // contribute, so a planted link cannot inflate or escape the count.
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                pending.push(entry.path());
            } else if kind.is_file()
                && let Ok(meta) = entry.metadata()
            {
                total = total.saturating_add(meta.len());
            }
        }
    }
    Ok(total)
}

/// Bytes one account holds on disk.
///
/// A normal account owns its whole directory. The account that adopted the legacy
/// storage instead owns `objects/`, `staging/` and its database at the data root,
/// while the `users/` tree below belongs to other accounts and is excluded.
pub fn owned_bytes(directory: &Path, legacy: bool) -> Result<u64> {
    if !legacy {
        return directory_bytes(directory);
    }
    let mut total = directory_bytes(&directory.join("objects"))?;
    total = total.saturating_add(directory_bytes(&directory.join("staging"))?);
    if let Ok(meta) = std::fs::symlink_metadata(directory.join("metadata.sqlite"))
        && meta.is_file()
    {
        total = total.saturating_add(meta.len());
    }
    Ok(total)
}
