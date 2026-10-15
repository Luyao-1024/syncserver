//! Privileged user catalogue and revocable login sessions. Not photo metadata.
use crate::{
    Error, Result,
    store::files::{directory, open_private},
};
use argon2::{Argon2, PasswordHash, PasswordHasher, PasswordVerifier, password_hash::SaltString};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};
use zeroize::Zeroizing;

pub const SESSION_TTL_SECONDS: i64 = 30 * 24 * 60 * 60;
const APPLICATION_ID: i64 = 0x53595541;
#[derive(Clone, Serialize)]
pub struct User {
    pub id: String,
    pub username: String,
    #[serde(skip)]
    pub(crate) legacy: bool,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Login {
    pub access_token: String,
    pub token_type: &'static str,
    pub expires_at: i64,
    pub user: User,
}
pub struct Users {
    connection: Mutex<Connection>,
    dummy_hash: String,
}
fn invalid(message: &'static str) -> Error {
    Error::Config(message)
}
pub fn canonical_username(raw: &str) -> Result<String> {
    let name = raw.to_ascii_lowercase();
    if name.is_empty()
        || name.len() > 64
        || !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    {
        return Err(invalid(
            "username must be 1-64 ASCII letters/digits/._-, starting with a letter or digit",
        ));
    }
    Ok(name)
}
pub fn validate_password(password: &str) -> Result<()> {
    if password.len() > 1024
        || password.chars().count() < 12
        || password.chars().any(char::is_control)
    {
        return Err(invalid(
            "password must contain at least 12 characters, at most 1024 UTF-8 bytes and no control characters",
        ));
    }
    Ok(())
}
fn random_bytes() -> Result<[u8; 32]> {
    use std::io::Read;
    let mut bytes = [0u8; 32];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes)
}
fn hash(password: &str) -> Result<String> {
    let salt = SaltString::encode_b64(&random_bytes()?[..16]).map_err(|_| Error::WorkerFailed)?;
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| Error::WorkerFailed)
}
fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
impl Users {
    /// Can be opened by the CLI while the HTTP process holds its photo store.
    pub fn open(root: &Path) -> Result<Self> {
        directory(root)?;
        let auth = root.join(".authentication");
        directory(&auth)?;
        let file = auth.join("users.sqlite");
        open_private(&file, true)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let side = auth.join(format!("users.sqlite{suffix}"));
            if side.symlink_metadata().is_ok() {
                open_private(&side, false)?;
            }
        }
        let conn = Connection::open(file)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.execute_batch(
            "PRAGMA foreign_keys=ON;PRAGMA journal_mode=WAL;PRAGMA synchronous=FULL;",
        )?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        let app: i64 = conn.pragma_query_value(None, "application_id", |r| r.get(0))?;
        if version > 1 || (version > 0 && app != APPLICATION_ID) {
            return Err(Error::SchemaUnsupported);
        }
        conn.execute_batch("BEGIN IMMEDIATE;CREATE TABLE IF NOT EXISTS users(id TEXT PRIMARY KEY,username TEXT NOT NULL UNIQUE,password_hash TEXT NOT NULL,auth_version INTEGER NOT NULL DEFAULT 1,enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),legacy INTEGER NOT NULL DEFAULT 0 CHECK(legacy IN (0,1)),created_at INTEGER NOT NULL);CREATE UNIQUE INDEX IF NOT EXISTS sole_legacy_owner ON users(legacy) WHERE legacy=1;CREATE TABLE IF NOT EXISTS sessions(digest BLOB PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id),auth_version INTEGER NOT NULL,expires_at INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS sessions_user ON sessions(user_id);PRAGMA application_id=1398363457;PRAGMA user_version=1;COMMIT;")?;
        Ok(Self {
            connection: Mutex::new(conn),
            dummy_hash: hash("non-account dummy password")?,
        })
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| Error::WorkerFailed)
    }
    pub fn create(&self, username: &str, password: &str, adopt_legacy: bool) -> Result<User> {
        let username = canonical_username(username)?;
        validate_password(password)?;
        let encoded = hash(password)?;
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            username,
            legacy: adopt_legacy,
        };
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let exists:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM users WHERE username=?1) OR (?2 AND EXISTS(SELECT 1 FROM users WHERE legacy=1))",params![user.username,user.legacy],|r|r.get(0))?;
        if exists {
            return Err(invalid(
                "username already exists or legacy storage already has an owner",
            ));
        }
        tx.execute(
            "INSERT INTO users(id,username,password_hash,legacy,created_at) VALUES(?1,?2,?3,?4,?5)",
            params![user.id, user.username, encoded, user.legacy, now()],
        )?;
        tx.commit()?;
        Ok(user)
    }
    pub fn list(&self) -> Result<Vec<User>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare("SELECT id,username,legacy FROM users ORDER BY username")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(User {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    legacy: r.get(2)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn has_users(&self) -> Result<bool> {
        Ok(self
            .lock()?
            .query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |r| r.get(0))?)
    }
    pub fn login(&self, username: &str, password: Zeroizing<String>) -> Result<Option<Login>> {
        let name = canonical_username(username).unwrap_or_default();
        let record:Option<(User,String,i64,bool)>=self.lock()?.query_row("SELECT id,username,legacy,password_hash,auth_version,enabled FROM users WHERE username=?1",[&name],|r|Ok((User {id:r.get(0)?,username:r.get(1)?,legacy:r.get(2)?},r.get(3)?,r.get(4)?,r.get(5)?))).optional()?;
        let encoded = record
            .as_ref()
            .map(|r| r.1.as_str())
            .unwrap_or(&self.dummy_hash);
        let parsed = PasswordHash::new(encoded).map_err(|_| Error::RecoveryFailed)?;
        let matches = Argon2::default()
            .verify_password(password.as_bytes(), &parsed)
            .is_ok();
        let Some((user, _, version, true)) = record else {
            return Ok(None);
        };
        if !matches {
            return Ok(None);
        }
        let random = random_bytes()?;
        let token = format!("ss1_{}", URL_SAFE_NO_PAD.encode(random));
        let digest = Sha256::digest(token.as_bytes());
        let expires = now() + SESSION_TTL_SECONDS;
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let current: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE id=?1 AND enabled=1 AND auth_version=?2)",
            params![user.id, version],
            |r| r.get(0),
        )?;
        if !current {
            return Ok(None);
        }
        tx.execute("DELETE FROM sessions WHERE expires_at<=?1", [now()])?;
        tx.execute(
            "INSERT INTO sessions(digest,user_id,auth_version,expires_at) VALUES(?1,?2,?3,?4)",
            params![digest.as_slice(), user.id, version, expires],
        )?;
        tx.commit()?;
        Ok(Some(Login {
            access_token: token,
            token_type: "Bearer",
            expires_at: expires,
            user,
        }))
    }
    pub fn authenticate(&self, token: &str) -> Result<Option<User>> {
        if token.len() != 47 || !token.starts_with("ss1_") {
            return Ok(None);
        }
        let digest = Sha256::digest(token.as_bytes());
        Ok(self.lock()?.query_row("SELECT u.id,u.username,u.legacy FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.digest=?1 AND s.expires_at>?2 AND s.auth_version=u.auth_version AND u.enabled=1",params![digest.as_slice(),now()],|r|Ok(User {id:r.get(0)?,username:r.get(1)?,legacy:r.get(2)?})).optional()?)
    }
    pub fn logout(&self, token: &str) -> Result<()> {
        self.lock()?.execute(
            "DELETE FROM sessions WHERE digest=?1",
            [Sha256::digest(token.as_bytes()).as_slice()],
        )?;
        Ok(())
    }
    pub fn change_password(&self, username: &str, password: &str) -> Result<()> {
        let name = canonical_username(username)?;
        validate_password(password)?;
        let encoded = hash(password)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if tx.execute(
            "UPDATE users SET password_hash=?2,auth_version=auth_version+1 WHERE username=?1",
            params![name, encoded],
        )? != 1
        {
            return Err(invalid("unknown user"));
        }
        tx.execute(
            "DELETE FROM sessions WHERE user_id=(SELECT id FROM users WHERE username=?1)",
            [name],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn set_enabled(&self, username: &str, enabled: bool) -> Result<()> {
        let name = canonical_username(username)?;
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if tx.execute(
            "UPDATE users SET enabled=?2,auth_version=auth_version+1 WHERE username=?1",
            params![name, enabled],
        )? != 1
        {
            return Err(invalid("unknown user"));
        }
        tx.execute(
            "DELETE FROM sessions WHERE user_id=(SELECT id FROM users WHERE username=?1)",
            [name],
        )?;
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn passwords_use_independent_salts_and_expired_sessions_cannot_authorize() {
        let root = tempfile::tempdir().unwrap();
        let users = Users::open(&root.path().join("data")).unwrap();
        users
            .create("alice", "same synthetic password", false)
            .unwrap();
        users
            .create("bob", "same synthetic password", false)
            .unwrap();
        let hashes = {
            let conn = users.lock().unwrap();
            let mut statement = conn
                .prepare("SELECT password_hash FROM users ORDER BY username")
                .unwrap();
            statement
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        assert_ne!(hashes[0], hashes[1]);
        assert!(hashes.iter().all(|h| h.starts_with("$argon2id$")));
        let session = users
            .login("alice", Zeroizing::new("same synthetic password".into()))
            .unwrap()
            .unwrap();
        assert!(users.authenticate(&session.access_token).unwrap().is_some());
        // Fault-inject the clock boundary; do not manufacture successful state.
        users
            .lock()
            .unwrap()
            .execute("UPDATE sessions SET expires_at=0", [])
            .unwrap();
        assert!(users.authenticate(&session.access_token).unwrap().is_none());
        drop(users);
        let users = Users::open(&root.path().join("data")).unwrap();
        assert!(users.authenticate(&session.access_token).unwrap().is_none());
    }
    #[test]
    fn invalid_names_passwords_and_repeated_legacy_adoption_do_not_replace_users() {
        let root = tempfile::tempdir().unwrap();
        let users = Users::open(&root.path().join("data")).unwrap();
        assert!(
            users
                .create("../other", "valid synthetic password", false)
                .is_err()
        );
        assert!(users.create("alice", "short", false).is_err());
        users
            .create("owner", "valid synthetic password", true)
            .unwrap();
        assert!(
            users
                .create("other", "valid synthetic password", true)
                .is_err()
        );
        assert_eq!(users.list().unwrap().len(), 1);
    }
}
