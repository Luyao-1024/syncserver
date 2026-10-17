//! Console read API over real HTTP: ordering, filtering, paging and the promise that
//! browsing the gallery costs nothing in synchronization terms. Every payload here is
//! synthetic and comes from the contract fixtures.
mod common;
use base64::{Engine, engine::general_purpose::STANDARD};
use common::{API_KEY, Lease, Running};
use serde_json::{Value, json};

const CK: &str = "/database/1/com.apple.photos.cloud/production/private";

async fn get(app: &Running, path: &str) -> (u16, Value) {
    let response = app
        .client
        .get(format!("{}{path}", app.base))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    (response.status().as_u16(), response.json().await.unwrap())
}

async fn ready(app: &Running) -> Lease {
    let lease = common::ready(
        &app.client,
        &app.base,
        API_KEY,
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    for phase in ["modify", "create"] {
        assert_eq!(
            lease
                .control(
                    &app.client,
                    &app.base,
                    API_KEY,
                    "phase",
                    json!({"phase":phase})
                )
                .await
                .0,
            200
        );
    }
    lease
}

/// Uploads one real photo pair from the contract fixtures and commits the pair.
///
/// The two fixture assets are genuine, thumbnail-validated PNGs, so the console
/// returns descriptors the server actually published rather than forged rows.
async fn publish_photo(app: &Running, lease: &Lease, fixture: &Value, assets: &[Value], id: usize) {
    let mut descriptors = Vec::new();
    for asset in assets {
        let bytes = STANDARD
            .decode(asset["bytesBase64"].as_str().unwrap())
            .unwrap();
        let (role, field) = if asset["role"] == "thumbnail" {
            ("thumbnail", "resJPEGThumbRes")
        } else {
            ("original", "resOriginalRes")
        };
        let body = json!({"zoneID":{"zoneName":"PrimarySync"},"tokens":[{"recordName":format!("m-{id}"),
            "recordType":"CPLMaster","fieldName":field,
            "_syncserver":{"role":role,"expectedSize":bytes.len(),"intent":"create"}}]});
        let response = lease
            .headers(
                app.client
                    .post(format!("{base}{CK}/assets/upload", base = app.base))
                    .bearer_auth(API_KEY)
                    .header("Idempotency-Key", format!("console-{id}-{role}"))
                    .json(&body),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        let reserved: Value = response.json().await.unwrap();
        let signed = url::Url::parse(reserved["tokens"][0]["url"].as_str().unwrap()).unwrap();
        let upload = lease
            .headers(
                app.client
                    .post(format!(
                        "{}{}?{}",
                        app.base,
                        signed.path(),
                        signed.query().unwrap()
                    ))
                    .bearer_auth(API_KEY)
                    .header("Content-Type", "application/octet-stream")
                    .body(bytes.to_vec()),
            )
            .send()
            .await
            .unwrap();
        assert_eq!(upload.status().as_u16(), 200);
        let published: Value = upload.json().await.unwrap();
        descriptors.push(published["singleFile"].clone());
    }
    // Start from the contract fixture so the request shape is exactly the frozen
    // one, then substitute this photo's own identity, path and resources.
    let mut create = fixture["createPhotoRequest"].clone();
    // Both records must carry one identical generation, so it is chosen once here
    // rather than derived per operation.
    let generation = uuid::Uuid::new_v4().to_string();
    for operation in create["operations"].as_array_mut().unwrap() {
        let master = operation["record"]["recordType"] == "CPLMaster";
        let record_name = if master {
            format!("master-{id}")
        } else {
            format!("photo-{id}")
        };
        operation["record"]["recordName"] = json!(record_name.clone());
        if master {
            operation["record"]["fields"]["photoId"]["value"] = json!(format!("photo-{id}"));
            operation["record"]["fields"]["contentGeneration"]["value"] = json!(generation.clone());
            operation["record"]["fields"]["resOriginalRes"]["value"] = descriptors[0].clone();
            operation["record"]["fields"]["resJPEGThumbRes"]["value"] = descriptors[1].clone();
        } else {
            operation["record"]["fields"]["masterRef"]["value"]["recordName"] =
                json!(format!("master-{id}"));
            operation["record"]["fields"]["contentGeneration"]["value"] = json!(generation.clone());
            operation["record"]["fields"]["relativePath"]["value"] =
                json!(format!("Camera/2026/IMG_{id:04}.png"));
            operation["record"]["fields"]["assetDate"] =
                json!({"type":"TIMESTAMP","value":1760000000000 + (id as i64) * 86400000});
        }
    }
    let response = lease
        .headers(
            app.client
                .post(format!("{}{CK}/records/modify", app.base))
                .bearer_auth(API_KEY)
                .header("Idempotency-Key", format!("console-create-{id}"))
                .json(&create),
        )
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let text = response.text().await.unwrap();
    assert_eq!(status, 200, "{text}");
}

async fn seeded(count: usize) -> (tempfile::TempDir, Running) {
    let fixture: Value =
        serde_json::from_str(include_str!("../docs/contracts/fixtures.json")).unwrap();
    let assets = fixture["binaryAssets"].as_array().unwrap().clone();
    let root = tempfile::tempdir().unwrap();
    // Four bytes per photo is enough for several photos and a spare page.
    let quota = 4096 * (count as u64 + 1);
    let app = Running::start(common::config_with(
        root.path(),
        &[("SYNC_USER_STORAGE_QUOTA_BYTES", &quota.to_string())],
    ))
    .await;
    let lease = ready(&app).await;
    for index in 0..count {
        publish_photo(&app, &lease, &fixture, &assets, index).await;
    }
    (root, app)
}

#[tokio::test]
async fn photos_are_listed_newest_first_with_usable_resource_urls() {
    let (_root, app) = seeded(3).await;

    let (status, body) = get(&app, "/v1/console/photos").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photos"].as_array().unwrap().len(), 3);
    assert_eq!(body["hasMore"], false);
    assert_eq!(body["nextCursor"], Value::Null);
    let photos = body["photos"].as_array().unwrap();
    // Newest first: each photo is a day later than the one before it.
    assert_eq!(photos[0]["photoId"], "photo-2");
    assert_eq!(photos[2]["photoId"], "photo-0");
    for photo in photos {
        assert_eq!(photo["mediaType"], "image");
        assert_eq!(photo["album"], "Camera/2026");
        assert_eq!(photo["original"]["checksumAlgorithm"], "SHA256");
        assert_eq!(photo["thumbnail"]["checksumAlgorithm"], "SHA256");
        assert!(photo["thumbnail"]["width"].as_i64().unwrap() > 0);
        assert_eq!(
            photo["bytes"].as_u64().unwrap(),
            photo["original"]["size"].as_u64().unwrap()
                + photo["thumbnail"]["size"].as_u64().unwrap()
        );
        assert_eq!(photo["recordChangeTags"].as_array().unwrap().len(), 2);
        // A browser cannot send the bearer header on an image element, so the URL
        // alone is not enough: the console must fetch these itself.
        let url = photo["thumbnail"]["downloadURL"].as_str().unwrap();
        assert!(url.contains("role=thumbnail"));
        assert!(url.contains("token="));
    }

    // The capability URL works exactly as the contract describes when the session
    // accompanies it.
    let url = photos[0]["thumbnail"]["downloadURL"].as_str().unwrap();
    let parsed = url::Url::parse(url).unwrap();
    let with_session = app
        .client
        .get(format!(
            "{}{}?{}",
            app.base,
            parsed.path(),
            parsed.query().unwrap()
        ))
        .bearer_auth(API_KEY)
        .send()
        .await
        .unwrap();
    assert_eq!(with_session.status(), 200);
    // This deployment has no accounts, so the API key capability is the whole
    // contract here. An account deployment additionally requires the session, which
    // is why the browser must fetch these with a header rather than an image tag.
    let without_session = app
        .client
        .get(format!(
            "{}{}?{}",
            app.base,
            parsed.path(),
            parsed.query().unwrap()
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(without_session.status(), 200);
}

#[tokio::test]
async fn paging_is_stable_and_never_repeats_or_drops_a_photo() {
    let (_root, app) = seeded(5).await;
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(value) => format!("/v1/console/photos?limit=2&cursor={value}"),
            None => "/v1/console/photos?limit=2".to_owned(),
        };
        let (status, body) = get(&app, &path).await;
        assert_eq!(status, 200, "{body}");
        pages += 1;
        assert!(pages < 10, "paging did not terminate");
        for photo in body["photos"].as_array().unwrap() {
            seen.push(photo["photoId"].as_str().unwrap().to_owned());
        }
        if body["hasMore"] == false {
            assert_eq!(body["nextCursor"], Value::Null);
            break;
        }
        cursor = Some(body["nextCursor"].as_str().unwrap().to_owned());
    }
    assert_eq!(pages, 3);
    assert_eq!(
        seen,
        vec!["photo-4", "photo-3", "photo-2", "photo-1", "photo-0"]
    );
    // A cursor the caller made up is not a position the server will honour.
    let (status, _) = get(&app, "/v1/console/photos?cursor=console-cursor.eyJ9.forged").await;
    assert_eq!(status, 400);
}

#[tokio::test]
async fn filters_narrow_the_listing_and_bad_parameters_are_refused() {
    let (_root, app) = seeded(3).await;
    let (status, body) = get(&app, "/v1/console/photos?media=video").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photos"].as_array().unwrap().len(), 0);

    let (status, body) = get(&app, "/v1/console/photos?media=image").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photos"].as_array().unwrap().len(), 3);

    let (status, body) = get(&app, "/v1/console/photos?q=IMG_0001").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photos"][0]["photoId"], "photo-1");

    let (status, body) = get(&app, "/v1/console/photos?album=Camera%2F2026").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photos"].as_array().unwrap().len(), 3);

    // Wildcards are matched literally rather than interpreted.
    let (status, body) = get(&app, "/v1/console/photos?q=%25").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photos"].as_array().unwrap().len(), 0);

    for path in [
        "/v1/console/photos?media=audio",
        "/v1/console/photos?limit=0",
        "/v1/console/photos?limit=201",
        "/v1/console/photos?limit=abc",
        "/v1/console/photos?favorite=maybe",
        "/v1/console/photos?zone=NoSuchZone",
        "/v1/console/photos?unknown=1",
        "/v1/console/photos?media=image&media=video",
        "/v1/console/photos?from=200&to=100",
    ] {
        let (status, body) = get(&app, path).await;
        assert_eq!(status, 400, "{path}: {body}");
    }
}

#[tokio::test]
async fn one_photo_can_be_read_alone_for_the_preview_view() {
    let (_root, app) = seeded(2).await;
    let (status, body) = get(&app, "/v1/console/photos/photo-1").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photo"]["photoId"], "photo-1");
    assert!(
        body["photo"]["original"]["downloadURL"]
            .as_str()
            .unwrap()
            .contains("role=original")
    );

    let (status, body) = get(&app, "/v1/console/photos/does-not-exist").await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["serverErrorCode"], "UNKNOWN_ITEM");
}

#[tokio::test]
async fn albums_timeline_and_summary_agree_with_the_listing() {
    let (_root, app) = seeded(3).await;
    let (status, body) = get(&app, "/v1/console/albums").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["albums"][0]["name"], "Camera/2026");
    assert_eq!(body["albums"][0]["photoCount"], 3);
    assert_eq!(body["ungrouped"]["photoCount"], 0);

    let (status, body) = get(&app, "/v1/console/timeline").await;
    assert_eq!(status, 200, "{body}");
    let months = body["months"].as_array().unwrap();
    assert_eq!(months.len(), 1);
    assert_eq!(months[0]["month"], "2025-10");
    assert_eq!(months[0]["photoCount"], 3);

    let (status, body) = get(&app, "/v1/console/summary").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["photoCount"], 3);
    assert_eq!(body["imageCount"], 3);
    assert_eq!(body["videoCount"], 0);
    assert_eq!(body["albumCount"], 1);

    // The gallery's byte total and the quota's byte total are the same number.
    let (status, quota) = get(&app, "/v1/storage").await;
    assert_eq!(status, 200);
    assert_eq!(body["bytes"], quota["usedBytes"]);
}

#[tokio::test]
async fn browsing_the_gallery_does_not_advance_the_change_cursor() {
    let (_root, app) = seeded(2).await;
    let token = {
        let response = app
            .client
            .post(format!("{}/v1/snapshots", app.base))
            .bearer_auth(API_KEY)
            .json(&json!({"zoneID":{"zoneName":"PrimarySync"}}))
            .send()
            .await
            .unwrap();
        let body: Value = response.json().await.unwrap();
        body["syncToken"].as_str().unwrap().to_owned()
    };
    for path in [
        "/v1/console/photos",
        "/v1/console/photos/photo-0",
        "/v1/console/albums",
        "/v1/console/timeline",
        "/v1/console/summary",
    ] {
        assert_eq!(get(&app, path).await.0, 200);
    }
    // The same token still returns the same position afterwards: reading the gallery
    // must not move a client forward.
    let response = app
        .client
        .post(format!("{}{CK}/changes/zone", app.base))
        .bearer_auth(API_KEY)
        .json(&json!({"zones":[{"zoneID":{"zoneName":"PrimarySync"},"syncToken":token,"resultsLimit":200}]}))
        .send()
        .await
        .unwrap();
    let page: Value = response.json().await.unwrap();
    assert_eq!(page["zones"][0]["records"].as_array().unwrap().len(), 0);
    assert_eq!(page["zones"][0]["moreComing"], false);
}

#[tokio::test]
async fn the_console_reads_one_deployment_and_never_answers_anonymous() {
    let app = Running::new().await;
    // A single-account deployment has a console too: it reads that one account's
    // library, and capabilities says so rather than a client guessing from a 404.
    for path in [
        "/v1/console/photos",
        "/v1/console/albums",
        "/v1/console/timeline",
        "/v1/console/summary",
    ] {
        let (status, body) = get(&app, path).await;
        assert_eq!(status, 200, "{path}: {body}");
    }
    let capabilities = get(&app, "/v1/capabilities").await.1;
    assert_eq!(capabilities["capabilities"]["webConsole"], true);
    let anonymous = app
        .client
        .get(format!("{}/v1/console/photos", app.base))
        .send()
        .await
        .unwrap();
    assert_eq!(anonymous.status().as_u16(), 401);
}
