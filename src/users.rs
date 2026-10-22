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
pub const ADMIN_SESSION_TTL_SECONDS: i64 = 2 * 60 * 60;
const APPLICATION_ID: i64 = 0x53595541;

/// Account role. Only `admin` may reach the console's administration API; the
/// distinction is server owned and travels with the session, never with a request
/// parameter.
#[derive(Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Admin,
}

impl Role {
    pub fn parse(raw: &str) -> Result<Self> {
        match raw {
            "user" => Ok(Role::User),
            "admin" => Ok(Role::Admin),
            _ => Err(invalid("role must be user or admin")),
        }
    }
    pub fn is_admin(self) -> bool {
        self == Role::Admin
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Role::User => "user",
            Role::Admin => "admin",
        }
    }
}

#[derive(Clone, Serialize)]
pub struct User {
    pub id: String,
    pub username: String,
    pub role: Role,
    #[serde(skip)]
    pub(crate) legacy: bool,
    /// Per-user override of `SYNC_USER_STORAGE_QUOTA_BYTES`; `None` inherits the
    /// deployment default. Never serialized: quota is not part of the login identity.
    #[serde(skip)]
    pub(crate) quota_bytes: Option<i64>,
}

/// Catalogue row for the administration console. Unlike [`User`] this carries
/// lifecycle fields that must never leak into a session response.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    pub id: String,
    pub username: String,
    pub role: Role,
    pub enabled: bool,
    pub legacy: bool,
    pub created_at: i64,
    pub quota_bytes: Option<u64>,
    pub active_sessions: u64,
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
/// The shortest password this deployment accepts. Eight characters is a floor, not a
/// recommendation: it rules out the passwords that get guessed first, and Argon2id plus
/// a per-account random salt is what actually does the work.
pub const MIN_PASSWORD_CHARS: usize = 8;

pub fn validate_password(password: &str) -> Result<()> {
    if password.len() > 1024
        || password.chars().count() < MIN_PASSWORD_CHARS
        || password.chars().any(char::is_control)
    {
        return Err(invalid(
            "password must contain at least 8 characters, at most 1024 UTF-8 bytes and no control characters",
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
/// Reading the role column must fail loudly on unexpected stored text rather than
/// silently downgrading it, so it surfaces as a row-conversion error.
fn role_column(raw: &str) -> rusqlite::Result<Role> {
    Role::parse(raw).map_err(|_| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::other("unexpected stored account role")),
        )
    })
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
        if version > 2 || (version > 0 && app != APPLICATION_ID) {
            return Err(Error::SchemaUnsupported);
        }
        conn.execute_batch("BEGIN IMMEDIATE;CREATE TABLE IF NOT EXISTS users(id TEXT PRIMARY KEY,username TEXT NOT NULL UNIQUE,password_hash TEXT NOT NULL,auth_version INTEGER NOT NULL DEFAULT 1,enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0,1)),legacy INTEGER NOT NULL DEFAULT 0 CHECK(legacy IN (0,1)),created_at INTEGER NOT NULL);CREATE UNIQUE INDEX IF NOT EXISTS sole_legacy_owner ON users(legacy) WHERE legacy=1;CREATE TABLE IF NOT EXISTS sessions(digest BLOB PRIMARY KEY,user_id TEXT NOT NULL REFERENCES users(id),auth_version INTEGER NOT NULL,expires_at INTEGER NOT NULL);CREATE INDEX IF NOT EXISTS sessions_user ON sessions(user_id);COMMIT;")?;
        if version < 2 {
            // Accounts that already exist keep their identity, sessions and photos.
            // They simply become ordinary users that inherit the deployment quota.
            conn.execute_batch("BEGIN IMMEDIATE;ALTER TABLE users ADD COLUMN role TEXT NOT NULL DEFAULT 'user' CHECK(role IN ('user','admin'));ALTER TABLE users ADD COLUMN quota_bytes INTEGER CHECK(quota_bytes IS NULL OR (quota_bytes>0 AND quota_bytes<=9223372036854775807));PRAGMA application_id=1398363457;PRAGMA user_version=2;COMMIT;")?;
        }
        // Existing administrator sessions from an older binary also adopt the cap.
        conn.execute("UPDATE sessions SET expires_at=MIN(expires_at,?1) WHERE user_id IN (SELECT id FROM users WHERE role='admin')", [now()+ADMIN_SESSION_TTL_SECONDS])?;
        crate::store::files::cap_database(&conn, 67_108_864)?;
        Ok(Self {
            connection: Mutex::new(conn),
            dummy_hash: hash("non-account dummy password")?,
        })
    }
    fn lock(&self) -> Result<std::sync::MutexGuard<'_, Connection>> {
        self.connection.lock().map_err(|_| Error::WorkerFailed)
    }
    pub fn create(
        &self,
        username: &str,
        password: &str,
        adopt_legacy: bool,
        role: Role,
    ) -> Result<User> {
        let username = canonical_username(username)?;
        validate_password(password)?;
        let encoded = hash(password)?;
        let user = User {
            id: uuid::Uuid::new_v4().to_string(),
            username,
            role,
            legacy: adopt_legacy,
            quota_bytes: None,
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
            "INSERT INTO users(id,username,password_hash,legacy,created_at,role) VALUES(?1,?2,?3,?4,?5,?6)",
            params![user.id, user.username, encoded, user.legacy, now(), user.role.as_str()],
        )?;
        tx.commit()?;
        Ok(user)
    }
    /// Catalogue rows with lifecycle state, for the administration console only.
    pub fn accounts(&self) -> Result<Vec<Account>> {
        let conn = self.lock()?;
        let mut stmt = conn.prepare(
            "SELECT u.id,u.username,u.role,u.enabled,u.legacy,u.created_at,u.quota_bytes,
                    (SELECT COUNT(*) FROM sessions s WHERE s.user_id=u.id AND s.expires_at>?1)
             FROM users u ORDER BY u.username",
        )?;
        Ok(stmt
            .query_map([now()], |r| {
                Ok(Account {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    role: role_column(&r.get::<_, String>(2)?)?,
                    enabled: r.get::<_, i64>(3)? != 0,
                    legacy: r.get::<_, i64>(4)? != 0,
                    created_at: r.get(5)?,
                    quota_bytes: r
                        .get::<_, Option<i64>>(6)?
                        .map(u64::try_from)
                        .transpose()
                        .map_err(|_| {
                            rusqlite::Error::FromSqlConversionFailure(
                                6,
                                rusqlite::types::Type::Integer,
                                Box::new(std::io::Error::other("negative stored quota")),
                            )
                        })?,
                    active_sessions: r.get::<_, i64>(7)?.try_into().map_err(|_| {
                        rusqlite::Error::FromSqlConversionFailure(
                            7,
                            rusqlite::types::Type::Integer,
                            Box::new(std::io::Error::other("negative stored session count")),
                        )
                    })?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    /// A role change applies to the next authorization and never revokes sessions.
    pub fn set_role(&self, id: &str, role: Role) -> Result<()> {
        let mut conn = self.lock()?;
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE users SET role=?2 WHERE id=?1",
            params![id, role.as_str()],
        )? != 1
        {
            return Err(invalid("unknown user"));
        }
        if role == Role::Admin {
            tx.execute(
                "UPDATE sessions SET expires_at=MIN(expires_at,?2) WHERE user_id=?1",
                params![id, now() + ADMIN_SESSION_TTL_SECONDS],
            )?;
        }
        if role == Role::User {
            let admins: i64 = tx.query_row(
                "SELECT COUNT(*) FROM users WHERE role='admin' AND enabled=1",
                [],
                |r| r.get(0),
            )?;
            if admins == 0 {
                return Err(Error::LastAdministrator);
            }
        }
        tx.commit()?;
        Ok(())
    }
    /// A quota change never deletes bytes and never revokes sessions: a lowered
    /// ceiling only stops further logical growth, exactly like the deployment default.
    pub fn set_quota(&self, id: &str, bytes: Option<u64>) -> Result<()> {
        let bytes = bytes
            .map(|value| i64::try_from(value).map_err(|_| invalid("quota is too large")))
            .transpose()?;
        if bytes == Some(0) {
            return Err(invalid("quota must be a positive byte count"));
        }
        let mut conn = self.lock()?;
        let tx = conn.transaction()?;
        if tx.execute(
            "UPDATE users SET quota_bytes=?2 WHERE id=?1",
            params![id, bytes],
        )? != 1
        {
            return Err(invalid("unknown user"));
        }
        tx.commit()?;
        Ok(())
    }
    pub fn list(&self) -> Result<Vec<User>> {
        let conn = self.lock()?;
        let mut stmt = conn
            .prepare("SELECT id,username,legacy,role,quota_bytes FROM users ORDER BY username")?;
        Ok(stmt
            .query_map([], |r| {
                Ok(User {
                    id: r.get(0)?,
                    username: r.get(1)?,
                    legacy: r.get::<_, i64>(2)? != 0,
                    role: role_column(&r.get::<_, String>(3)?)?,
                    quota_bytes: r.get(4)?,
                })
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?)
    }
    pub fn has_users(&self) -> Result<bool> {
        Ok(self
            .lock()?
            .query_row("SELECT EXISTS(SELECT 1 FROM users)", [], |r| r.get(0))?)
    }
    /// Catalogue management addresses accounts by immutable id; this resolves the
    /// one human-facing name the CLI takes.
    pub fn id_of(&self, username: &str) -> Result<String> {
        let name = canonical_username(username)?;
        self.lock()?
            .query_row("SELECT id FROM users WHERE username=?1", [&name], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .ok_or_else(|| invalid("unknown user"))
    }

    /// The reverse lookup the administration API needs, because the catalogue's
    /// password and enable paths are keyed by name.
    pub fn username_of(&self, id: &str) -> Result<String> {
        self.lock()?
            .query_row("SELECT username FROM users WHERE id=?1", [id], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .ok_or_else(|| invalid("unknown user"))
    }
    pub fn login(&self, username: &str, password: Zeroizing<String>) -> Result<Option<Login>> {
        let name = canonical_username(username).unwrap_or_default();
        let record:Option<(User,String,i64,bool)>=self.lock()?.query_row("SELECT id,username,legacy,role,quota_bytes,password_hash,auth_version,enabled FROM users WHERE username=?1",[&name],|r|Ok((User {id:r.get(0)?,username:r.get(1)?,legacy:r.get::<_,i64>(2)? != 0,role:role_column(&r.get::<_,String>(3)?)?,quota_bytes:r.get(4)?},r.get(5)?,r.get(6)?,r.get(7)?))).optional()?;
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
        let expires = now()
            + if user.role.is_admin() {
                ADMIN_SESSION_TTL_SECONDS
            } else {
                SESSION_TTL_SECONDS
            };
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
        Ok(self.lock()?.query_row("SELECT u.id,u.username,u.legacy,u.role,u.quota_bytes FROM sessions s JOIN users u ON u.id=s.user_id WHERE s.digest=?1 AND s.expires_at>?2 AND s.auth_version=u.auth_version AND u.enabled=1",params![digest.as_slice(),now()],|r|Ok(User {id:r.get(0)?,username:r.get(1)?,legacy:r.get::<_,i64>(2)? != 0,role:role_column(&r.get::<_,String>(3)?)?,quota_bytes:r.get(4)?})).optional()?)
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
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if tx.execute(
            "UPDATE users SET enabled=?2,auth_version=auth_version+1 WHERE username=?1",
            params![name, enabled],
        )? != 1
        {
            return Err(invalid("unknown user"));
        }
        if !enabled {
            // An administrator may hand the role to another account and then step
            // down, but the deployment must never be left with nobody able to
            // administer it. Only an account that actually holds the role is
            // constrained: disabling an ordinary user is always allowed, including in
            // a deployment that has no administrator at all.
            let is_admin: bool = tx.query_row(
                "SELECT role='admin' FROM users WHERE username=?1",
                [&name],
                |r| r.get(0),
            )?;
            if is_admin {
                let remaining: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM users WHERE role='admin' AND enabled=1 AND username<>?1",
                    [&name],
                    |r| r.get(0),
                )?;
                if remaining == 0 {
                    return Err(Error::LastAdministrator);
                }
            }
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
            .create("alice", "same synthetic password", false, Role::User)
            .unwrap();
        users
            .create("bob", "same synthetic password", false, Role::User)
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
                .create("../other", "valid synthetic password", false, Role::User)
                .is_err()
        );
        assert!(users.create("alice", "short", false, Role::User).is_err());
        users
            .create("owner", "valid synthetic password", true, Role::User)
            .unwrap();
        assert!(
            users
                .create("other", "valid synthetic password", true, Role::User)
                .is_err()
        );
        assert_eq!(users.list().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod password_tests {
    use super::{MIN_PASSWORD_CHARS, validate_password};

    #[test]
    fn eight_characters_is_the_floor() {
        assert!(
            validate_password("1234567").is_err(),
            "seven is below the floor"
        );
        assert!(
            validate_password("12345678").is_ok(),
            "exactly eight characters is accepted"
        );
        assert_eq!(MIN_PASSWORD_CHARS, 8);
        // The floor counts characters, not bytes, so a multi-byte passphrase of
        // eight characters is as acceptable as eight ASCII ones.
        assert!(validate_password("密码密码密码密码").is_ok());
        assert!(validate_password("密码密码密码密").is_err());
    }

    #[test]
    fn the_unchanged_rules_still_hold() {
        assert!(
            validate_password(&"a".repeat(1025)).is_err(),
            "the upper bound stands"
        );
        assert!(validate_password(&"a".repeat(1024)).is_ok());
        // Control characters would corrupt a terminal, a header and a log line alike.
        assert!(validate_password("abcdefgh\n").is_err());
        assert!(validate_password("abcdefgh\t").is_err());
        assert!(
            validate_password("        ").is_ok(),
            "spaces are not control characters"
        );
    }
}
