//! Usage: cargo run --locked --example receive_metadata -- BASE_URL API_KEY_FILE STATE_DIR
//! Persists metadata and resource obligations; intentionally does not publish UI
//! media or claim applied progress before original+thumbnail work is completed.
#[path = "support/receiver.rs"]
mod receiver;
use receiver::Receiver;
use serde_json::{Value, json};
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() != 4 {
        return Err("expected BASE_URL API_KEY_FILE STATE_DIR".into());
    }
    let base = url::Url::parse(&args[1])?;
    if base.scheme() != "https"
        && !base.host_str().is_some_and(|s| {
            s.parse::<std::net::IpAddr>()
                .ok()
                .is_some_and(|ip| ip.is_loopback())
        })
    {
        return Err("HTTPS is required outside loopback".into());
    }
    let key = std::fs::read_to_string(&args[2])?;
    let state = std::path::Path::new(&args[3]);
    std::fs::create_dir_all(state)?;
    let mut receiver = Receiver::open(&state.join("receiver.sqlite"))?;
    let client = reqwest::Client::new();
    let caps: Value = client
        .get(base.join("v1/capabilities")?)
        .bearer_auth(key.trim())
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let container = caps["namespace"]["container"]
        .as_str()
        .ok_or("missing namespace")?;
    loop {
        let mut request =
            json!({"zones":[{"zoneID":{"zoneName":"PrimarySync"},"resultsLimit":200}]});
        if let Some(token) = receiver.ingest()? {
            request["zones"][0]["syncToken"] = json!(token);
        }
        let page: Value = client
            .post(base.join(&format!(
                "database/1/{container}/production/private/changes/zone"
            ))?)
            .bearer_auth(key.trim())
            .json(&request)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        let page = &page["zones"][0];
        if page.get("serverErrorCode").is_some() {
            return Err(
                "zone synchronization failed; inspect serverErrorCode without logging credentials"
                    .into(),
            );
        }
        receiver.receive(page)?;
        if page["moreComing"] == false {
            break;
        }
    }
    println!(
        "Metadata durably received. Pending resources: {}. Applied progress ready: {}.",
        receiver.pending()?.len(),
        receiver.applied()?.is_some()
    );
    Ok(())
}
