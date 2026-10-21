//! The photo pair: one `CPLMaster` and one `CPLAsset` that describe the same
//! picture.
//!
//! A photo is never half a pair. Creation writes both records in one atomic
//! mutation; content replacement updates both against the revisions they were
//! read at; deletion removes both. [`PhotoGraph`] rebuilds the pair from plain
//! records and rejects a graph that breaks any of these invariants:
//!
//! * `CPLMaster.photoId` names the paired `CPLAsset.recordName`.
//! * `CPLAsset.masterRef` names a master in the same zone.
//! * Both records carry the same non-nil UUID `contentGeneration`.
//! * `relativePath` is a safe, unique, zone-relative placement.
//! * `thumbnailSourceOriginalSHA256` states the original's digest.
//!
//! Every mutation uses a deterministic idempotency key derived from the logical
//! change, exposed as the `*_operation_id` functions. After an uncertain
//! failure, pass that id to [`Connection::operation_status`] instead of blindly
//! writing again.

use crate::Error;
use crate::assets::{
    AssetReservation, AssetSource, AssetUpload, ContentDigest, ReservationRequest, UploadIntent,
};
use crate::client::Connection;
use crate::lease::{UploadLease, UploadPhase};
use crate::model::{
    ASSET_RECORD_TYPE, AssetDescriptor, EditEvents, Field, MASTER_RECORD_TYPE, ModifyRequest,
    RecordOperation, RecordResult, RemoteRecord, identifier,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
use uuid::Uuid;

/// Why a set of records is not a usable photo graph. Each variant names a
/// concrete, user-visible inconsistency rather than "invalid data".
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum GraphError {
    #[error("record {name:?} has no change tag")]
    MissingRevision { name: String },
    #[error("record {name:?} appears more than once")]
    DuplicateRecord { name: String },
    #[error("photo {name:?} references master {master:?}, which this zone does not contain")]
    DanglingMaster { name: String, master: String },
    #[error("master {master:?} is not referenced by any photo")]
    UnreferencedMaster { master: String },
    #[error("master {master:?} names photo {claimed:?} but is referenced as {actual:?}")]
    MasterPhotoIdMismatch {
        master: String,
        claimed: String,
        actual: String,
    },
    #[error("photo {name:?} and master {master:?} disagree on content generation")]
    GenerationMismatch { name: String, master: String },
    #[error("photo {name:?} is missing its {field} asset")]
    MissingAsset { name: String, field: &'static str },
    #[error("photo {name:?} has an invalid relative path")]
    InvalidPath { name: String },
    #[error("photo {name:?} shares relative path {path:?} with another photo in this zone")]
    DuplicatePath { name: String, path: String },
    #[error("photo {name:?} declares a thumbnail digest that is not the original's SHA-256")]
    InvalidThumbnailDigest { name: String },
}

/// One validated photo. The two [`RemoteRecord`]s are kept so a later
/// conditional update carries exactly the revisions this value was built from.
///
/// `original.download_url` and `original.receipt` are temporary transport
/// credentials. Persist identity (asset id, size, checksum), never those fields;
/// call [`Connection::describe_asset`] for a fresh capability.
#[derive(Clone, Debug, PartialEq)]
pub struct RemotePhoto {
    /// The `CPLAsset.recordName`; stable cloud identity, never a path.
    pub photo_id: String,
    /// The paired `CPLMaster.recordName`.
    pub master_id: String,
    pub content_generation: String,
    /// A `/`-separated path relative to the photo library root.
    pub relative_path: String,
    pub is_favorite: bool,
    /// Milliseconds since the epoch, exactly as the wire states it.
    pub asset_date_ms: Option<i64>,
    /// Preserved but never interpreted as a trash or visibility decision here.
    pub is_hidden: bool,
    pub original: AssetDescriptor,
    pub thumbnail: AssetDescriptor,
    /// Lowercase hex SHA-256 of the original, as declared by the master.
    pub thumbnail_source_sha256: String,
    pub events: EditEvents,
    pub asset_revision: String,
    pub master_revision: String,
    pub asset: RemoteRecord,
    pub master: RemoteRecord,
}

impl RemotePhoto {
    pub fn metadata(&self) -> PhotoMetadata {
        PhotoMetadata {
            relative_path: self.relative_path.clone(),
            is_favorite: self.is_favorite,
            asset_date_ms: self.asset_date_ms,
            is_hidden: self.is_hidden,
        }
    }

    pub fn original_digest(&self) -> Result<ContentDigest, Error> {
        ContentDigest::from_descriptor(&self.original)
    }

    pub fn thumbnail_digest(&self) -> Result<ContentDigest, Error> {
        ContentDigest::from_descriptor(&self.thumbnail)
    }
}

/// Every live photo in one set of records, in stable `photo_id` order.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct PhotoGraph {
    pub photos: Vec<RemotePhoto>,
}

impl PhotoGraph {
    /// Build a graph from any mix of `CPLMaster`/`CPLAsset` records. Records of
    /// other types and tombstones are ignored; a half pair is never ignored.
    pub fn from_records<'a>(
        records: impl IntoIterator<Item = &'a RemoteRecord>,
    ) -> Result<Self, GraphError> {
        let mut masters: BTreeMap<&'a str, &'a RemoteRecord> = BTreeMap::new();
        let mut assets: BTreeMap<&'a str, &'a RemoteRecord> = BTreeMap::new();
        for record in records {
            if record.deleted {
                continue;
            }
            let target = match record.record_type.as_deref() {
                Some(MASTER_RECORD_TYPE) => &mut masters,
                Some(ASSET_RECORD_TYPE) => &mut assets,
                _ => continue,
            };
            if record.revision().is_none() {
                return Err(GraphError::MissingRevision {
                    name: record.record_name.clone(),
                });
            }
            if target.insert(&record.record_name, record).is_some() {
                return Err(GraphError::DuplicateRecord {
                    name: record.record_name.clone(),
                });
            }
        }

        let mut photos = Vec::with_capacity(assets.len());
        let mut referenced = BTreeSet::new();
        let mut paths = HashSet::new();
        for (photo_id, asset) in &assets {
            let master_name =
                asset
                    .reference_name("masterRef")
                    .ok_or_else(|| GraphError::DanglingMaster {
                        name: (*photo_id).to_string(),
                        master: String::from("<missing masterRef>"),
                    })?;
            let master = masters
                .get(master_name)
                .ok_or_else(|| GraphError::DanglingMaster {
                    name: (*photo_id).to_string(),
                    master: master_name.to_string(),
                })?;
            if let (Some(asset_zone), Some(master_zone)) = (&asset.zone_id, &master.zone_id)
                && asset_zone != master_zone
            {
                // The name may exist elsewhere, which is still unusable here.
                return Err(GraphError::DanglingMaster {
                    name: (*photo_id).to_string(),
                    master: master_name.to_string(),
                });
            }

            match (master.string("photoId"), asset.record_name.as_str()) {
                (Some(claimed), actual) if claimed == actual => {}
                (claimed, actual) => {
                    return Err(GraphError::MasterPhotoIdMismatch {
                        master: master.record_name.clone(),
                        claimed: claimed.unwrap_or("<missing photoId>").to_string(),
                        actual: actual.to_string(),
                    });
                }
            }

            let generation = match (
                master.string("contentGeneration"),
                asset.string("contentGeneration"),
            ) {
                (Some(master_generation), Some(asset_generation))
                    if master_generation == asset_generation =>
                {
                    master_generation.to_string()
                }
                _ => {
                    return Err(GraphError::GenerationMismatch {
                        name: (*photo_id).to_string(),
                        master: master.record_name.clone(),
                    });
                }
            };

            let photo = build_photo(asset, master, generation)?;
            if !paths.insert(photo.relative_path.clone()) {
                return Err(GraphError::DuplicatePath {
                    name: photo.photo_id.clone(),
                    path: photo.relative_path.clone(),
                });
            }
            referenced.insert(master.record_name.as_str());
            photos.push(photo);
        }

        for master in masters.keys() {
            if !referenced.contains(*master) {
                return Err(GraphError::UnreferencedMaster {
                    master: (*master).to_string(),
                });
            }
        }
        Ok(Self { photos })
    }

    pub fn photo(&self, photo_id: &str) -> Option<&RemotePhoto> {
        self.photos.iter().find(|photo| photo.photo_id == photo_id)
    }

    pub fn by_path(&self, relative_path: &str) -> Option<&RemotePhoto> {
        self.photos
            .iter()
            .find(|photo| photo.relative_path == relative_path)
    }
}

fn build_photo(
    asset: &RemoteRecord,
    master: &RemoteRecord,
    content_generation: String,
) -> Result<RemotePhoto, GraphError> {
    let photo_id = asset.record_name.as_str();
    let original = descriptor_field(master, "resOriginalRes", photo_id)?;
    let thumbnail = descriptor_field(master, "resJPEGThumbRes", photo_id)?;

    let relative_path = asset
        .string("relativePath")
        .ok_or_else(|| GraphError::MissingAsset {
            name: photo_id.to_string(),
            field: "relativePath",
        })?;
    if !is_safe_relative_path(relative_path) {
        return Err(GraphError::InvalidPath {
            name: photo_id.to_string(),
        });
    }

    let thumbnail_source_sha256 =
        master
            .string("thumbnailSourceOriginalSHA256")
            .ok_or(GraphError::MissingAsset {
                name: photo_id.to_string(),
                field: "thumbnailSourceOriginalSHA256",
            })?;
    let original_hex = original
        .sha256_hex()
        .map_err(|_| GraphError::MissingAsset {
            name: photo_id.to_string(),
            field: "resOriginalRes",
        })?;
    if !is_lower_hex(thumbnail_source_sha256) || original_hex != thumbnail_source_sha256 {
        return Err(GraphError::InvalidThumbnailDigest {
            name: photo_id.to_string(),
        });
    }

    let asset_revision = asset
        .revision()
        .ok_or_else(|| GraphError::MissingRevision {
            name: photo_id.to_string(),
        })?
        .to_string();
    let master_revision = master
        .revision()
        .ok_or_else(|| GraphError::MissingRevision {
            name: master.record_name.clone(),
        })?
        .to_string();

    Ok(RemotePhoto {
        photo_id: photo_id.to_string(),
        master_id: master.record_name.clone(),
        content_generation,
        relative_path: relative_path.to_string(),
        is_favorite: asset.int64("isFavorite").unwrap_or(0) != 0,
        asset_date_ms: asset.int64("assetDate"),
        is_hidden: asset.int64("isHidden").unwrap_or(0) != 0,
        original,
        thumbnail,
        thumbnail_source_sha256: thumbnail_source_sha256.to_string(),
        events: asset.events.clone(),
        asset_revision,
        master_revision,
        asset: asset.clone(),
        master: master.clone(),
    })
}

fn descriptor_field(
    record: &RemoteRecord,
    name: &'static str,
    photo_id: &str,
) -> Result<AssetDescriptor, GraphError> {
    let missing = || GraphError::MissingAsset {
        name: photo_id.to_string(),
        field: name,
    };
    let field = record.field(name).ok_or_else(missing)?;
    if field.kind != "ASSETID" {
        return Err(missing());
    }
    let descriptor = field.as_asset().ok_or_else(missing)?;
    descriptor.validate().map_err(|_| missing())?;
    Ok(descriptor)
}

/// The metadata that can change without touching the stored bytes.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct PhotoMetadata {
    pub relative_path: String,
    pub is_favorite: bool,
    pub asset_date_ms: Option<i64>,
    pub is_hidden: bool,
}

impl PhotoMetadata {
    pub fn new(relative_path: impl Into<String>) -> Self {
        Self {
            relative_path: relative_path.into(),
            is_favorite: false,
            asset_date_ms: None,
            is_hidden: false,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if !is_safe_relative_path(&self.relative_path) {
            return Err(Error::Protocol(format!(
                "invalid relative path {:?}",
                self.relative_path
            )));
        }
        Ok(())
    }
}

/// A photo to publish. Reuse the same value when retrying: the ids and digests
/// determine the operation key and the reserved bytes.
#[derive(Clone, Debug)]
pub struct PhotoUpload {
    pub photo_id: String,
    pub master_id: String,
    pub content_generation: String,
    pub metadata: PhotoMetadata,
    pub original: AssetUpload,
    pub thumbnail: AssetUpload,
}

impl PhotoUpload {
    /// Fresh cloud identities and a fresh content generation.
    pub fn new(metadata: PhotoMetadata, original: AssetUpload, thumbnail: AssetUpload) -> Self {
        Self {
            photo_id: Uuid::new_v4().to_string(),
            master_id: Uuid::new_v4().to_string(),
            content_generation: Uuid::new_v4().to_string(),
            metadata,
            original,
            thumbnail,
        }
    }

    /// Preserve identities chosen by an importing client. The generation must
    /// still be a fresh, non-nil UUID for this version of the content.
    pub fn with_ids(
        photo_id: impl Into<String>,
        master_id: impl Into<String>,
        content_generation: impl Into<String>,
        metadata: PhotoMetadata,
        original: AssetUpload,
        thumbnail: AssetUpload,
    ) -> Self {
        Self {
            photo_id: photo_id.into(),
            master_id: master_id.into(),
            content_generation: content_generation.into(),
            metadata,
            original,
            thumbnail,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        if !identifier(&self.photo_id) || !identifier(&self.master_id) {
            return Err(Error::Protocol(
                "invalid photo or master record name".into(),
            ));
        }
        if self.photo_id == self.master_id {
            return Err(Error::Protocol(
                "a photo and its master need distinct record names".into(),
            ));
        }
        validate_generation(&self.content_generation)?;
        self.metadata.validate()?;
        self.original.validate()?;
        self.thumbnail.validate()?;
        Ok(())
    }
}

/// Replacement bytes for an existing photo. Both roles are replaced together:
/// a photo whose original and thumbnail describe different versions is not a
/// photo.
#[derive(Clone, Debug)]
pub struct PhotoContentUpdate {
    pub content_generation: String,
    pub original: AssetUpload,
    pub thumbnail: AssetUpload,
}

impl PhotoContentUpdate {
    pub fn new(original: AssetUpload, thumbnail: AssetUpload) -> Self {
        Self {
            content_generation: Uuid::new_v4().to_string(),
            original,
            thumbnail,
        }
    }

    pub fn with_generation(
        content_generation: impl Into<String>,
        original: AssetUpload,
        thumbnail: AssetUpload,
    ) -> Self {
        Self {
            content_generation: content_generation.into(),
            original,
            thumbnail,
        }
    }

    fn validate(&self) -> Result<(), Error> {
        validate_generation(&self.content_generation)?;
        self.original.validate()?;
        self.thumbnail.validate()?;
        Ok(())
    }
}

/// The idempotency key of a create issued through
/// [`Connection::create_photo`].
pub fn create_operation_id(photo_id: &str, content_generation: &str) -> String {
    operation_id(
        "photo-create",
        &[photo_id.as_bytes(), content_generation.as_bytes()],
    )
}

/// The idempotency key of a content replacement.
pub fn update_content_operation_id(photo_id: &str, update: &PhotoContentUpdate) -> String {
    operation_id(
        "photo-content",
        &[
            photo_id.as_bytes(),
            update.content_generation.as_bytes(),
            update.original.digest.hex().as_bytes(),
            update.thumbnail.digest.hex().as_bytes(),
        ],
    )
}

/// The idempotency key of a metadata update.
pub fn update_metadata_operation_id(photo_id: &str, metadata: &PhotoMetadata) -> String {
    let favorite = [u8::from(metadata.is_favorite)];
    let hidden = [u8::from(metadata.is_hidden)];
    let date = metadata.asset_date_ms.map(i64::to_be_bytes);
    operation_id(
        "photo-metadata",
        &[
            photo_id.as_bytes(),
            metadata.relative_path.as_bytes(),
            &favorite,
            &hidden,
            date.as_ref().map_or(&[], |value| value.as_slice()),
        ],
    )
}

/// The idempotency key of a deletion issued from this observed version.
pub fn delete_operation_id(photo: &RemotePhoto) -> String {
    operation_id(
        "photo-delete",
        &[
            photo.photo_id.as_bytes(),
            photo.asset_revision.as_bytes(),
            photo.master_revision.as_bytes(),
        ],
    )
}

fn operation_id(prefix: &str, parts: &[&[u8]]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part);
    }
    format!("{prefix}-{:x}", hash.finalize())
}

fn validate_generation(generation: &str) -> Result<(), Error> {
    let uuid = Uuid::parse_str(generation)
        .map_err(|_| Error::Protocol("content generation must be a UUID".into()))?;
    if uuid.is_nil() {
        return Err(Error::Protocol("content generation cannot be nil".into()));
    }
    Ok(())
}

impl Connection {
    /// Publish a new photo pair under a lease in the `create` phase.
    ///
    /// Both resources are reserved in one request, uploaded original first,
    /// then bound to a master and an asset in one atomic mutation. A failure
    /// after the mutation was sent leaves the deterministic
    /// [`create_operation_id`] available for reconciliation.
    pub async fn create_photo(
        &self,
        lease: &UploadLease,
        upload: PhotoUpload,
    ) -> Result<RemotePhoto, Error> {
        require_phase(lease, UploadPhase::Create)?;
        upload.validate()?;

        let requests = [
            ReservationRequest::new(
                crate::assets::AssetRole::Original,
                upload.original.digest.clone(),
            ),
            ReservationRequest::new(
                crate::assets::AssetRole::Thumbnail,
                upload.thumbnail.digest.clone(),
            ),
        ];
        let reservations = self
            .reserve_assets(lease, &upload.master_id, UploadIntent::Create, &requests)
            .await?;
        let original = self
            .put_asset(lease, &reservations[0], &upload.original)
            .await?;
        let thumbnail = self
            .put_asset(lease, &reservations[1], &upload.thumbnail)
            .await?;

        let mut master_fields = BTreeMap::new();
        master_fields.insert(
            "contentGeneration".to_string(),
            Field::string(upload.content_generation.clone()),
        );
        master_fields.insert(
            "thumbnailSourceOriginalSHA256".to_string(),
            Field::string(upload.original.digest.hex().to_string()),
        );
        master_fields.insert("resOriginalRes".to_string(), Field::asset(&original)?);
        master_fields.insert("resJPEGThumbRes".to_string(), Field::asset(&thumbnail)?);
        master_fields.insert(
            "photoId".to_string(),
            Field::string(upload.photo_id.clone()),
        );
        let master =
            RecordOperation::create(upload.master_id.clone(), MASTER_RECORD_TYPE, master_fields);

        let asset = RecordOperation::create(
            upload.photo_id.clone(),
            ASSET_RECORD_TYPE,
            asset_metadata_fields(
                &upload.master_id,
                &upload.content_generation,
                &upload.metadata,
            ),
        );

        let request =
            ModifyRequest::new(lease.zone().clone(), vec![master, asset]).idempotency_key(
                create_operation_id(&upload.photo_id, &upload.content_generation),
            );
        let results = self.modify(lease.proof(), request).await?;
        let mut records = expect_records(results, 2)?;
        let master_record = take_named(&mut records, &upload.master_id)?;
        let asset_record = take_named(&mut records, &upload.photo_id)?;
        photo_from_pair(asset_record, master_record)
    }

    /// Replace a photo's bytes under a lease in the `modify` phase. The
    /// generation, both descriptors and both records move together.
    pub async fn update_photo_content(
        &self,
        lease: &UploadLease,
        photo: &RemotePhoto,
        update: PhotoContentUpdate,
    ) -> Result<RemotePhoto, Error> {
        require_phase(lease, UploadPhase::Modify)?;
        update.validate()?;
        check_photo_zone(lease, photo)?;
        if update.content_generation == photo.content_generation {
            return Err(Error::Protocol(
                "replacing content requires a fresh content generation".into(),
            ));
        }

        let requests = [
            ReservationRequest::new(
                crate::assets::AssetRole::Original,
                update.original.digest.clone(),
            ),
            ReservationRequest::new(
                crate::assets::AssetRole::Thumbnail,
                update.thumbnail.digest.clone(),
            ),
        ];
        let reservations = self
            .reserve_assets(lease, &photo.master_id, UploadIntent::Update, &requests)
            .await?;
        let original = self
            .put_asset(lease, &reservations[0], &update.original)
            .await?;
        let thumbnail = self
            .put_asset(lease, &reservations[1], &update.thumbnail)
            .await?;

        let mut master_fields = BTreeMap::new();
        master_fields.insert(
            "contentGeneration".to_string(),
            Field::string(update.content_generation.clone()),
        );
        master_fields.insert(
            "thumbnailSourceOriginalSHA256".to_string(),
            Field::string(update.original.digest.hex().to_string()),
        );
        master_fields.insert("resOriginalRes".to_string(), Field::asset(&original)?);
        master_fields.insert("resJPEGThumbRes".to_string(), Field::asset(&thumbnail)?);
        let master = RecordOperation::update(
            photo.master_id.clone(),
            photo.master_revision.clone(),
            master_fields,
        );
        let asset = RecordOperation::update(
            photo.photo_id.clone(),
            photo.asset_revision.clone(),
            [(
                "contentGeneration".to_string(),
                Field::string(update.content_generation.clone()),
            )],
        );

        let request = ModifyRequest::new(lease.zone().clone(), vec![master, asset])
            .idempotency_key(update_content_operation_id(&photo.photo_id, &update));
        let results = self.modify(lease.proof(), request).await?;
        let mut records = expect_records(results, 2)?;
        let master_record =
            merge_record(&photo.master, take_named(&mut records, &photo.master_id)?)?;
        let asset_record = merge_record(&photo.asset, take_named(&mut records, &photo.photo_id)?)?;
        photo_from_pair(asset_record, master_record)
    }

    /// Change only a photo's metadata under a lease in the `modify` phase.
    pub async fn update_photo_metadata(
        &self,
        lease: &UploadLease,
        photo: &RemotePhoto,
        metadata: PhotoMetadata,
    ) -> Result<RemotePhoto, Error> {
        self.update_photo_metadata_with_events(lease, photo, metadata, EditEvents::default())
            .await
    }

    /// Metadata update carrying edit events. Retries must reuse the same
    /// events: the server compares the whole logical request under the key.
    pub async fn update_photo_metadata_with_events(
        &self,
        lease: &UploadLease,
        photo: &RemotePhoto,
        metadata: PhotoMetadata,
        events: EditEvents,
    ) -> Result<RemotePhoto, Error> {
        require_phase(lease, UploadPhase::Modify)?;
        metadata.validate()?;
        check_photo_zone(lease, photo)?;

        let operation = RecordOperation::update_fields(
            photo.photo_id.clone(),
            photo.asset_revision.clone(),
            metadata_fields(&metadata),
        )
        .with_events(events);
        let request = ModifyRequest::new(lease.zone().clone(), vec![operation])
            .idempotency_key(update_metadata_operation_id(&photo.photo_id, &metadata));
        let results = self.modify(lease.proof(), request).await?;
        let mut records = expect_records(results, 1)?;
        let asset_record = merge_record(&photo.asset, take_named(&mut records, &photo.photo_id)?)?;
        photo_from_pair(asset_record, photo.master.clone())
    }

    /// Delete both records under a lease in the `delete` phase. The revisions
    /// this value was built from make the delete conditional: a photo edited
    /// or removed elsewhere is a conflict, not a silent overwrite.
    pub async fn delete_photo(
        &self,
        lease: &UploadLease,
        photo: &RemotePhoto,
    ) -> Result<(), Error> {
        self.delete_photo_with_events(lease, photo, EditEvents::default())
            .await
    }

    pub async fn delete_photo_with_events(
        &self,
        lease: &UploadLease,
        photo: &RemotePhoto,
        events: EditEvents,
    ) -> Result<(), Error> {
        require_phase(lease, UploadPhase::Delete)?;
        check_photo_zone(lease, photo)?;

        let asset = RecordOperation::delete(photo.photo_id.clone(), photo.asset_revision.clone())
            .with_events(events.clone());
        let master =
            RecordOperation::delete(photo.master_id.clone(), photo.master_revision.clone())
                .with_events(events);
        let request = ModifyRequest::new(lease.zone().clone(), vec![asset, master])
            .idempotency_key(delete_operation_id(photo));
        let results = self.modify(lease.proof(), request).await?;
        let mut records = expect_records(results, 2)?;
        take_named(&mut records, &photo.photo_id)?;
        take_named(&mut records, &photo.master_id)?;
        Ok(())
    }

    async fn put_asset(
        &self,
        lease: &UploadLease,
        reservation: &AssetReservation,
        upload: &AssetUpload,
    ) -> Result<AssetDescriptor, Error> {
        match &upload.source {
            AssetSource::Bytes(bytes) => {
                self.upload_asset(lease, reservation, &upload.digest, bytes)
                    .await
            }
            AssetSource::File(path) => {
                self.upload_asset_file(lease, reservation, &upload.digest, path)
                    .await
            }
        }
    }
}

fn require_phase(lease: &UploadLease, expected: UploadPhase) -> Result<(), Error> {
    lease.require_held()?;
    match lease.phase() {
        Some(phase) if phase == expected => Ok(()),
        other => Err(Error::PhaseMismatch(format!(
            "this call requires the {:?} phase, but the lease is in {:?}",
            expected, other
        ))),
    }
}

fn check_photo_zone(lease: &UploadLease, photo: &RemotePhoto) -> Result<(), Error> {
    for (record, label) in [(&photo.asset, "photo"), (&photo.master, "master")] {
        if let Some(zone) = &record.zone_id
            && zone != lease.zone()
        {
            return Err(Error::Protocol(format!(
                "the {label} record belongs to zone {:?}, not {:?}",
                zone.zone_name,
                lease.zone().zone_name
            )));
        }
    }
    Ok(())
}

fn asset_metadata_fields(
    master_id: &str,
    content_generation: &str,
    metadata: &PhotoMetadata,
) -> BTreeMap<String, Field> {
    let mut fields: BTreeMap<String, Field> = metadata_fields(metadata)
        .into_iter()
        .filter_map(|(name, field)| field.map(|field| (name, field)))
        .collect();
    fields.insert(
        "masterRef".to_string(),
        Field::reference(master_id.to_string()),
    );
    fields.insert(
        "contentGeneration".to_string(),
        Field::string(content_generation.to_string()),
    );
    fields
}

/// A patch, not a full record: `assetDate` absent means "remove it".
fn metadata_fields(metadata: &PhotoMetadata) -> BTreeMap<String, Option<Field>> {
    BTreeMap::from([
        (
            "relativePath".to_string(),
            Some(Field::string(metadata.relative_path.clone())),
        ),
        (
            "isFavorite".to_string(),
            Some(Field::int64(i64::from(metadata.is_favorite))),
        ),
        (
            "isHidden".to_string(),
            Some(Field::int64(i64::from(metadata.is_hidden))),
        ),
        (
            "assetDate".to_string(),
            metadata.asset_date_ms.map(Field::timestamp),
        ),
    ])
}

/// The server answers with the full record after an update, but merging makes
/// the pair check independent of how much of an unchanged record it echoes.
fn merge_record(previous: &RemoteRecord, returned: RemoteRecord) -> Result<RemoteRecord, Error> {
    let revision = returned.record_change_tag.clone().ok_or_else(|| {
        Error::Protocol(format!(
            "record {:?} was confirmed without a recordChangeTag",
            returned.record_name
        ))
    })?;
    let mut fields = previous.fields.clone();
    if !returned.fields.is_empty() {
        // The service answers an update with the whole record after the patch,
        // so this is also how a removed field (for example `assetDate`) stays
        // removed. A partial echo falls back to the record this update was
        // built from.
        fields = returned.fields;
    }
    Ok(RemoteRecord {
        record_name: returned.record_name,
        record_type: returned
            .record_type
            .or_else(|| previous.record_type.clone()),
        zone_id: returned.zone_id.or_else(|| previous.zone_id.clone()),
        record_change_tag: Some(revision),
        fields,
        events: returned.events,
        deleted: returned.deleted,
    })
}

fn expect_records(results: Vec<RecordResult>, expected: usize) -> Result<Vec<RemoteRecord>, Error> {
    let mut records = Vec::with_capacity(expected);
    for result in results {
        match result {
            RecordResult::Confirmed(record) => records.push(record),
            RecordResult::Failed(failure) => return Err(Error::InBand(Box::new(failure))),
        }
    }
    if records.len() != expected {
        return Err(Error::Protocol(format!(
            "a mutation confirmed {} of {expected} records",
            records.len()
        )));
    }
    Ok(records)
}

fn take_named(records: &mut Vec<RemoteRecord>, name: &str) -> Result<RemoteRecord, Error> {
    let index = records
        .iter()
        .position(|record| record.record_name == name)
        .ok_or_else(|| Error::Protocol(format!("mutation did not confirm record {name:?}")))?;
    Ok(records.remove(index))
}

fn photo_from_pair(asset: RemoteRecord, master: RemoteRecord) -> Result<RemotePhoto, Error> {
    PhotoGraph::from_records([&asset, &master])?
        .photos
        .into_iter()
        .next()
        .ok_or_else(|| Error::Protocol("the confirmed pair did not yield a photo".into()))
}

/// A library-relative placement with no traversal, no empty segment and no
/// backslash. Checked here so an unsafe remote path never reaches a filesystem.
pub fn is_safe_relative_path(path: &str) -> bool {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') {
        return false;
    }
    path.split('/').all(|segment| {
        !segment.is_empty() && segment != "." && segment != ".." && segment.len() <= 255
    })
}

fn is_lower_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{Engine, engine::general_purpose::STANDARD};
    use serde_json::json;

    const ORIGINAL_SHA: [u8; 32] = [7u8; 32];
    const THUMBNAIL_SHA: [u8; 32] = [9u8; 32];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    fn descriptor(bytes: &[u8], mime: &str) -> AssetDescriptor {
        AssetDescriptor {
            asset_id: Uuid::new_v4().to_string(),
            size: 100,
            file_checksum: STANDARD.encode(bytes),
            checksum_algorithm: "SHA256".into(),
            mime_type: mime.into(),
            width: Some(4),
            height: Some(3),
            download_url: None,
            receipt: None,
        }
    }

    fn master(photo_id: &str, generation: &str) -> RemoteRecord {
        let original = descriptor(&ORIGINAL_SHA, "image/heic");
        let thumbnail = descriptor(&THUMBNAIL_SHA, "image/jpeg");
        RemoteRecord {
            record_name: format!("{photo_id}-master"),
            record_type: Some(MASTER_RECORD_TYPE.into()),
            zone_id: Some(crate::model::ZoneId::primary()),
            record_change_tag: Some("master-tag".into()),
            fields: BTreeMap::from([
                ("photoId".to_string(), Field::string(photo_id)),
                ("contentGeneration".to_string(), Field::string(generation)),
                (
                    "thumbnailSourceOriginalSHA256".to_string(),
                    Field::string(hex(&ORIGINAL_SHA)),
                ),
                (
                    "resOriginalRes".to_string(),
                    Field::raw("ASSETID", json!(original)),
                ),
                (
                    "resJPEGThumbRes".to_string(),
                    Field::raw("ASSETID", json!(thumbnail)),
                ),
            ]),
            events: EditEvents::default(),
            deleted: false,
        }
    }

    fn asset(photo_id: &str, generation: &str, path: &str) -> RemoteRecord {
        RemoteRecord {
            record_name: photo_id.to_string(),
            record_type: Some(ASSET_RECORD_TYPE.into()),
            zone_id: Some(crate::model::ZoneId::primary()),
            record_change_tag: Some("asset-tag".into()),
            fields: BTreeMap::from([
                (
                    "masterRef".to_string(),
                    Field::reference(format!("{photo_id}-master")),
                ),
                ("contentGeneration".to_string(), Field::string(generation)),
                ("relativePath".to_string(), Field::string(path)),
                ("isFavorite".to_string(), Field::int64(1)),
                ("isHidden".to_string(), Field::int64(0)),
                ("assetDate".to_string(), Field::timestamp(42)),
            ]),
            events: EditEvents::default(),
            deleted: false,
        }
    }

    #[test]
    fn a_complete_pair_becomes_a_photo() {
        let graph = PhotoGraph::from_records([
            &asset("photo", "gen", "2024/a.heic"),
            &master("photo", "gen"),
        ])
        .unwrap();
        assert_eq!(graph.photos.len(), 1);
        let photo = &graph.photos[0];
        assert_eq!(photo.photo_id, "photo");
        assert_eq!(photo.master_id, "photo-master");
        assert_eq!(photo.relative_path, "2024/a.heic");
        assert!(photo.is_favorite);
        assert_eq!(photo.asset_date_ms, Some(42));
        assert_eq!(photo.asset_revision, "asset-tag");
        assert_eq!(photo.master_revision, "master-tag");
        assert_eq!(photo.thumbnail_source_sha256, hex(&ORIGINAL_SHA));
    }

    #[test]
    fn half_a_pair_is_rejected() {
        assert_eq!(
            PhotoGraph::from_records([&master("photo", "gen")]),
            Err(GraphError::UnreferencedMaster {
                master: "photo-master".into()
            })
        );
        let dangling = PhotoGraph::from_records([&asset("photo", "gen", "a.heic")]);
        assert_eq!(
            dangling,
            Err(GraphError::DanglingMaster {
                name: "photo".into(),
                master: "photo-master".into()
            })
        );
    }

    #[test]
    fn inconsistent_generations_and_paths_are_rejected() {
        let mismatch = PhotoGraph::from_records([
            &asset("photo", "gen-a", "a.heic"),
            &master("photo", "gen-b"),
        ]);
        assert_eq!(
            mismatch,
            Err(GraphError::GenerationMismatch {
                name: "photo".into(),
                master: "photo-master".into()
            })
        );

        let unsafe_path = PhotoGraph::from_records([
            &asset("photo", "gen", "../escape.heic"),
            &master("photo", "gen"),
        ]);
        assert_eq!(
            unsafe_path,
            Err(GraphError::InvalidPath {
                name: "photo".into()
            })
        );
    }

    #[test]
    fn a_thumbnail_must_describe_the_original() {
        let mut broken = master("photo", "gen");
        broken.fields.insert(
            "thumbnailSourceOriginalSHA256".into(),
            Field::string(hex(&THUMBNAIL_SHA)),
        );
        assert_eq!(
            PhotoGraph::from_records([&asset("photo", "gen", "a.heic"), &broken]),
            Err(GraphError::InvalidThumbnailDigest {
                name: "photo".into()
            })
        );
    }

    #[test]
    fn duplicate_paths_are_rejected() {
        let result = PhotoGraph::from_records([
            &asset("one", "gen", "same.heic"),
            &master("one", "gen"),
            &asset("two", "gen", "same.heic"),
            &master("two", "gen"),
        ]);
        assert!(matches!(result, Err(GraphError::DuplicatePath { .. })));
    }

    #[test]
    fn operation_ids_are_stable_and_shaped_for_the_wire() {
        let metadata = PhotoMetadata {
            relative_path: "2024/a.heic".into(),
            is_favorite: true,
            asset_date_ms: Some(1),
            is_hidden: false,
        };
        assert_eq!(
            update_metadata_operation_id("photo", &metadata),
            update_metadata_operation_id("photo", &metadata)
        );
        let changed = PhotoMetadata {
            relative_path: "2024/b.heic".into(),
            ..metadata.clone()
        };
        assert_ne!(
            update_metadata_operation_id("photo", &metadata),
            update_metadata_operation_id("photo", &changed)
        );
        for id in [
            create_operation_id("photo", "gen"),
            update_metadata_operation_id("photo", &metadata),
            delete_operation_id(
                &PhotoGraph::from_records([
                    &asset("photo", "gen", "a.heic"),
                    &master("photo", "gen"),
                ])
                .unwrap()
                .photos[0],
            ),
        ] {
            assert!(identifier(&id));
            assert!(id.len() <= 128);
            assert!(id.is_ascii());
        }
    }

    #[test]
    fn uploads_reject_an_invalid_generation_before_network_use() {
        let upload = PhotoUpload::with_ids(
            "photo",
            "master",
            Uuid::nil().to_string(),
            PhotoMetadata::new("a.heic"),
            AssetUpload::from_bytes(b"x".to_vec(), "image/heic"),
            AssetUpload::from_bytes(b"y".to_vec(), "image/jpeg"),
        );
        assert!(upload.validate().is_err());
        assert!(!is_safe_relative_path("/absolute"));
        assert!(!is_safe_relative_path("a//b"));
        assert!(is_safe_relative_path("a/b"));
    }
}
