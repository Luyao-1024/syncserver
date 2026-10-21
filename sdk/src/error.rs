//! The SDK error model.
//!
//! Every transport, protocol and request failure is reported through one enum so
//! callers can decide between "retry", "resync" and "stop this upload round"
//! without parsing strings.

use crate::model::{RemoteRecord, ZoneId};
use serde::Deserialize;

/// The error envelope the service uses for both non-2xx responses and in-band
/// failures inside an HTTP 200.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerFailure {
    #[serde(rename = "serverErrorCode")]
    pub code: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub record_name: Option<String>,
    #[serde(rename = "zoneID", default)]
    pub zone: Option<ZoneId>,
    #[serde(rename = "serverRecord", default)]
    pub server_record: Option<Box<RemoteRecord>>,
}

impl ServerFailure {
    pub fn detail(&self) -> String {
        if self.reason.is_empty() {
            self.code.clone()
        } else {
            format!("{}: {}", self.code, self.reason)
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid endpoint: {0}")]
    Endpoint(String),

    #[error("transport error: {0}")]
    Transport(String),

    #[error("protocol error: {0}")]
    Protocol(String),

    #[error("authentication failed: {0}")]
    Authentication(String),

    #[error("server rejected the request with HTTP {status}: {}", .failure.detail())]
    Api {
        status: u16,
        failure: Box<ServerFailure>,
    },

    #[error("server reported an error: {}", .0.detail())]
    InBand(Box<ServerFailure>),

    /// The upload lease is gone: expired, released or fenced by another device.
    /// No further mutation may be issued in this round.
    #[error("the upload lease was lost")]
    LeaseLost,

    /// Another device currently owns the upload lease.
    #[error("another device holds the upload lease")]
    LeaseBusy,

    #[error("upload phase mismatch: {0}")]
    PhaseMismatch(String),

    /// The client's applied zone token is behind the cloud. Keep the lease and
    /// finish catch-up before uploading.
    #[error("this client is not current with the zone: {0}")]
    NotCurrent(String),

    /// A snapshot or continuation token no longer belongs to the retained
    /// generation or range. Rebuild from a fresh snapshot.
    #[error("sync token expired; rebuild progress from a fresh snapshot")]
    ExpiredSyncToken,

    #[error("record not found: {0}")]
    NotFound(String),

    #[error("the idempotency key is already bound to another request")]
    IdempotencyConflict,

    #[error("the account storage quota is exhausted")]
    QuotaExceeded,

    #[error("checksum mismatch: expected {expected}, computed {actual}")]
    ChecksumMismatch { expected: String, actual: String },

    #[error("size mismatch: expected {expected} bytes, received {actual}")]
    SizeMismatch { expected: u64, actual: u64 },

    /// The service refused the uploaded bytes (format, digest or size).
    #[error("the service rejected the upload ({code}): {reason}")]
    UploadRejected { code: String, reason: String },

    /// The session is no longer usable. Uploads cannot be resumed; reserve a
    /// fresh session.
    #[error("upload session failed: {0}")]
    UploadFailed(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Graph(#[from] crate::photo::GraphError),
}

impl Error {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Api { status, .. } => Some(*status),
            Self::Authentication(_) => Some(401),
            Self::NotFound(_) => Some(404),
            Self::LeaseBusy | Self::IdempotencyConflict | Self::QuotaExceeded => Some(409),
            Self::ExpiredSyncToken => Some(400),
            _ => None,
        }
    }

    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Api { failure, .. } | Self::InBand(failure) => Some(&failure.code),
            Self::UploadRejected { code, .. } => Some(code),
            Self::ExpiredSyncToken => Some("EXPIRED_SYNC_TOKEN"),
            Self::IdempotencyConflict => Some("IDEMPOTENCY_CONFLICT"),
            Self::QuotaExceeded => Some("STORAGE_QUOTA_EXCEEDED"),
            Self::LeaseLost => Some("UPLOAD_LEASE_LOST"),
            Self::LeaseBusy => Some("UPLOAD_LOCK_BUSY"),
            _ => None,
        }
    }

    pub fn server_record(&self) -> Option<&RemoteRecord> {
        match self {
            Self::Api { failure, .. } | Self::InBand(failure) => failure.server_record.as_deref(),
            _ => None,
        }
    }

    pub fn is_lease_lost(&self) -> bool {
        matches!(self, Self::LeaseLost) || self.code() == Some("UPLOAD_LEASE_LOST")
    }

    /// Whether repeating the same logical request may succeed unchanged.
    pub fn is_retryable(&self) -> bool {
        match self {
            Self::Transport(_) | Self::LeaseBusy => true,
            Self::Api { status, .. } => matches!(status, 429 | 502 | 503 | 504),
            _ => false,
        }
    }
}

pub(crate) fn parse_failure(body: &[u8]) -> Option<ServerFailure> {
    let value: serde_json::Value = serde_json::from_slice(body).ok()?;
    value.get("serverErrorCode")?;
    serde_json::from_value(value).ok()
}

pub(crate) fn map_failure(status: u16, failure: ServerFailure) -> Error {
    let code = failure.code.clone();
    match code.as_str() {
        "UPLOAD_LEASE_LOST" => Error::LeaseLost,
        "UPLOAD_LOCK_BUSY" => Error::LeaseBusy,
        "UPLOAD_PHASE_MISMATCH" => Error::PhaseMismatch(failure.detail()),
        "SYNC_NOT_CURRENT" => Error::NotCurrent(failure.detail()),
        "EXPIRED_SYNC_TOKEN" => Error::ExpiredSyncToken,
        "UNKNOWN_ITEM" => Error::NotFound(failure.detail()),
        "IDEMPOTENCY_CONFLICT" => Error::IdempotencyConflict,
        "STORAGE_QUOTA_EXCEEDED" => Error::QuotaExceeded,
        "CHECKSUM_MISMATCH" | "UPLOAD_SIZE_MISMATCH" | "INVALID_THUMBNAIL" => {
            Error::UploadRejected {
                code: failure.code.clone(),
                reason: failure.reason.clone(),
            }
        }
        "UPLOAD_TIMEOUT" | "UPLOAD_INTERRUPTED" | "UPLOAD_EXPIRED" | "UPLOAD_STATE_CONFLICT" => {
            Error::UploadFailed(failure.detail())
        }
        "AUTHENTICATION_FAILED" | "AUTHENTICATION_REQUIRED" => {
            Error::Authentication(failure.detail())
        }
        _ => Error::Api {
            status,
            failure: Box::new(failure),
        },
    }
}

/// Map a non-success HTTP response. An absent envelope still maps by status.
pub(crate) fn map_http(status: u16, body: &[u8]) -> Error {
    if let Some(failure) = parse_failure(body) {
        return map_failure(status, failure);
    }
    match status {
        401 | 403 => Error::Authentication(format!("HTTP {status}")),
        404 => Error::NotFound(format!("HTTP {status}")),
        429 => Error::Api {
            status,
            failure: Box::new(ServerFailure {
                code: "RATE_LIMITED".into(),
                reason: "the service asked the client to retry later".into(),
                record_name: None,
                zone: None,
                server_record: None,
            }),
        },
        _ => Error::Protocol(format!("HTTP {status} without an error envelope")),
    }
}
