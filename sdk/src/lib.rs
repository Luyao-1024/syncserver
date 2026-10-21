//! A client SDK for the `syncserver` photo synchronization service.
//!
//! The crate speaks the service's native protocol: account login, capability
//! probing, snapshot and incremental metadata reads, conditional record
//! mutations, the exclusive upload lease, asset transfer and photo publication.
//! It deliberately has no storage or scheduling of its own — persistence and
//! the shape of a synchronization cycle belong to the application.
//!
//! # Layers
//!
//! * [`Endpoint`] validates the configured origin; [`Client`] owns transport and
//!   the access token; [`Connection`] is a probe-validated session whose scope
//!   cannot silently change.
//! * [`metadata`] exposes snapshots, pages, lookups, changes and mutations as
//!   typed values.
//! * [`lease`] wraps the exclusive upload lease with a conservative deadline and
//!   automatic renewal. [`assets`] covers reservation, upload, download and
//!   storage usage. [`photo`] publishes, updates and deletes photo pairs.
//!
//! # Example
//!
//! ```no_run
//! use sync_client::Client;
//!
//! # async fn run() -> Result<(), sync_client::Error> {
//! let client = Client::builder("https://sync.example.com")?
//!     .access_token("session-or-api-token")
//!     .build()?;
//! client.ready().await?;
//! let connection = client.connect().await?;
//! let zone = connection.primary_zone()?;
//!
//! let graph = connection.bootstrap(&zone).await?;
//! for photo in &graph.photos {
//!     println!("{} {}", photo.photo_id, photo.relative_path);
//! }
//! # Ok(())
//! # }
//! ```
//!
//! Without a pre-existing token, call [`Client::login`] first. The token is kept
//! in memory only; persisting a session is the application's decision.

pub mod assets;
pub mod client;
pub mod endpoint;
pub mod error;
pub mod lease;
pub mod metadata;
pub mod model;
pub mod photo;
pub mod scope;

pub use assets::{
    AssetBytes, AssetReservation, AssetRole, AssetSource, AssetUpload, ContentDigest,
    DownloadDescriptor, DownloadOptions, ReservationRequest, SessionInfo, SessionState,
    StorageUsage, UploadIntent,
};
pub use client::{Client, ClientBuilder, Connection, Session};
pub use endpoint::Endpoint;
pub use error::{Error, ServerFailure};
pub use lease::{
    LeaseCheck, LeaseGrant, LeaseProof, LeaseRenewal, UploadLease, UploadPhase, ZoneSyncToken,
};
pub use metadata::{
    Bootstrap, ChangesPage, DatabaseChanges, Lookup, OperationState, QueryAll, QueryRequest,
    RecordPage, Snapshot, ZoneChange, ZoneChangeOutcome, ZoneChangesRequest,
};
pub use model::{
    ASSET_RECORD_TYPE, AssetDescriptor, EditEvent, EditEvents, Field, MASTER_RECORD_TYPE,
    ModifyRequest, OperationType, PRIMARY_ZONE, RecordOperation, RecordResult, RemoteRecord,
    ZoneId,
};
pub use photo::{
    GraphError, PhotoContentUpdate, PhotoGraph, PhotoMetadata, PhotoUpload, RemotePhoto,
    create_operation_id, delete_operation_id, update_content_operation_id,
    update_metadata_operation_id,
};
pub use scope::{
    Authentication, Capabilities, CapabilityBits, Limits, Namespace, PinnedScope, RecordLimits,
    SessionUser, SyncLimits,
};
