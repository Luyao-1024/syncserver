//! Asset reservation, byte upload, download and storage usage.
//!
//! A digest appears in two encodings: reservations declare lowercase hex; the
//! service returns `fileChecksum` as canonical padded Base64. [`ContentDigest`]
//! owns both forms so no call site rehashes or converts by hand.
//!
//! There is **no resume**. A failed upload session is terminal; the recovery is
//! a fresh reservation, never a retry of the old session. Downloads use the
//! capability URL from a fresh [`Connection::describe_asset`] call, and the SDK
//! always sends the session bearer token as well: account mode requires both.
//!
//! Upload and download URLs are absolute capability URLs. They are accepted
//! only when their origin matches the configured endpoint, because sending the
//! bearer token and lease proof anywhere else would leak them.

use crate::Error;
use crate::client::{Connection, MAX_JSON_BYTES, decode, read_limited};
use crate::error::map_http;
use crate::lease::{LeaseProof, UploadLease};
use crate::model::{AssetDescriptor, MASTER_RECORD_TYPE, ZoneId, identifier};
use base64::{Engine, engine::general_purpose::STANDARD};
use reqwest::StatusCode;
use reqwest::header::{
    ACCEPT_RANGES, CONTENT_LENGTH, CONTENT_RANGE, CONTENT_TYPE, ETAG, HeaderValue, IF_MATCH,
    IF_NONE_MATCH, IF_RANGE, RANGE,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use url::Url;

/// Transfers are bounded by the service's own upload/download limits, not by
/// the short control-request timeout.
const TRANSFER_TIMEOUT: Duration = Duration::from_secs(3600);

/// A SHA-256 digest in both wire encodings, plus the byte size it describes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContentDigest {
    size: u64,
    hex: String,
    base64: String,
}

impl ContentDigest {
    pub fn from_bytes(bytes: &[u8]) -> Self {
        let digest = Sha256::digest(bytes);
        Self {
            size: bytes.len() as u64,
            hex: to_hex(&digest),
            base64: STANDARD.encode(digest),
        }
    }

    /// Hash a file. Callers should build uploads through this rather than
    /// hashing a copy that may no longer match the file on disk.
    pub async fn from_file(path: impl AsRef<Path>) -> Result<Self, Error> {
        let mut file = tokio::fs::File::open(path.as_ref()).await?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = tokio::io::AsyncReadExt::read(&mut file, &mut buffer).await?;
            if read == 0 {
                break;
            }
            size = size
                .checked_add(read as u64)
                .ok_or_else(|| Error::Protocol("file size overflow".into()))?;
            hash.update(&buffer[..read]);
        }
        let digest = hash.finalize();
        Ok(Self {
            size,
            hex: to_hex(&digest),
            base64: STANDARD.encode(digest),
        })
    }

    /// Build from the lowercase hex form reservations use.
    pub fn from_hex(size: u64, hex: &str) -> Result<Self, Error> {
        if !is_lower_hex(hex) {
            return Err(Error::Protocol(
                "a SHA-256 digest must be 64 lowercase hex characters".into(),
            ));
        }
        let bytes = hex_bytes(hex)?;
        Ok(Self {
            size,
            hex: hex.to_string(),
            base64: STANDARD.encode(bytes),
        })
    }

    /// Build from the canonical Base64 form descriptors carry.
    pub fn from_base64(size: u64, encoded: &str) -> Result<Self, Error> {
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| Error::Protocol("asset checksum is not valid Base64".into()))?;
        if bytes.len() != 32 || STANDARD.encode(&bytes) != encoded {
            return Err(Error::Protocol(
                "asset checksum is not a canonical SHA-256".into(),
            ));
        }
        Ok(Self {
            size,
            hex: to_hex(&bytes),
            base64: encoded.to_string(),
        })
    }

    /// The digest a stored [`AssetDescriptor`] states. The service verified
    /// these bytes, so this is authoritative identity, not a hint.
    pub fn from_descriptor(descriptor: &AssetDescriptor) -> Result<Self, Error> {
        descriptor.validate()?;
        Self::from_base64(descriptor.size, &descriptor.file_checksum)
    }

    pub fn size(&self) -> u64 {
        self.size
    }

    /// Lowercase hex, as reservations and photo fields state it.
    pub fn hex(&self) -> &str {
        &self.hex
    }

    /// Canonical padded Base64, as an asset descriptor states it.
    pub fn base64(&self) -> &str {
        &self.base64
    }
}

/// The two roles bound to one master record. They are never interchangeable.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AssetRole {
    Original,
    Thumbnail,
}

impl AssetRole {
    pub fn field_name(self) -> &'static str {
        match self {
            Self::Original => "resOriginalRes",
            Self::Thumbnail => "resJPEGThumbRes",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Original => "original",
            Self::Thumbnail => "thumbnail",
        }
    }

    fn from_field(field_name: &str) -> Option<Self> {
        match field_name {
            "resOriginalRes" => Some(Self::Original),
            "resJPEGThumbRes" => Some(Self::Thumbnail),
            _ => None,
        }
    }
}

/// Whether a reservation creates the binding or replaces it. Must equal the
/// lease's current phase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum UploadIntent {
    Create,
    Update,
}

impl UploadIntent {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Update => "update",
        }
    }

    pub(crate) fn phase(self) -> crate::lease::UploadPhase {
        match self {
            Self::Create => crate::lease::UploadPhase::Create,
            Self::Update => crate::lease::UploadPhase::Modify,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum SessionState {
    Reserved,
    Receiving,
    PublishPrepared,
    Published,
    Failed,
    Expired,
}

impl SessionState {
    pub fn is_terminal_failure(self) -> bool {
        matches!(self, Self::Failed | Self::Expired)
    }

    pub fn is_published(self) -> bool {
        matches!(self, Self::Published)
    }
}

/// Where the bytes of one upload come from.
#[derive(Clone, Debug)]
pub enum AssetSource {
    Bytes(Vec<u8>),
    File(std::path::PathBuf),
}

/// One resource to upload: its bytes, the digest stated at reservation time and
/// the MIME type the service records.
#[derive(Clone, Debug)]
pub struct AssetUpload {
    pub source: AssetSource,
    pub digest: ContentDigest,
    pub mime_type: String,
    pub width: Option<u32>,
    pub height: Option<u32>,
}

impl AssetUpload {
    /// Bytes already in memory. The digest is computed once here; the same
    /// value is stated at reservation and checked before the upload leaves.
    pub fn from_bytes(bytes: impl Into<Vec<u8>>, mime_type: impl Into<String>) -> Self {
        let bytes = bytes.into();
        let digest = ContentDigest::from_bytes(&bytes);
        Self {
            source: AssetSource::Bytes(bytes),
            digest,
            mime_type: mime_type.into(),
            width: None,
            height: None,
        }
    }

    /// A file on disk. The digest is computed now, so a later modification is
    /// caught locally by the size check and remotely by the service.
    pub async fn from_file(
        path: impl Into<std::path::PathBuf>,
        mime_type: impl Into<String>,
    ) -> Result<Self, Error> {
        let path = path.into();
        let digest = ContentDigest::from_file(&path).await?;
        Ok(Self {
            source: AssetSource::File(path),
            digest,
            mime_type: mime_type.into(),
            width: None,
            height: None,
        })
    }

    pub fn with_dimensions(mut self, width: u32, height: u32) -> Self {
        self.width = Some(width);
        self.height = Some(height);
        self
    }

    pub(crate) fn validate(&self) -> Result<(), Error> {
        if self.mime_type.trim().is_empty() {
            return Err(Error::Protocol("an upload needs a MIME type".into()));
        }
        if self.digest.size() == 0 {
            return Err(Error::Protocol("an asset cannot be empty".into()));
        }
        match &self.source {
            AssetSource::Bytes(bytes) if bytes.len() as u64 != self.digest.size() => Err(
                Error::Protocol("the upload bytes do not match their stated digest size".into()),
            ),
            _ => Ok(()),
        }
    }
}

/// One resource to reserve. Both roles of a photo should be reserved in one
/// request so the pair is bound atomically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReservationRequest {
    pub role: AssetRole,
    pub digest: ContentDigest,
}

impl ReservationRequest {
    pub fn new(role: AssetRole, digest: ContentDigest) -> Self {
        Self { role, digest }
    }
}

/// A granted reservation. [`AssetReservation::url`] is a capability and is
/// never persisted; a published reservation has none because no upload is due.
#[derive(Clone, Debug, PartialEq)]
pub struct AssetReservation {
    pub role: AssetRole,
    pub record_name: String,
    pub field_name: String,
    pub session_id: String,
    pub state: SessionState,
    pub url: Option<String>,
    pub asset: Option<AssetDescriptor>,
}

impl AssetReservation {
    pub fn requires_upload(&self) -> bool {
        !self.state.is_published()
    }

    pub fn descriptor(&self) -> Option<&AssetDescriptor> {
        self.asset.as_ref()
    }
}

#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionInfo {
    #[serde(rename = "sessionId")]
    pub session_id: String,
    pub state: SessionState,
    #[serde(default)]
    pub asset: Option<AssetDescriptor>,
    #[serde(rename = "serverErrorCode", default)]
    pub error_code: Option<String>,
}

/// A freshly signed download capability plus the authoritative descriptor.
#[derive(Clone, Debug, PartialEq)]
pub struct DownloadDescriptor {
    pub descriptor: AssetDescriptor,
    pub download_url: String,
    pub receipt: String,
}

impl DownloadDescriptor {
    pub fn asset_id(&self) -> &str {
        &self.descriptor.asset_id
    }

    pub fn size(&self) -> u64 {
        self.descriptor.size
    }

    pub fn digest(&self) -> Result<ContentDigest, Error> {
        ContentDigest::from_descriptor(&self.descriptor)
    }
}

/// Conditional headers for a resource read. A 206 response continues a ranged
/// transfer; a 200 answer to a ranged request means the asset changed and the
/// caller must restart rather than append.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DownloadOptions {
    /// HTTP `Range`, for example `bytes=1024-`.
    pub range: Option<String>,
    /// HTTP `If-Range`; pass the ETag a resumed transfer started from.
    pub if_range: Option<String>,
    pub if_match: Option<String>,
    pub if_none_match: Option<String>,
}

impl DownloadOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn range(mut self, range: impl Into<String>) -> Self {
        self.range = Some(range.into());
        self
    }

    pub fn if_range(mut self, etag: impl Into<String>) -> Self {
        self.if_range = Some(etag.into());
        self
    }

    pub fn if_match(mut self, etag: impl Into<String>) -> Self {
        self.if_match = Some(etag.into());
        self
    }

    pub fn if_none_match(mut self, etag: impl Into<String>) -> Self {
        self.if_none_match = Some(etag.into());
        self
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetBytes {
    pub status: u16,
    pub bytes: Vec<u8>,
    pub etag: Option<String>,
    pub content_range: Option<String>,
    pub accept_ranges: Option<String>,
}

impl AssetBytes {
    /// True for a 206 response to a ranged request.
    pub fn continues(&self) -> bool {
        self.status == 206
    }

    pub fn not_modified(&self) -> bool {
        self.status == 304
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageUsage {
    pub quota_bytes: u64,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub available_bytes: u64,
    pub image_bytes: u64,
    pub video_bytes: u64,
    pub other_bytes: u64,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReserveBody<'a> {
    #[serde(rename = "zoneID")]
    zone: &'a ZoneId,
    tokens: Vec<ReserveToken<'a>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReserveToken<'a> {
    #[serde(rename = "recordName")]
    record_name: &'a str,
    #[serde(rename = "recordType")]
    record_type: &'static str,
    #[serde(rename = "fieldName")]
    field_name: &'static str,
    #[serde(rename = "_syncserver")]
    options: ReserveOptions<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReserveOptions<'a> {
    role: AssetRole,
    intent: UploadIntent,
    expected_size: u64,
    expected_sha256: &'a str,
}

#[derive(Deserialize)]
struct ReserveResponse {
    #[serde(default)]
    tokens: Vec<ReserveTokenResponse>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ReserveTokenResponse {
    #[serde(rename = "recordName")]
    record_name: String,
    #[serde(rename = "fieldName")]
    field_name: String,
    #[serde(default)]
    url: Option<String>,
    #[serde(rename = "_syncserver", default)]
    options: Option<TokenOptions>,
    #[serde(default)]
    asset: Option<Value>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TokenOptions {
    #[serde(rename = "sessionId")]
    session_id: String,
    state: SessionState,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UploadResponse {
    #[serde(rename = "singleFile")]
    single_file: Value,
}

impl Connection {
    /// Reserve one master's resources in a single request. Both roles of a
    /// photo belong together: reserving one and failing the other would leave
    /// a session with no counterpart, and sessions cannot be resumed.
    pub async fn reserve_assets(
        &self,
        lease: &UploadLease,
        master_record: &str,
        intent: UploadIntent,
        requests: &[ReservationRequest],
    ) -> Result<Vec<AssetReservation>, Error> {
        lease.require_held()?;
        let phase = lease.phase().ok_or_else(|| {
            Error::PhaseMismatch(
                "the progress check has not passed; the lease is still checking".into(),
            )
        })?;
        if phase != intent.phase() {
            return Err(Error::PhaseMismatch(format!(
                "a {:?} reservation requires the {:?} phase, but the lease is in {:?}",
                intent,
                intent.phase(),
                phase
            )));
        }
        self.reserve_assets_with_proof(lease.zone(), lease.proof(), master_record, intent, requests)
            .await
    }

    /// Reserve resources under a lease proof the caller already holds.
    ///
    /// This is for clients that own lease renewal and phase tracking: the
    /// service still enforces both, but the SDK can only enforce the request
    /// shape here. Prefer [`Connection::reserve_assets`] when an
    /// [`UploadLease`] is available.
    pub async fn reserve_assets_with_proof(
        &self,
        zone: &crate::model::ZoneId,
        proof: &LeaseProof,
        master_record: &str,
        intent: UploadIntent,
        requests: &[ReservationRequest],
    ) -> Result<Vec<AssetReservation>, Error> {
        self.check_zone(zone)?;
        if !identifier(master_record) {
            return Err(Error::Protocol("invalid master record name".into()));
        }
        if requests.is_empty() || requests.len() > 2 {
            return Err(Error::Protocol(
                "reserve one or both roles of a photo".into(),
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for request in requests {
            if !seen.insert(request.role) {
                return Err(Error::Protocol("a role may be reserved once".into()));
            }
            if request.digest.size() == 0 {
                return Err(Error::Protocol("an asset cannot be empty".into()));
            }
            let maximum = match request.role {
                AssetRole::Original => self.scope().limits.max_asset_bytes,
                AssetRole::Thumbnail => self.scope().limits.thumbnail_max_bytes,
            };
            if maximum > 0 && request.digest.size() > maximum {
                return Err(Error::Protocol(format!(
                    "a {:?} of {} bytes exceeds the {maximum}-byte limit",
                    request.role,
                    request.digest.size()
                )));
            }
        }
        let tokens: Vec<ReserveToken<'_>> = requests
            .iter()
            .map(|request| ReserveToken {
                record_name: master_record,
                record_type: MASTER_RECORD_TYPE,
                field_name: request.role.field_name(),
                options: ReserveOptions {
                    role: request.role,
                    intent,
                    expected_size: request.digest.size(),
                    // Hex, not Base64: the reservation validates `[0-9a-f]{64}`.
                    expected_sha256: request.digest.hex(),
                },
            })
            .collect();
        let body = ReserveBody { zone, tokens };
        let value = serde_json::to_value(&body)
            .map_err(|error| Error::Protocol(format!("reservation is not encodable: {error}")))?;
        let payload = serde_json::to_vec(&value)
            .map_err(|error| Error::Protocol(format!("reservation is not encodable: {error}")))?;
        let key = reservation_key(&payload, proof);
        let url = self.cloudkit_url("assets/upload")?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &value, Some(proof), Some(&key))
            .await?;
        let response: ReserveResponse = decode(&bytes)?;
        if response.tokens.len() != requests.len() {
            return Err(Error::Protocol(format!(
                "the service granted {} of {} requested resources",
                response.tokens.len(),
                requests.len()
            )));
        }
        let mut granted = Vec::with_capacity(requests.len());
        for token in response.tokens {
            let role = AssetRole::from_field(&token.field_name).ok_or_else(|| {
                Error::Protocol(format!("unknown asset field {:?}", token.field_name))
            })?;
            if token.record_name != master_record {
                return Err(Error::Protocol(
                    "the reservation named a different master record".into(),
                ));
            }
            let options = token.options.ok_or_else(|| {
                Error::Protocol(format!(
                    "reservation for {} carried no session",
                    token.field_name
                ))
            })?;
            let asset = match token.asset {
                Some(value) => Some(parse_descriptor(value)?),
                None => None,
            };
            if options.state.is_published() != asset.is_some() {
                return Err(Error::Protocol(
                    "a published reservation must carry its descriptor".into(),
                ));
            }
            if options.state.is_terminal_failure() {
                return Err(Error::UploadFailed(format!(
                    "the reservation for {:?} was already {:?}",
                    role, options.state
                )));
            }
            granted.push(AssetReservation {
                role,
                record_name: token.record_name,
                field_name: token.field_name,
                session_id: options.session_id,
                state: options.state,
                url: token.url,
                asset,
            });
        }
        let mut ordered = Vec::with_capacity(requests.len());
        for request in requests {
            let found = granted
                .iter()
                .find(|reservation| reservation.role == request.role)
                .ok_or_else(|| {
                    Error::Protocol(format!(
                        "the service did not grant the {:?} resource",
                        request.role
                    ))
                })?;
            ordered.push(found.clone());
        }
        Ok(ordered)
    }

    /// Upload one resource's bytes to its reserved session. The local digest
    /// is checked before the bytes leave, then the service's descriptor is
    /// checked against it. A published reservation returns its descriptor
    /// without sending anything.
    pub async fn upload_asset(
        &self,
        lease: &UploadLease,
        reservation: &AssetReservation,
        digest: &ContentDigest,
        bytes: &[u8],
    ) -> Result<AssetDescriptor, Error> {
        lease.require_held()?;
        self.upload_asset_with_proof(lease.proof(), reservation, digest, bytes)
            .await
    }

    /// Upload bytes under a lease proof the caller already holds. The caller is
    /// responsible for lease liveness; the service verifies the proof.
    pub async fn upload_asset_with_proof(
        &self,
        proof: &LeaseProof,
        reservation: &AssetReservation,
        digest: &ContentDigest,
        bytes: &[u8],
    ) -> Result<AssetDescriptor, Error> {
        let actual = ContentDigest::from_bytes(bytes);
        if actual.size() != digest.size() || actual.hex() != digest.hex() {
            return Err(Error::ChecksumMismatch {
                expected: digest.hex().to_string(),
                actual: actual.hex().to_string(),
            });
        }
        if let Some(descriptor) = self.published(reservation, digest)? {
            return Ok(descriptor);
        }
        let url = reservation_url(reservation)?;
        self.check_origin(&url)?;
        let mut request = self
            .client()
            .http()
            .post(url)
            .bearer_auth(self.client().require_token()?)
            .timeout(TRANSFER_TIMEOUT)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, bytes.len() as u64)
            .body(bytes.to_vec());
        for (name, value) in proof.headers() {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        let uploaded: UploadResponse = decode(&body)?;
        let descriptor = parse_descriptor(uploaded.single_file)?;
        check_uploaded_descriptor(&descriptor, digest)
    }

    /// Stream a file to its reserved session. The file's size must match the
    /// digest declared at reservation; the service verifies the content.
    pub async fn upload_asset_file(
        &self,
        lease: &UploadLease,
        reservation: &AssetReservation,
        digest: &ContentDigest,
        path: impl AsRef<Path>,
    ) -> Result<AssetDescriptor, Error> {
        lease.require_held()?;
        self.upload_asset_file_with_proof(lease.proof(), reservation, digest, path)
            .await
    }

    /// Stream a file under a lease proof the caller already holds. The caller
    /// is responsible for lease liveness; the service verifies the proof.
    pub async fn upload_asset_file_with_proof(
        &self,
        proof: &LeaseProof,
        reservation: &AssetReservation,
        digest: &ContentDigest,
        path: impl AsRef<Path>,
    ) -> Result<AssetDescriptor, Error> {
        if let Some(descriptor) = self.published(reservation, digest)? {
            return Ok(descriptor);
        }
        let url = reservation_url(reservation)?;
        self.check_origin(&url)?;
        let file = tokio::fs::File::open(path.as_ref()).await?;
        let size = file.metadata().await?.len();
        if size != digest.size() {
            return Err(Error::SizeMismatch {
                expected: digest.size(),
                actual: size,
            });
        }
        let stream = tokio_util::io::ReaderStream::with_capacity(file, 64 * 1024);
        let mut request = self
            .client()
            .http()
            .post(url)
            .bearer_auth(self.client().require_token()?)
            .timeout(TRANSFER_TIMEOUT)
            .header(CONTENT_TYPE, "application/octet-stream")
            .header(CONTENT_LENGTH, size)
            .body(reqwest::Body::wrap_stream(stream));
        for (name, value) in proof.headers() {
            request = request.header(name, value);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        let uploaded: UploadResponse = decode(&body)?;
        let descriptor = parse_descriptor(uploaded.single_file)?;
        check_uploaded_descriptor(&descriptor, digest)
    }

    /// What a session turned out to be. Readable without a lease, which is what
    /// makes reconciling an uncertain upload possible.
    pub async fn upload_session(&self, session_id: &str) -> Result<SessionInfo, Error> {
        if !identifier(session_id) {
            return Err(Error::Protocol("invalid session id".into()));
        }
        let body = self
            .client()
            .get_bytes(&self.url(&["v1", "uploads", session_id])?)
            .await?;
        decode(&body)
    }

    /// Extend a `Reserved` session and receive a fresh upload capability.
    pub async fn renew_upload_session(
        &self,
        lease: &UploadLease,
        session_id: &str,
    ) -> Result<AssetReservation, Error> {
        lease.require_held()?;
        self.renew_upload_session_with_proof(lease.proof(), session_id)
            .await
    }

    /// Extend a `Reserved` session under a lease proof the caller already
    /// holds. The caller is responsible for lease liveness.
    pub async fn renew_upload_session_with_proof(
        &self,
        proof: &LeaseProof,
        session_id: &str,
    ) -> Result<AssetReservation, Error> {
        if !identifier(session_id) {
            return Err(Error::Protocol("invalid session id".into()));
        }
        let url = self.url(&["v1", "uploads", session_id, "renew"])?;
        let bytes = self
            .client()
            .post_json_bytes(&url, &json!({}), Some(proof), None)
            .await?;
        let token: ReserveTokenResponse = decode(&bytes)?;
        let role = AssetRole::from_field(&token.field_name).ok_or_else(|| {
            Error::Protocol(format!("unknown asset field {:?}", token.field_name))
        })?;
        let options = token
            .options
            .ok_or_else(|| Error::Protocol("the renewed session carried no state".into()))?;
        Ok(AssetReservation {
            role,
            record_name: token.record_name,
            field_name: token.field_name,
            session_id: options.session_id,
            state: options.state,
            url: token.url,
            asset: token.asset.map(parse_descriptor).transpose()?,
        })
    }

    /// A fresh capability for an existing asset plus its authoritative
    /// descriptor. The role is part of identity: asking for the other role is
    /// a different resource, not a lesser answer.
    pub async fn describe_asset(
        &self,
        asset_id: &str,
        role: AssetRole,
    ) -> Result<DownloadDescriptor, Error> {
        if asset_id.parse::<uuid::Uuid>().is_err() {
            return Err(Error::Protocol("invalid asset id".into()));
        }
        let mut url = self.url(&["v1", "assets", asset_id])?;
        url.query_pairs_mut().append_pair("role", role.as_str());
        let body = self.client().get_bytes(&url).await?;
        parse_download_descriptor(&body)
    }

    /// Read asset bytes. 200 and 206 are data; 304 means the caller's verified
    /// copy is current. Conditional and ranged options are applied verbatim.
    pub async fn download_asset(
        &self,
        descriptor: &DownloadDescriptor,
        options: &DownloadOptions,
    ) -> Result<AssetBytes, Error> {
        let url = Url::parse(&descriptor.download_url)
            .map_err(|error| Error::Protocol(format!("invalid download URL: {error}")))?;
        self.check_origin(&url)?;
        let mut request = self
            .client()
            .http()
            .get(url)
            .bearer_auth(self.client().require_token()?)
            .timeout(TRANSFER_TIMEOUT);
        if let Some(value) = &options.range {
            request = request.header(RANGE, header_value(value)?);
        }
        if let Some(value) = &options.if_range {
            request = request.header(IF_RANGE, header_value(value)?);
        }
        if let Some(value) = &options.if_match {
            request = request.header(IF_MATCH, header_value(value)?);
        }
        if let Some(value) = &options.if_none_match {
            request = request.header(IF_NONE_MATCH, header_value(value)?);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let status = response.status();
        let etag = response
            .headers()
            .get(ETAG)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let content_range = response
            .headers()
            .get(CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let accept_ranges = response
            .headers()
            .get(ACCEPT_RANGES)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        if status == StatusCode::NOT_MODIFIED {
            return Ok(AssetBytes {
                status: 304,
                bytes: Vec::new(),
                etag,
                content_range,
                accept_ranges,
            });
        }
        if status == StatusCode::PRECONDITION_FAILED {
            return Err(precondition(
                412,
                "PRECONDITION_FAILED",
                "If-Match did not match",
            ));
        }
        if status == StatusCode::RANGE_NOT_SATISFIABLE {
            return Err(precondition(
                416,
                "RANGE_NOT_SATISFIABLE",
                "the requested range is outside the asset",
            ));
        }
        let (status, bytes) = read_limited(response, transfer_limit(self)).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &bytes));
        }
        Ok(AssetBytes {
            status: status.as_u16(),
            bytes,
            etag,
            content_range,
            accept_ranges,
        })
    }

    /// Stream a complete resource to a new file and verify its size and
    /// SHA-256 before returning. The path must not already exist. A failed
    /// verification removes the partial file.
    pub async fn download_to_file(
        &self,
        descriptor: &DownloadDescriptor,
        path: impl AsRef<Path>,
    ) -> Result<(), Error> {
        let expected = descriptor.digest()?;
        let url = Url::parse(&descriptor.download_url)
            .map_err(|error| Error::Protocol(format!("invalid download URL: {error}")))?;
        self.check_origin(&url)?;
        let response = self
            .client()
            .http()
            .get(url)
            .bearer_auth(self.client().require_token()?)
            .timeout(TRANSFER_TIMEOUT)
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let status = response.status();
        if status != StatusCode::OK {
            let (_, body) = read_limited(response, MAX_JSON_BYTES).await?;
            return Err(map_http(status.as_u16(), &body));
        }
        let path = path.as_ref().to_path_buf();
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut response = response;
        let outcome = async {
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|error| Error::Transport(error.to_string()))?
            {
                size = size
                    .checked_add(chunk.len() as u64)
                    .ok_or_else(|| Error::Protocol("download size overflow".into()))?;
                if size > expected.size() {
                    return Err(Error::SizeMismatch {
                        expected: expected.size(),
                        actual: size,
                    });
                }
                hash.update(&chunk);
                file.write_all(&chunk).await?;
            }
            if size != expected.size() {
                return Err(Error::SizeMismatch {
                    expected: expected.size(),
                    actual: size,
                });
            }
            let actual = to_hex(&hash.finalize());
            if actual != expected.hex() {
                return Err(Error::ChecksumMismatch {
                    expected: expected.hex().to_string(),
                    actual,
                });
            }
            file.sync_all().await.map_err(Error::from)
        }
        .await;
        if outcome.is_err() {
            drop(file);
            let _ = tokio::fs::remove_file(&path).await;
        }
        outcome
    }

    /// Account storage usage for the authenticated user.
    pub async fn storage_usage(&self) -> Result<StorageUsage, Error> {
        let body = self
            .client()
            .get_bytes(&self.url(&["v1", "storage"])?)
            .await?;
        decode(&body)
    }

    fn published(
        &self,
        reservation: &AssetReservation,
        digest: &ContentDigest,
    ) -> Result<Option<AssetDescriptor>, Error> {
        if !reservation.state.is_published() {
            return Ok(None);
        }
        let descriptor = reservation.descriptor().cloned().ok_or_else(|| {
            Error::UploadFailed("a published reservation carried no descriptor".into())
        })?;
        Ok(Some(check_uploaded_descriptor(&descriptor, digest)?))
    }
}

fn reservation_key(payload: &[u8], proof: &LeaseProof) -> String {
    let mut hash = Sha256::new();
    hash.update(payload);
    hash.update(proof.lease_id.as_bytes());
    hash.update(proof.fencing_token.as_bytes());
    format!("reserve-{:x}", hash.finalize())
}

fn reservation_url(reservation: &AssetReservation) -> Result<Url, Error> {
    let raw = reservation.url.as_deref().ok_or_else(|| {
        Error::UploadFailed(format!(
            "session {} has no upload capability",
            reservation.session_id
        ))
    })?;
    Url::parse(raw).map_err(|error| Error::Protocol(format!("invalid upload URL: {error}")))
}

fn parse_descriptor(value: Value) -> Result<AssetDescriptor, Error> {
    let descriptor: AssetDescriptor = serde_json::from_value(value)
        .map_err(|error| Error::Protocol(format!("invalid asset descriptor: {error}")))?;
    descriptor.validate()?;
    if descriptor.mime_type.is_empty() {
        return Err(Error::Protocol("asset descriptor has no MIME type".into()));
    }
    Ok(descriptor)
}

fn check_uploaded_descriptor(
    descriptor: &AssetDescriptor,
    digest: &ContentDigest,
) -> Result<AssetDescriptor, Error> {
    if descriptor.size != digest.size() {
        return Err(Error::SizeMismatch {
            expected: digest.size(),
            actual: descriptor.size,
        });
    }
    if descriptor.file_checksum != digest.base64() {
        return Err(Error::ChecksumMismatch {
            expected: digest.base64().to_string(),
            actual: descriptor.file_checksum.clone(),
        });
    }
    if descriptor.receipt.is_none() {
        return Err(Error::Protocol(
            "uploaded asset descriptor has no receipt".into(),
        ));
    }
    Ok(descriptor.clone())
}

fn parse_download_descriptor(body: &[u8]) -> Result<DownloadDescriptor, Error> {
    let value: Value = decode(body)?;
    let descriptor = parse_descriptor(value)?;
    let download_url = descriptor
        .download_url
        .clone()
        .ok_or_else(|| Error::Protocol("asset descriptor has no download URL".into()))?;
    let receipt = descriptor
        .receipt
        .clone()
        .ok_or_else(|| Error::Protocol("asset descriptor has no receipt".into()))?;
    Url::parse(&download_url)
        .map_err(|error| Error::Protocol(format!("invalid download URL: {error}")))?;
    Ok(DownloadDescriptor {
        descriptor,
        download_url,
        receipt,
    })
}

fn header_value(value: &str) -> Result<HeaderValue, Error> {
    HeaderValue::from_str(value)
        .map_err(|_| Error::Protocol(format!("invalid HTTP header value {value:?}")))
}

fn transfer_limit(connection: &Connection) -> usize {
    let announced = connection.scope().limits.max_asset_bytes;
    let limit = if announced == 0 {
        8 * 1024 * 1024 * 1024
    } else {
        announced
    };
    usize::try_from(limit).unwrap_or(usize::MAX)
}

fn precondition(status: u16, code: &str, reason: &str) -> Error {
    Error::Api {
        status,
        failure: Box::new(crate::ServerFailure {
            code: code.into(),
            reason: reason.into(),
            record_name: None,
            zone: None,
            server_record: None,
        }),
    }
}

fn is_lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn hex_bytes(value: &str) -> Result<Vec<u8>, Error> {
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| Error::Protocol("invalid hex digest".into()))
        })
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write;
        let _ = write!(out, "{byte:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digests_agree_in_both_encodings() {
        let bytes = b"hello sync";
        let digest = ContentDigest::from_bytes(bytes);
        assert_eq!(digest.size(), bytes.len() as u64);
        assert_eq!(digest.hex().len(), 64);
        assert_eq!(digest.base64().len(), 44);
        assert_eq!(
            ContentDigest::from_hex(digest.size(), digest.hex()).unwrap(),
            digest
        );
        assert_eq!(
            ContentDigest::from_base64(digest.size(), digest.base64()).unwrap(),
            digest
        );
        assert!(ContentDigest::from_hex(digest.size(), &digest.hex().to_uppercase()).is_err());
        assert!(ContentDigest::from_base64(digest.size(), "AAAA").is_err());
    }

    #[test]
    fn role_fields_are_the_ones_the_service_binds() {
        assert_eq!(AssetRole::Original.field_name(), "resOriginalRes");
        assert_eq!(AssetRole::Thumbnail.field_name(), "resJPEGThumbRes");
        assert_eq!(
            AssetRole::from_field("resJPEGThumbRes"),
            Some(AssetRole::Thumbnail)
        );
        assert_eq!(AssetRole::from_field("other"), None);
    }
}
