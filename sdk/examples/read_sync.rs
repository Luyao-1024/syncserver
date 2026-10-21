//! Read the cloud photo graph and fetch one original.
//!
//! ```sh
//! SYNC_ENDPOINT=https://sync.example.com \
//! SYNC_USERNAME=alice SYNC_PASSWORD=secret \
//! cargo run -p sync-client --example read_sync -- /tmp/photos
//! ```

use std::env;
use std::path::PathBuf;
use sync_client::{AssetRole, Client, UploadLease};

#[tokio::main]
async fn main() -> Result<(), sync_client::Error> {
    let endpoint = env::var("SYNC_ENDPOINT").expect("SYNC_ENDPOINT is required");
    let username = env::var("SYNC_USERNAME").expect("SYNC_USERNAME is required");
    let password = env::var("SYNC_PASSWORD").expect("SYNC_PASSWORD is required");
    let target = PathBuf::from(
        env::args()
            .nth(1)
            .expect("usage: read_sync <download-directory>"),
    );

    let client = Client::builder(&endpoint)?.build()?;
    client.ready().await?;
    let session = client.login(&username, &password).await?;
    println!(
        "signed in as {} (expires {})",
        session.user.username, session.expires_at
    );

    let connection = client.connect().await?;
    let zone = connection.primary_zone()?;
    let bootstrap = connection.bootstrap(&zone).await?;
    println!(
        "bootstrap read {} photos at generation {}",
        bootstrap.photos.len(),
        connection.scope().database_generation
    );

    if let Some(photo) = bootstrap.photos.first() {
        let descriptor = connection
            .describe_asset(&photo.original.asset_id, AssetRole::Original)
            .await?;
        let path = target.join(&photo.relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        connection.download_to_file(&descriptor, &path).await?;
        println!("downloaded {} to {}", photo.photo_id, path.display());
    }

    // Take (and immediately release) the uplink lease only to show the phase
    // gate. A real writer keeps it for the whole delete -> modify -> create
    // cycle.
    let lease = UploadLease::acquire(&connection, "read-sync-example", &zone).await?;
    let check = lease.check(&bootstrap.sync_token, true).await?;
    println!(
        "lease current={} mayUpload={} phase={}",
        check.is_current, check.may_upload, check.phase
    );
    lease.release().await?;

    let usage = connection.storage_usage().await?;
    println!(
        "storage {}/{} bytes used ({} reserved)",
        usage.used_bytes, usage.quota_bytes, usage.reserved_bytes
    );

    client.logout().await?;
    Ok(())
}
