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
    #[error("LOG_STORAGE_FAILED: log database operation failed")]
    LogStorage,
}

impl Error {
    /// Short stable code without detail text, safe for logs and operator output.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Protocol(_) => "PROTOCOL_ERROR",
            Self::Config(_) => "CONFIG_INVALID",
            Self::ScopeMismatch => "CONFIG_SCOPE_MISMATCH",
            Self::SigningKeyMismatch => "SIGNING_KEY_MISMATCH",
            Self::DataDirLocked => "DATA_DIR_LOCKED",
            Self::SchemaUnsupported => "SCHEMA_UNSUPPORTED",
            Self::RecoveryFailed => "RECOVERY_FAILED",
            Self::Database(_) => "DATABASE_ERROR",
            Self::Io(_) => "IO_ERROR",
            Self::Busy => "SERVER_BUSY",
            Self::Unavailable => "SERVICE_UNAVAILABLE",
            Self::WorkerFailed => "WORKER_FAILED",
            Self::LogStorage => "LOG_STORAGE_FAILED",
        }
    }
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
