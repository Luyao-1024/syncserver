use crate::{Error, Result};
use fs2::FileExt;
use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

pub(crate) struct DataDirectory {
    pub database: PathBuf,
    // Lifetime is the database writer's lifetime, not the HTTP handle's lifetime.
    _lock: File,
}

pub(crate) fn open_private(path: &Path, create: bool) -> Result<File> {
    let file = OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)?;
    let meta = file.metadata()?;
    // SAFETY: geteuid has no arguments and returns the process's effective UID.
    if !meta.is_file()
        || meta.uid() != unsafe { libc::geteuid() }
        || meta.mode() & 0o777 != 0o600
        || meta.nlink() != 1
    {
        return Err(Error::Config(
            "managed files must be regular, privately owned and have one hard link",
        ));
    }
    Ok(file)
}

pub(crate) fn directory(path: &Path) -> Result<()> {
    match DirBuilder::new().recursive(true).mode(0o700).create(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no arguments.
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } || meta.mode() & 0o777 != 0o700 {
        return Err(Error::Config(
            "data directories must be owned by the service user, mode 0700 and not symbolic links",
        ));
    }
    Ok(())
}

impl DataDirectory {
    pub fn acquire(root: &Path) -> Result<Self> {
        directory(root)?;
        let root = root.canonicalize()?;
        let lock = open_private(&root.join(".owner.lock"), true)?;
        lock.try_lock_exclusive().map_err(|error| {
            if error.kind() == std::io::ErrorKind::WouldBlock {
                Error::DataDirLocked
            } else {
                Error::Io(error)
            }
        })?;
        for name in ["objects", "staging"] {
            directory(&root.join(name))?;
        }
        let database = root.join("metadata.sqlite");
        let database_file = open_private(&database, true)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let sibling = root.join(format!("metadata.sqlite{suffix}"));
            if fs::symlink_metadata(&sibling).is_ok() {
                open_private(&sibling, false)?;
            }
        }
        database_file.sync_all()?;
        File::open(&root)?.sync_all()?;
        Ok(Self {
            database,
            _lock: lock,
        })
    }
}

/// Enforce a storage envelope without trimming any existing database rows.
/// The WAL is checkpointed frequently; deployment also provides a filesystem cap.
pub(crate) fn cap_database(connection: &rusqlite::Connection, bytes: u64) -> Result<()> {
    let size: i64 = connection.pragma_query_value(None, "page_size", |row| row.get(0))?;
    connection.pragma_update(
        None,
        "max_page_count",
        i64::try_from((bytes / size as u64).max(1)).map_err(|_| Error::RecoveryFailed)?,
    )?;
    let actual: i64 = connection.pragma_query_value(None, "max_page_count", |r| r.get(0))?;
    if u128::from(actual as u64) * u128::from(size as u64) > u128::from(bytes) {
        return Err(Error::Config(
            "database cap is smaller than the existing database",
        ));
    }
    connection.pragma_update(None, "wal_autocheckpoint", 256)?;
    Ok(())
}
pub(crate) fn check_free_space(root: &Path, required: u64, reserve: u64) -> Result<()> {
    use std::{ffi::CString, os::unix::ffi::OsStrExt};
    let path = CString::new(root.as_os_str().as_bytes()).map_err(|_| Error::RecoveryFailed)?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: path is NUL-terminated and stat points to writable storage.
    if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // SAFETY: a successful statvfs initialized the structure.
    let stat = unsafe { stat.assume_init() };
    let available = u128::from(stat.f_bavail) * u128::from(stat.f_frsize);
    if available < u128::from(required) + u128::from(reserve) {
        return Err(crate::domain::records::Failure::new(
            507,
            "INSUFFICIENT_STORAGE",
            "Storage free-space reserve would be exhausted",
        )
        .into());
    }
    Ok(())
}
