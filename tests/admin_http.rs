//! Administration API over real HTTP: role gating, per-account quota changes and
//! cross-account accounting. Every account, password and payload here is synthetic.
mod common;
use common::Running;
use serde_json::{Value, json};
use syncserver::users::{Role, Users};

const CK: &str = "/database/1/com.apple.photos.cloud/production/private";
const PASSWORD: &str = "synthetic-admin-password";
const OTHER: &str = "synthetic-admin-password-two";

struct Deployment {
    app: Running,
    admin: String,
    admin_id: String,
    user: String,
    user_id: String,
}

async fn login(app: &Running, name: &str, password: &str) -> Value {
    let response = app
        .client
        .post(format!("{}/v1/auth/login", app.base))
        .json(&json!({"username":name, "password":password}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200, "login for {name} failed");
    response.json().await.unwrap()
}

async fn start(names: &[(&str, Role, &str)], quota: &str) -> (tempfile::TempDir, Deployment) {
    let root = tempfile::tempdir().unwrap();
    let config = common::config_with(root.path(), &[("SYNC_USER_STORAGE_QUOTA_BYTES", quota)]);
    let catalogue = Users::open(&root.path().join("data")).unwrap();
    let mut created = Vec::new();
    for (name, role, password) in names {
        let user = catalogue.create(name, password, false, *role).unwrap();
        created.push((user.username.clone(), user.id, password.to_string()));
    }
    drop(catalogue);
    let app = Running::start(config).await;
    let mut sessions = Vec::new();
    for (name, id, password) in created {
        let login = login(&app, &name, &password).await;
        let role = names
            .iter()
            .find(|(candidate, _, _)| *candidate == name)
            .expect("created account")
            .1;
        assert_eq!(login["user"]["role"], role.as_str());
        sessions.push((
            login["accessToken"].as_str().unwrap().to_owned(),
            id,
            role.is_admin(),
        ));
    }
    let (admin, admin_id) = sessions
        .iter()
        .find(|(_, _, is_admin)| *is_admin)
        .map(|(token, id, _)| (token.clone(), id.clone()))
        .expect("an administrator was created");
    let (user, user_id) = sessions
        .iter()
        .find(|(_, _, is_admin)| !*is_admin)
        .map(|(token, id, _)| (token.clone(), id.clone()))
        .expect("an ordinary account was created");
    (
        root,
        Deployment {
            app,
            admin,
            admin_id,
            user,
            user_id,
        },
    )
}

async fn get(token: &str, deployment: &Deployment, path: &str) -> (u16, Value) {
    let response = deployment
        .app
        .client
        .get(format!("{}{path}", deployment.app.base))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    // Some of these routes answer with bytes or with nothing, and this helper only
    // ever looks at the status and the error code.
    let body = response.text().await.unwrap();
    let value = serde_json::from_str(&body).unwrap_or(Value::Null);
    (status, value)
}

async fn post(token: &str, deployment: &Deployment, path: &str, body: Value) -> (u16, Value) {
    let response = deployment
        .app
        .client
        .post(format!("{}{path}", deployment.app.base))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    let body = response.text().await.unwrap_or_default();
    (status, serde_json::from_str(&body).unwrap_or(Value::Null))
}

async fn admin_get(deployment: &Deployment, path: &str) -> (u16, Value) {
    get(&deployment.admin, deployment, path).await
}

async fn admin_post(deployment: &Deployment, path: &str, body: Value) -> (u16, Value) {
    post(&deployment.admin, deployment, path, body).await
}

async fn user_get(deployment: &Deployment, path: &str) -> (u16, Value) {
    get(&deployment.user, deployment, path).await
}

async fn user_post(deployment: &Deployment, path: &str, body: Value) -> (u16, Value) {
    post(&deployment.user, deployment, path, body).await
}

/// The user row for one account inside an administration listing.
fn row(listing: &Value, id: &str) -> Value {
    listing["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == id)
        .cloned()
        .unwrap_or_else(|| panic!("no row for {id}"))
}

/// Reserves one thumbnail-sized upload for an account, so admission is judged
/// against the ceiling that is actually in force.
async fn reserve(
    deployment: &Deployment,
    lease: &common::Lease,
    name: &str,
    key: &str,
) -> (u16, Value) {
    let response = lease
        .headers(
            deployment
                .app
                .client
                .post(format!("{}{CK}/assets/upload", deployment.app.base))
                .bearer_auth(&deployment.user)
                .header("Idempotency-Key", key)
                .json(&json!({"zoneID":{"zoneName":"PrimarySync"},"tokens":[{"recordName":name,
                    "recordType":"CPLMaster","fieldName":"resJPEGThumbRes",
                    "_syncserver":{"role":"thumbnail","expectedSize":2_000_000,"intent":"create"}}]})),
        )
        .send()
        .await
        .unwrap();
    (
        response.status().as_u16(),
        response.json::<Value>().await.unwrap(),
    )
}

async fn own_quota(deployment: &Deployment, token: &str) -> Value {
    let (status, value) = get(token, deployment, "/v1/storage").await;
    assert_eq!(status, 200, "{value}");
    value
}

#[tokio::test]
async fn only_administrators_reach_the_administration_api() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;

    for path in [
        "/v1/admin/overview",
        "/v1/admin/users",
        "/v1/admin/users/x/storage",
    ] {
        let (status, body) = user_get(&deployment, path).await;
        assert_eq!(status, 403, "{path}: {body}");
        assert_eq!(body["serverErrorCode"], "ADMIN_REQUIRED");
    }
    for (path, body) in [
        (
            "/v1/admin/users",
            json!({"username":"mallory","password":OTHER}),
        ),
        ("/v1/admin/users/x/quota", json!({"bytes": 1_u64})),
        ("/v1/admin/users/x/role", json!({"role":"admin"})),
        ("/v1/admin/users/x/enabled", json!(false)),
        ("/v1/admin/users/x/password", json!({"password": OTHER})),
    ] {
        let (status, _) = user_post(&deployment, path, body).await;
        assert_eq!(status, 403, "{path}");
    }

    // An anonymous request is refused before any role question is asked.
    let (status, _) = get("", &deployment, "/v1/admin/users").await;
    assert_eq!(status, 401);

    for path in ["/v1/admin/overview", "/v1/admin/users"] {
        let (status, body) = admin_get(&deployment, path).await;
        assert_eq!(status, 200, "{path}: {body}");
    }
}

#[tokio::test]
async fn process_metrics_require_an_administrator_once_accounts_exist() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    let (status, body) = user_get(&deployment, "/v1/metrics").await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["serverErrorCode"], "ADMIN_REQUIRED");
    let (status, body) = admin_get(&deployment, "/v1/metrics").await;
    assert_eq!(status, 200, "{body}");
    assert!(body["metrics"]["requests"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn capabilities_offer_an_administrator_no_synchronization_at_all() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    let (status, admin) = get(&deployment.admin, &deployment, "/v1/capabilities").await;
    assert_eq!(status, 200);
    assert_eq!(admin["authentication"]["user"]["role"], "admin");
    assert_eq!(admin["administrator"], true);
    assert_eq!(admin["capabilities"]["adminConsole"], true);

    let (status, user) = get(&deployment.user, &deployment, "/v1/capabilities").await;
    assert_eq!(status, 200);
    assert_eq!(user["authentication"]["user"]["role"], "user");
    assert_eq!(user["administrator"], false);
    assert_eq!(user["capabilities"]["adminConsole"], false);

    // An administrator is offered no synchronization capability at all, rather than
    // being offered one and then refused: a client reading capabilities must never see
    // a capability whose calls it cannot make.
    for capability in [
        "storageQuota",
        "storageUsageByMediaType",
        "zoneListing",
        "recordCrud",
        "snapshots",
        "incrementalChanges",
        "uploadLeases",
        "fencing",
        "uploadPhases",
        "operationStatusRead",
        "queryBatch",
        "assetUpload",
        "assetDownload",
        "photoPublication",
        "webConsole",
    ] {
        assert_eq!(
            admin["capabilities"][capability], false,
            "an administrator should not advertise {capability}"
        );
        assert_eq!(user["capabilities"][capability], true, "{capability}");
    }
    // Management and operational reads are unaffected by the role split.
    for capability in [
        "operationalMetrics",
        "appleAuthentication",
        "sharing",
        "uploadResume",
    ] {
        assert_eq!(
            admin["capabilities"][capability], user["capabilities"][capability],
            "{capability}"
        );
    }
}

#[tokio::test]
async fn an_administrator_cannot_reach_any_part_of_the_photo_namespace() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    // Each entry is the method that route actually answers, so a 405 can never be
    // mistaken for a refusal.
    let native: &[(&str, &str)] = &[
        ("GET", "/v1/storage"),
        ("POST", "/v1/snapshots"),
        ("GET", "/v1/console/photos"),
        ("GET", "/v1/console/albums"),
        ("GET", "/v1/console/timeline"),
        ("GET", "/v1/console/summary"),
        ("GET", "/v1/console/photos/anything"),
        ("POST", "/v1/upload-leases"),
        ("GET", "/v1/operations/anything"),
        ("GET", "/v1/assets/anything"),
        ("GET", "/v1/assets/anything/content"),
        ("POST", "/v1/uploads/anything"),
        ("POST", "/v1/uploads/anything/renew"),
        ("POST", "/v1/uploads/anything/content"),
        ("POST", "/v1/upload-leases/anything/phase"),
    ];
    for (method, path) in native {
        let body = json!({});
        let (mine, ordinary) = if *method == "GET" {
            let mine = get(&deployment.admin, &deployment, path).await;
            let ordinary = user_get(&deployment, path).await;
            (mine, ordinary)
        } else {
            let mine = post(&deployment.admin, &deployment, path, body.clone()).await;
            let ordinary = user_post(&deployment, path, body).await;
            (mine, ordinary)
        };
        // An ordinary account is never refused on role grounds, so what is asserted
        // here is the error code rather than a status a malformed body could change.
        assert_ne!(
            ordinary.1["serverErrorCode"], "ADMIN_CANNOT_SYNC",
            "an ordinary account must not be refused on {method} {path}"
        );
        assert_eq!(
            mine.0, 403,
            "{method} {path} must be refused to an administrator"
        );
        assert_eq!(
            mine.1["serverErrorCode"], "ADMIN_CANNOT_SYNC",
            "{method} {path}"
        );
    }
    for operation in [
        "records/modify",
        "records/query",
        "records/lookup",
        "changes/zone",
        "assets/upload",
        "zones/list",
    ] {
        let path = format!("/database/1/com.apple.photos.cloud/production/private/{operation}");
        let (_, ordinary) = user_post(&deployment, &path, json!({})).await;
        assert_ne!(
            ordinary["serverErrorCode"], "ADMIN_CANNOT_SYNC",
            "an ordinary account must not be refused on {operation}"
        );
        let (status, body) = post(&deployment.admin, &deployment, &path, json!({})).await;
        assert_eq!(
            status, 403,
            "{operation} must be refused to an administrator"
        );
        assert_eq!(body["serverErrorCode"], "ADMIN_CANNOT_SYNC", "{operation}");
    }
    // Management itself keeps working, which is the whole point of the role.
    assert_eq!(admin_get(&deployment, "/v1/admin/users").await.0, 200);
    assert_eq!(admin_get(&deployment, "/v1/metrics").await.0, 200);
}

#[tokio::test]
async fn a_role_change_is_reversible_and_demotion_is_never_blocked() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    // bob has stored nothing yet, so promotion is allowed, and demotion is always
    // available again — which is what makes the "account already holds photos" guard
    // safe rather than a one-way door.
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/role", deployment.user_id),
        json!({"role": "admin"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/role", deployment.user_id),
        json!({"role": "user"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (_, listing) = admin_get(&deployment, "/v1/admin/users").await;
    assert_eq!(row(&listing, &deployment.user_id)["role"], "user");
}

#[tokio::test]
async fn a_quota_change_takes_effect_without_a_restart() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    assert_eq!(
        own_quota(&deployment, &deployment.user).await["quotaBytes"],
        1_000_000
    );

    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/quota", deployment.user_id),
        json!({"bytes": 4_000_000_000_u64}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["effectiveQuotaBytes"], 4_000_000_000_u64);

    // The account is open right now, so its own usage endpoint must already report
    // the new ceiling rather than waiting for a restart.
    assert_eq!(
        own_quota(&deployment, &deployment.user).await["quotaBytes"],
        4_000_000_000_u64
    );

    let (_, listing) = admin_get(&deployment, "/v1/admin/users").await;
    let entry = row(&listing, &deployment.user_id);
    assert_eq!(entry["usage"]["quotaBytes"], 4_000_000_000_u64);
    assert_eq!(entry["usage"]["quotaIsOverride"], true);
    assert_eq!(entry["usage"]["defaultQuotaBytes"], 1_000_000);

    // Restoring inheritance falls back to the deployment default.
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/quota", deployment.user_id),
        json!({"inherit": true}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(
        own_quota(&deployment, &deployment.user).await["quotaBytes"],
        1_000_000
    );
    let (_, listing) = admin_get(&deployment, "/v1/admin/users").await;
    let entry = row(&listing, &deployment.user_id);
    assert_eq!(entry["usage"]["quotaIsOverride"], false);
}

#[tokio::test]
async fn a_lowered_ceiling_is_consulted_by_upload_admission() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    let lease = common::ready(
        &deployment.app.client,
        &deployment.app.base,
        &deployment.user,
        vec![json!({"zoneName":"PrimarySync"})],
    )
    .await;
    // A reserve belongs to the create phase, which follows the verified delete phase.
    for phase in ["modify", "create"] {
        let (status, body) = lease
            .control(
                &deployment.app.client,
                &deployment.app.base,
                &deployment.user,
                "phase",
                json!({"phase": phase}),
            )
            .await;
        assert_eq!(status, 200, "{phase}: {body}");
    }
    let (status, body) = reserve(&deployment, &lease, "too-big", "oversized-upload").await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["serverErrorCode"], "STORAGE_QUOTA_EXCEEDED");

    // Raising the ceiling makes the same request admissible without a restart.
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/quota", deployment.user_id),
        json!({"bytes": 10_000_000_u64}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = reserve(&deployment, &lease, "fits-now", "fitting-upload").await;
    assert_eq!(status, 200, "{body}");
}

#[tokio::test]
async fn quota_requests_reject_ambiguous_or_out_of_range_values() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    let path = format!("/v1/admin/users/{}/quota", deployment.user_id);
    // Neither form, both forms, and a zero ceiling are all refused rather than
    // silently interpreted as one of them.
    for body in [
        json!({}),
        json!({"inherit": true, "bytes": 5_u64}),
        json!({"bytes": 0}),
        json!({"bytes": -1}),
    ] {
        let (status, response) = admin_post(&deployment, &path, body.clone()).await;
        assert_eq!(status, 400, "{body}: {response}");
    }
    let (_, listing) = admin_get(&deployment, "/v1/admin/users").await;
    let entry = row(&listing, &deployment.user_id);
    assert_eq!(entry["usage"]["quotaBytes"], 1_000_000);
    assert_eq!(entry["usage"]["quotaIsOverride"], false);
}

#[tokio::test]
async fn the_last_administrator_cannot_be_demoted_or_self_disabled() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/role", deployment.admin_id),
        json!({"role":"user"}),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["serverErrorCode"], "LAST_ADMINISTRATOR");

    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/enabled", deployment.admin_id),
        json!(false),
    )
    .await;
    assert_eq!(status, 409, "{body}");
    assert_eq!(body["serverErrorCode"], "LAST_ADMINISTRATOR");

    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/role", deployment.user_id),
        json!({"role":"nonsense"}),
    )
    .await;
    assert_eq!(status, 400, "{body}");

    // Promoting the ordinary account is what makes the demotion possible, so the
    // refusal above was about being the last administrator, not about the target.
    own_quota(&deployment, &deployment.user).await; // initialize the ordinary account cache
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/role", deployment.user_id),
        json!({"role":"admin"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/role", deployment.admin_id),
        json!({"role":"user"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = admin_get(&deployment, "/v1/admin/users").await;
    assert_eq!(status, 403, "{body}");
    let fresh = login(&deployment.app, "admin1", PASSWORD).await;
    let (status, _) = get(
        fresh["accessToken"].as_str().unwrap(),
        &deployment,
        "/v1/admin/users",
    )
    .await;
    assert_eq!(status, 403);
    // Promotion also takes effect for an account whose store was already cached.
    let (status, body) = get(&deployment.user, &deployment, "/v1/admin/users").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(row(&body, &deployment.user_id)["role"], "admin");
    assert_eq!(row(&body, &deployment.admin_id)["role"], "user");
    assert_eq!(
        get(&deployment.user, &deployment, "/v1/storage").await.0,
        403
    );
    let (_, caps) = get(&deployment.user, &deployment, "/v1/capabilities").await;
    assert_eq!(caps["authentication"]["user"]["role"], "admin");
    assert_eq!(caps["authentication"]["sessionTTLSeconds"], 7200);
}

#[tokio::test]
async fn account_lifecycle_over_http_matches_the_command_line() {
    let (root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/enabled", deployment.user_id),
        json!(false),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["sessionsRevoked"], true);

    let (status, _) = user_get(&deployment, "/v1/storage").await;
    assert_eq!(status, 401);
    let response = deployment
        .app
        .client
        .post(format!("{}/v1/auth/login", deployment.app.base))
        .json(&json!({"username":"bob","password":OTHER}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 401);

    // An account created over HTTP is a real catalogue row with the same isolation
    // as one created from the command line.
    let (status, body) = admin_post(
        &deployment,
        "/v1/admin/users",
        json!({"username":"carol","password":PASSWORD,"admin":false}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user"]["role"], "user");
    assert_eq!(body["user"]["username"], "carol");
    assert!(body.get("password").is_none());
    let carol = login(&deployment.app, "carol", PASSWORD).await;
    assert_eq!(
        own_quota(&deployment, carol["accessToken"].as_str().unwrap()).await["quotaBytes"],
        1_000_000
    );
    // A duplicate name must not replace the existing account.
    let (status, _) = admin_post(
        &deployment,
        "/v1/admin/users",
        json!({"username":"carol","password":PASSWORD}),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        Users::open(&root.path().join("data"))
            .unwrap()
            .accounts()
            .unwrap()
            .len(),
        3
    );

    // A password reset revokes that account's sessions, exactly as the CLI does.
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/password", deployment.user_id),
        json!({"password": "synthetic-admin-password-three"}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["sessionsRevoked"], true);
    let (status, body) = admin_post(
        &deployment,
        &format!("/v1/admin/users/{}/enabled", deployment.user_id),
        json!(true),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let login = login(&deployment.app, "bob", "synthetic-admin-password-three").await;
    assert_eq!(login["user"]["username"], "bob");
}

#[tokio::test]
async fn administration_never_exposes_any_account_photos() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    // An administrator holds no photos at all, so there is no space to report: it
    // cannot even ask for its own usage.
    let (status, denied) = get(&deployment.admin, &deployment, "/v1/storage").await;
    assert_eq!(status, 403, "{denied}");
    assert_eq!(denied["serverErrorCode"], "ADMIN_CANNOT_SYNC");

    let (status, body) = admin_get(
        &deployment,
        &format!("/v1/admin/users/{}/storage", deployment.user_id),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["user"]["id"], deployment.user_id.as_str());
    assert_eq!(body["usage"]["quotaBytes"], 1_000_000);
    // Nothing behind an administrator session may answer with another account's
    // metadata or resource capability.
    for path in ["/v1/admin/overview", "/v1/admin/users"] {
        let (_, body) = admin_get(&deployment, path).await;
        let text = body.to_string();
        for forbidden in [
            "downloadURL",
            "recordName",
            "assetId",
            "receipt",
            "CPLAsset",
        ] {
            assert!(!text.contains(forbidden), "{path} leaked {forbidden}");
        }
    }

    let (status, body) = admin_get(
        &deployment,
        "/v1/admin/users/00000000-0000-4000-8000-000000000000/storage",
    )
    .await;
    assert_eq!(status, 404, "{body}");
    assert_eq!(body["serverErrorCode"], "UNKNOWN_ITEM");
}

#[tokio::test]
async fn an_account_without_a_store_reports_zero_rather_than_failing() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "1000000",
    )
    .await;
    // The administrator has never synchronized, so it has no store directory yet.
    let (status, body) = admin_get(&deployment, "/v1/admin/users").await;
    assert_eq!(status, 200, "{body}");
    let entry = row(&body, &deployment.admin_id);
    assert_eq!(entry["readable"], true);
    assert_eq!(entry["usage"]["usedBytes"], 0);
    let (status, body) = admin_get(&deployment, "/v1/admin/overview").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["accounts"]["usageComplete"], true);
    assert_eq!(body["accounts"]["total"], 2);
    assert_eq!(body["accounts"]["enabled"], 2);
    assert_eq!(body["accounts"]["administrators"], 1);
}

#[tokio::test]
async fn the_overview_describes_the_process_rather_than_the_caller() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    // A snapshot describes the requests that have already finished, so the console has
    // to be used a little before there is anything to report.
    admin_get(&deployment, "/v1/admin/overview").await;
    admin_get(&deployment, "/v1/capabilities").await;
    let (status, body) = admin_get(&deployment, "/v1/admin/overview").await;
    assert_eq!(status, 200, "{body}");

    // The counters must describe the deployment, not the administrator's own empty
    // namespace: these requests alone have already gone through the middleware.
    assert!(
        body["http"]["requests"].as_u64().unwrap() > 0,
        "an overview that counts nothing is reading the wrong context"
    );
    assert!(body["metrics"]["requests"].as_u64().unwrap() > 0);
    assert!(body["ready"].as_bool().unwrap());

    // Every request the administrator made is itself accounted for.
    let routes = body["http"]["routes"].as_array().unwrap();
    assert!(!routes.is_empty(), "per-route counters should not be empty");
    let total: u64 = routes
        .iter()
        .map(|route| route["requests"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(total, body["http"]["requests"].as_u64().unwrap());
    assert!(
        routes.iter().any(|route| route["route"]
            .as_str()
            .unwrap()
            .contains("/v1/admin/overview")),
        "the route shape, not a raw path, is what is recorded: {routes:?}"
    );

    // A client error is counted as one, not as a success.
    user_get(&deployment, "/v1/admin/users").await;
    let (_, body) = admin_get(&deployment, "/v1/admin/overview").await;
    assert!(body["http"]["clientErrors"].as_u64().unwrap() >= 1);

    // The process reports its own footprint, and a missing measurement is absent
    // rather than reported as zero.
    assert!(body["host"]["processMemoryBytes"].as_u64().unwrap() > 0);
    assert!(body["host"]["threads"].as_u64().unwrap() > 0);
    assert!(body["host"]["memoryTotalBytes"].as_u64().unwrap() > 0);
    assert!(body["disk"]["filesystemTotalBytes"].as_u64().unwrap() > 0);
    let free = body["disk"]["filesystemFreeBytes"].as_u64().unwrap();
    let total = body["disk"]["filesystemTotalBytes"].as_u64().unwrap();
    assert!(free <= total);

    // A rate needs two samples, so the first overview reports none rather than zero.
    assert!(
        body["host"]["cpuPercent"].is_null(),
        "one sample has no interval to divide by"
    );
    assert!(body["http"]["history"].as_array().unwrap().len() < 2);
    // The window and the resolution that answered it travel together, so the console
    // never draws minute rollups under five-second tick marks.
    assert_eq!(body["http"]["window"], "15m");
    assert_eq!(body["http"]["resolutionMs"], 5000);
}

#[tokio::test]
async fn the_sampler_fills_a_history_the_console_can_draw() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    // The sampler runs for the life of the service, not only while a page is open,
    // so history exists before anyone has looked at it.
    tokio::time::sleep(std::time::Duration::from_millis(400)).await;
    for _ in 0..3 {
        admin_get(&deployment, "/v1/admin/overview").await;
    }
    let (_, body) = admin_get(&deployment, "/v1/admin/overview").await;
    let history = body["http"]["history"].as_array().unwrap();
    assert!(
        !history.is_empty(),
        "the sampler should have recorded something"
    );
    for sample in history {
        assert!(
            sample["tsMs"].as_i64().unwrap() > 0,
            "a sample needs a timestamp"
        );
        assert!(sample["rssBytes"].as_u64().unwrap() > 0);
    }
    // Oldest first, which is the order a chart reads.
    assert!(
        history
            .windows(2)
            .all(|pair| pair[0]["tsMs"].as_i64() <= pair[1]["tsMs"].as_i64()),
        "history must be in time order"
    );
    // The first sample has no interval and reports nothing rather than zero; the rate
    // appears from the second tick onward, which the sampler unit tests cover.
    assert!(
        history.first().unwrap()["cpuPercent"].is_null(),
        "one sample has no interval to divide by"
    );
}

#[tokio::test]
async fn the_history_window_picks_a_resolution_that_matches_its_span() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;
    admin_get(&deployment, "/v1/admin/overview").await;

    // The console offers the windows a hosting dashboard offers, and the server names
    // the resolution it answered with so the chart's axis cannot disagree.
    for (window, resolution) in [
        ("15m", 5_000),
        ("1h", 60_000),
        ("12h", 60_000),
        ("1d", 60_000),
        ("3d", 3_600_000),
        ("7d", 3_600_000),
    ] {
        let (status, body) =
            admin_get(&deployment, &format!("/v1/admin/overview?window={window}")).await;
        assert_eq!(status, 200, "{window}: {body}");
        assert_eq!(body["http"]["window"], window);
        assert_eq!(body["http"]["resolutionMs"], resolution, "{window}");
        assert!(body["http"]["history"].is_array(), "{window}");
    }
    // An unknown window is refused rather than silently answered with another span.
    for bad in ["2h", "", "1d%27%20or%201=1", "..", "30d"] {
        let (status, _) = admin_get(&deployment, &format!("/v1/admin/overview?window={bad}")).await;
        assert_eq!(status, 400, "{bad} should be refused");
    }
    // The default is the shortest window, which is the one that has data immediately.
    let (_, body) = admin_get(&deployment, "/v1/admin/overview").await;
    assert_eq!(body["http"]["window"], "15m");
}

#[tokio::test]
async fn the_log_search_validates_its_filters_and_reports_an_unpersisted_deployment() {
    let (_root, deployment) = start(
        &[
            ("admin1", Role::Admin, PASSWORD),
            ("bob", Role::User, OTHER),
        ],
        "20000000000",
    )
    .await;

    // A filter the reader typed has to either work or be refused. Dropping a
    // malformed bound and answering with everything looks exactly like a working
    // search that found nothing, which is the failure this endpoint used to have.
    for bad in [
        "/v1/admin/logs?limit=0",
        "/v1/admin/logs?limit=1001",
        "/v1/admin/logs?since=yesterday",
        "/v1/admin/logs?until=abc",
        "/v1/admin/logs?since=-1",
        "/v1/admin/logs?archive=maybe",
        "/v1/admin/logs?colour=blue",
        "/v1/admin/logs?user=demo",
        "/v1/admin/logs?user=not-a-uuid",
        "/v1/admin/logs?user=12345",
    ] {
        let (status, _) = admin_get(&deployment, bad).await;
        assert_eq!(status, 400, "{bad} should be refused");
    }

    // Every filter the console offers has to be accepted, including the ones that
    // are new here: the compressed archive and a pinned request.
    for good in [
        "/v1/admin/logs",
        "/v1/admin/logs?limit=1",
        "/v1/admin/logs?limit=1000",
        "/v1/admin/logs?level=WARN,ERROR",
        "/v1/admin/logs?contains=lease&archive=false",
        "/v1/admin/logs?requestId=not-a-real-id",
        "/v1/admin/logs?since=0&until=99999999999999&archive=true",
        "/v1/admin/logs?user=c094d2a8-dfc0-418b-ab28-76f658cb7a7d",
        "/v1/admin/logs?user=c094d2a8-dfc0-418b-ab28-76f658cb7a7d&archive=true&limit=50",
    ] {
        let (status, body) = admin_get(&deployment, good).await;
        assert_eq!(status, 200, "{good}: {body}");
        assert!(body["logs"].is_array(), "{good}");
        assert!(body["truncated"].is_boolean(), "{good}");
    }

    // An ordinary account cannot read the log database either.
    let (status, body) = user_get(&deployment, "/v1/admin/logs").await;
    assert_eq!(status, 403, "{body}");
    assert_eq!(body["serverErrorCode"], "ADMIN_REQUIRED");
    let (status, _) = get("", &deployment, "/v1/admin/logs").await;
    assert_eq!(status, 401);

    // This harness serves the library in process, and only the binary installs the
    // logging pipeline, so there is no log database to read. The endpoint has to say
    // so, rather than answering with an empty list that looks like a quiet server.
    // What the filters do to real records is covered against the database itself in
    // tests/logging.rs.
    let (status, body) = admin_get(&deployment, "/v1/admin/logs").await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["persisted"], false, "{body}");
    assert_eq!(body["logs"].as_array().map(Vec::len), Some(0));
    assert_eq!(body["truncated"], false, "an empty page is not a full one");
}
