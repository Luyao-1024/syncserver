//! Publish one photo pair from two local files.
//!
//! ```sh
//! SYNC_ENDPOINT=https://sync.example.com \
//! SYNC_USERNAME=alice SYNC_PASSWORD=secret \
//! cargo run -p sync-client --example upload_photo -- \
//!     /photos/IMG_0001.HEIC /photos/IMG_0001-thumb.jpg 2025/IMG_0001.HEIC
//! ```

use std::env;
use sync_client::{AssetUpload, Client, PhotoMetadata, PhotoUpload, UploadLease, UploadPhase};

#[tokio::main]
async fn main() -> Result<(), sync_client::Error> {
    let endpoint = env::var("SYNC_ENDPOINT").expect("SYNC_ENDPOINT is required");
    let username = env::var("SYNC_USERNAME").expect("SYNC_USERNAME is required");
    let password = env::var("SYNC_PASSWORD").expect("SYNC_PASSWORD is required");
    let mut arguments = env::args().skip(1);
    let original_path = arguments.next().expect("original path is required");
    let thumbnail_path = arguments.next().expect("thumbnail path is required");
    let relative_path = arguments
        .next()
        .unwrap_or_else(|| "Imported/photo.heic".to_string());

    let client = Client::builder(&endpoint)?.build()?;
    client.ready().await?;
    client.login(&username, &password).await?;
    let connection = client.connect().await?;
    let zone = connection.primary_zone()?;

    // The progress gate compares the applied sync token with the cloud head.
    // Feed it the token this client last persisted; a fresh installation
    // bootstraps first and passes `bootstrap.sync_token`.
    let bootstrap = connection.bootstrap(&zone).await?;

    let lease = UploadLease::acquire(&connection, "upload-example", &zone).await?;
    let check = lease.check(&bootstrap.sync_token, true).await?;
    if !check.may_upload {
        eprintln!("not current at this zone; catch up and retry before uploading");
        lease.release().await?;
        return Ok(());
    }
    while lease.phase() != Some(UploadPhase::Create) {
        lease.advance_phase().await?;
    }

    let photo = PhotoUpload::new(
        PhotoMetadata {
            relative_path,
            is_favorite: false,
            asset_date_ms: None,
            is_hidden: false,
        },
        AssetUpload::from_file(&original_path, "image/heic")
            .await?
            .with_dimensions(4032, 3024),
        AssetUpload::from_file(&thumbnail_path, "image/jpeg").await?,
    );
    let photo = connection.create_photo(&lease, photo).await?;
    println!(
        "created photo {} (master {}, generation {})",
        photo.photo_id, photo.master_id, photo.content_generation
    );

    lease.release().await?;
    client.logout().await?;
    Ok(())
}
