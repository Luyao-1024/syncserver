use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

// Messages deliberately exclude filesystem paths, SQL, and credential contents.
#[derive(Debug, Error)]
pub enum Error {
    #[error("PROTOCOL_ERROR: {0:?}")]
    Protocol(crate::domain::records::Failure),
    #[error("CONFIG_INVALID: {0}")]
    Config(&'static str),
    #[error("CONFIG_SCOPE_MISMATCH: namespace or zone set differs from the existing database")]
    ScopeMismatch,
    #[error("SIGNING_KEY_MISMATCH: existing directory requires its original signing key")]
    SigningKeyMismatch,
    #[error("DATA_DIR_LOCKED: another process owns this data directory")]
    DataDirLocked,
    #[error("SCHEMA_UNSUPPORTED: database schema is newer or belongs to another application")]
    SchemaUnsupported,
    #[error("RECOVERY_FAILED: database metadata is inconsistent")]
    RecoveryFailed,
    #[error("DATABASE_ERROR: database operation failed")]
    Database(#[source] rusqlite::Error),
    #[error("IO_ERROR: local I/O operation failed")]
    Io(#[source] std::io::Error),
    #[error("SERVER_BUSY: database queue is full")]
    Busy,
    #[error("SERVICE_UNAVAILABLE: database executor has stopped")]
    Unavailable,
    #[error("WORKER_FAILED: database executor terminated unexpectedly")]
    WorkerFailed,
}

impl From<std::io::Error> for Error {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(value: rusqlite::Error) -> Self {
        Self::Database(value)
    }
}

impl From<crate::domain::records::Failure> for Error {
    fn from(value: crate::domain::records::Failure) -> Self {
        Self::Protocol(value)
    }
}
