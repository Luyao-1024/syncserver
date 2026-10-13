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

fn directory(path: &Path) -> Result<()> {
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
