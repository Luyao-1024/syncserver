//! Blocking stream sink, bounded thumbnail validation, stable no-overwrite publication.
use crate::{
    Error, Result,
    domain::{
        assets::{Asset, Role, Session, Verified},
        records::Failure,
    },
    store::files::open_private,
};
use axum::body::Bytes;
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};
use tokio::sync::mpsc;
use uuid::Uuid;
#[derive(Default)]
pub(crate) struct Metrics {
    pub bytes_read: AtomicU64,
    pub bytes_hashed: AtomicU64,
    pub uploaded_bytes: AtomicU64,
}
pub(crate) struct Sink {
    pub root: PathBuf,
    pub session: Session,
    pub receiver: mpsc::Receiver<Bytes>,
    pub metrics: Arc<Metrics>,
}
pub(crate) fn temporary(root: &Path, name: &str) -> PathBuf {
    root.join("staging").join(name)
}
pub(crate) fn object(root: &Path, sha: &str) -> PathBuf {
    root.join("objects").join(sha)
}
impl Sink {
    pub fn receive(mut self) -> Result<Verified> {
        let name = self
            .session
            .staging_name
            .clone()
            .ok_or(Error::RecoveryFailed)?;
        let path = temporary(&self.root, &name);
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut prefix = Vec::new();
        while let Some(chunk) = self.receiver.blocking_recv() {
            size = size
                .checked_add(chunk.len() as u64)
                .ok_or_else(Failure::limit)?;
            if size > self.session.expected_size {
                return Err(Failure::limit().into());
            }
            file.write_all(&chunk)?;
            hash.update(&chunk);
            self.metrics
                .bytes_hashed
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            self.metrics
                .uploaded_bytes
                .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            let take = 64usize.saturating_sub(prefix.len()).min(chunk.len());
            prefix.extend_from_slice(&chunk[..take]);
        }
        if size != self.session.expected_size {
            tracing::warn!(
                upload_id = %self.session.id,
                declared_bytes = self.session.expected_size,
                received_bytes = size,
                "upload body length did not match the reservation"
            );
            return Err(Failure::new(
                400,
                "UPLOAD_SIZE_MISMATCH",
                "Body length does not match the reservation",
            )
            .into());
        }
        let sha: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
        if self
            .session
            .expected_sha256
            .as_ref()
            .is_some_and(|expected| expected != &sha)
        {
            // Content digests only appear when development detail logging is enabled.
            tracing::warn!(
                upload_id = %self.session.id,
                declared_digest = ?self.session.expected_sha256,
                verified_digest = %sha,
                "uploaded bytes did not match the expected digest"
            );
            return Err(Failure::new(
                400,
                "CHECKSUM_MISMATCH",
                "Uploaded bytes do not match the expected digest",
            )
            .into());
        }
        file.sync_all()?;
        drop(file);
        File::open(self.root.join("staging"))?.sync_all()?;
        let (mime, width, height, thumbnail) = if self.session.role == Role::Thumbnail {
            let mut file = open_private(&path, false)?;
            let mut bytes = Vec::new();
            file.read_to_end(&mut bytes)?;
            let mut reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
                .with_guessed_format()
                .map_err(|_| Failure::bad())?;
            let mime = match reader.format() {
                Some(image::ImageFormat::Png) => "image/png",
                Some(image::ImageFormat::Jpeg) => "image/jpeg",
                _ => {
                    return Err(Failure::new(
                        400,
                        "INVALID_THUMBNAIL",
                        "Thumbnail must be a complete bounded PNG or JPEG",
                    )
                    .into());
                }
            };
            let mut limits = image::Limits::default();
            limits.max_image_width = Some(2048);
            limits.max_image_height = Some(2048);
            limits.max_alloc = Some(32 * 1024 * 1024);
            reader.limits(limits);
            let image = reader.decode().map_err(|_| {
                Failure::new(
                    400,
                    "INVALID_THUMBNAIL",
                    "Thumbnail is malformed or exceeds dimension limits",
                )
            })?;
            if image.width() == 0 || image.height() == 0 {
                return Err(Failure::bad().into());
            }
            (
                mime.to_owned(),
                Some(image.width()),
                Some(image.height()),
                true,
            )
        } else {
            // Original bodies are opaque; only trusted bounded signature headers
            // identify common image types, never full-image decode on this path.
            let mime = if prefix.starts_with(b"\x89PNG\r\n\x1a\n") {
                "image/png"
            } else if prefix.starts_with(&[0xff, 0xd8, 0xff]) {
                "image/jpeg"
            } else {
                "application/octet-stream"
            };
            (mime.into(), None, None, false)
        };
        Ok(Verified {
            session_id: self.session.id,
            staging_name: name,
            asset: Asset {
                asset_id: Uuid::new_v4(),
                sha256: sha,
                size,
                mime_type: mime,
                width,
                height,
                thumbnail_validated: thumbnail,
            },
        })
    }
}
pub(crate) fn publish(root: &Path, verified: &Verified) -> Result<()> {
    let source = temporary(root, &verified.staging_name);
    let target = object(root, &verified.asset.sha256);
    match std::fs::hard_link(&source, &target) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            let file = open_private(&target, false)?;
            if file.metadata()?.len() != verified.asset.size {
                return Err(Error::RecoveryFailed);
            }
        }
        Err(e) => return Err(e.into()),
    }
    // hard_link is atomic and never overwrites an existing immutable object.
    std::fs::remove_file(&source)?;
    let file = open_private(&target, false)?;
    file.sync_all()?;
    File::open(root.join("objects"))?.sync_all()?;
    File::open(root.join("staging"))?.sync_all()?;
    Ok(())
}
pub(crate) fn cleanup(root: &Path, name: &str) -> Result<()> {
    match std::fs::remove_file(temporary(root, name)) {
        Ok(()) => File::open(root.join("staging"))?.sync_all()?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

/// Recover only persisted in-flight names. Old fences never gain a new success;
/// verified objects remain quarantined for safe reuse, and are never deleted.
pub(crate) fn recover(db: &mut rusqlite::Connection, root: &Path) -> Result<(u64, u64)> {
    let pending = {
        let mut stmt=db.prepare("SELECT session_id,staging_name,asset_json FROM publish_intents WHERE namespace_id=1 AND state='Prepared'")?;
        stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    let mut verified_intents = Vec::new();
    for (id, name, data) in pending {
        valid_staging(&name)?;
        let session_id = Uuid::parse_str(&id).map_err(|_| Error::RecoveryFailed)?;
        if name[..36] != session_id.to_string() {
            return Err(Error::RecoveryFailed);
        }
        let asset: Asset = serde_json::from_str(&data).map_err(|_| Error::RecoveryFailed)?;
        if !crate::domain::assets::digest(&asset.sha256) {
            return Err(Error::RecoveryFailed);
        }
        let verified = Verified {
            session_id,
            staging_name: name.clone(),
            asset,
        };
        let source = temporary(root, &name);
        let target = object(root, &verified.asset.sha256);
        let source_exists = std::fs::symlink_metadata(&source).is_ok();
        let target_exists = std::fs::symlink_metadata(&target).is_ok();
        if source_exists && !target_exists {
            publish(root, &verified)?;
        } else if source_exists {
            cleanup(root, &name)?;
        }
        verified_intents.push(verified);
    }
    // Remove every recorded receiving/failed source before checking target link
    // counts. A different cancelled intent may own the target's second link.
    let interrupted = {
        let mut stmt=db.prepare("SELECT session_id,staging_name FROM upload_sessions WHERE namespace_id=1 AND state IN('Reserved','Receiving')")?;
        stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for (id, name) in interrupted {
        if let Some(name) = name {
            valid_staging(&name)?;
            cleanup(root, &name)?;
        }
        db.execute("UPDATE upload_sessions SET state='Failed',error='UPLOAD_LEASE_LOST' WHERE namespace_id=1 AND session_id=?1",[id])?;
    }
    let failed = {
        let mut stmt=db.prepare("SELECT staging_name FROM upload_sessions WHERE namespace_id=1 AND state IN('Failed','Expired') AND staging_name IS NOT NULL")?;
        stmt.query_map([], |r| r.get::<_, String>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
    };
    for name in failed {
        valid_staging(&name)?;
        cleanup(root, &name)?;
    }
    let mut checked = std::collections::BTreeMap::new();
    let mut read = 0u64;
    let mut hashed = 0u64;
    for verified in verified_intents {
        let asset = &verified.asset;
        if let Some(size) = checked.get(&asset.sha256) {
            if *size != asset.size {
                return Err(Error::RecoveryFailed);
            }
        } else {
            let mut file = open_private(&object(root, &asset.sha256), false)?;
            if file.metadata()?.len() != asset.size {
                return Err(Error::RecoveryFailed);
            }
            let mut hash = Sha256::new();
            let mut buffer = [0u8; 64 * 1024];
            loop {
                let size = file.read(&mut buffer)?;
                if size == 0 {
                    break;
                }
                hash.update(&buffer[..size]);
                read += size as u64;
                hashed += size as u64;
            }
            let digest: String = hash.finalize().iter().map(|b| format!("{b:02x}")).collect();
            if digest != asset.sha256 {
                return Err(Error::RecoveryFailed);
            }
            checked.insert(asset.sha256.clone(), asset.size);
        }
        let tx = db.transaction()?;
        tx.execute(
            "UPDATE publish_intents SET state='Quarantined' WHERE namespace_id=1 AND session_id=?1",
            [verified.session_id.to_string()],
        )?;
        tx.execute("UPDATE upload_sessions SET state='Failed',error='UPLOAD_LEASE_LOST' WHERE namespace_id=1 AND session_id=?1",[verified.session_id.to_string()])?;
        tx.commit()?;
    }
    Ok((read, hashed))
}

fn valid_staging(name: &str) -> Result<()> {
    if !name.is_ascii()
        || name.len() != 73
        || Uuid::parse_str(&name[..36]).is_err()
        || name.as_bytes()[36] != b'-'
        || Uuid::parse_str(&name[37..]).is_err()
    {
        return Err(Error::RecoveryFailed);
    }
    Ok(())
}
