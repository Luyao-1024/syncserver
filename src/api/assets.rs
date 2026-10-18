use super::{AppState, error::ApiError, leases};
use crate::{
    Error,
    domain::{
        assets::{AssetCommand, AssetReply, Capability, Reserve, Role},
        records::Failure,
    },
};
use axum::{
    Json,
    body::{Body, Bytes},
    extract::{Path, Request},
    http::{HeaderMap, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::StreamExt;
use std::{
    collections::BTreeMap,
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt},
    sync::mpsc,
};
use uuid::Uuid;

pub(super) async fn storage(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let mut response = if request.uri().query().is_some() {
        ApiError::bad_request().into_response()
    } else {
        json_reply(&state, AssetCommand::Storage).await
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
}

pub(super) fn parameters(request: &Request) -> Result<BTreeMap<String, String>, ApiError> {
    let mut params = BTreeMap::new();
    for (key, value) in
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
    {
        if !matches!(key.as_ref(), "role" | "expires" | "token")
            || params
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(ApiError::bad_request());
        }
    }
    Ok(params)
}
fn role(params: &BTreeMap<String, String>) -> Result<Role, ApiError> {
    match params.get("role").map(String::as_str) {
        Some("original") => Ok(Role::Original),
        Some("thumbnail") => Ok(Role::Thumbnail),
        _ => Err(ApiError::bad_request()),
    }
}
pub(super) fn capability(state: &AppState, request: &Request) -> Result<Capability, ApiError> {
    let params = parameters(request)?;
    let token = params.get("token").ok_or_else(ApiError::bad_request)?;
    let path = request.uri().path();
    let pieces: Vec<_> = path.split('/').collect();
    if pieces.len() != 5 || pieces[1] != "v1" || pieces[4] != "content" {
        return Err(ApiError::bad_request());
    }
    let (kind, id) = match pieces[2] {
        "uploads" if request.method() == Method::POST => ("upload", pieces[3]),
        "assets" if matches!(*request.method(), Method::GET | Method::HEAD) => {
            ("download", pieces[3])
        }
        _ => return Err(ApiError::bad_request()),
    };
    let claims: Capability = state
        .credentials
        .tokens
        .unseal(token, kind)
        .map_err(ApiError::from)?;
    if claims.namespace != state.config.namespace
        || claims.generation != state.store.metadata().database_generation
        || claims.id.to_string() != id
        || params.get("expires").and_then(|v| v.parse::<i64>().ok()) != Some(claims.expires)
        || claims.expires <= crate::jobs::now_ms().map_err(ApiError::from)? / 1000
    {
        return Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "AUTHENTICATION_REQUIRED",
            "Invalid or expired resource capability",
        ));
    }
    if kind == "download" && role(&params)? != claims.role {
        return Err(ApiError::bad_request());
    }
    Ok(claims)
}
async fn json_reply(state: &AppState, command: AssetCommand) -> Response {
    match state.store.asset(command).await {
        Ok(AssetReply::Json(value)) => Json(value).into_response(),
        Ok(_) => ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "INTERNAL_ERROR",
            "Unexpected asset result",
        )
        .into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
pub(super) async fn reserve(
    state: &AppState,
    value: serde_json::Value,
    proof: crate::domain::leases::LeaseProof,
    key: Option<String>,
) -> Response {
    match serde_json::from_value::<Reserve>(value) {
        Ok(request) => json_reply(state, AssetCommand::Reserve(request, proof, key)).await,
        Err(_) => ApiError::bad_request().into_response(),
    }
}
pub(super) async fn session(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    json_reply(&state, AssetCommand::Status(id)).await
}
pub(super) async fn renew(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    if request.uri().query().is_some() {
        return ApiError::bad_request().into_response();
    }
    let proof = match leases::proof(request.headers()) {
        Ok(proof) => proof,
        Err(error) => return error.into_response(),
    };
    match super::read_json(request, state.config.limits.json_body_limit).await {
        Ok(value) if value == serde_json::json!({}) => {
            json_reply(&state, AssetCommand::Renew(id, proof)).await
        }
        Ok(_) => ApiError::bad_request().into_response(),
        Err(error) => error.into_response(),
    }
}
pub(super) async fn describe(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    let role = match parameters(&request).and_then(|params| {
        if params.len() != 1 {
            return Err(ApiError::bad_request());
        }
        role(&params)
    }) {
        Ok(role) => role,
        Err(error) => return error.into_response(),
    };
    json_reply(&state, AssetCommand::Describe(id, role)).await
}
struct UploadCleanup {
    store: crate::store::Store,
    root: std::path::PathBuf,
    id: Uuid,
    name: String,
    finished: bool,
}
impl Drop for UploadCleanup {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        let store = self.store.clone();
        let root = self.root.clone();
        let id = self.id;
        let name = self.name.clone();
        tokio::spawn(async move {
            let _ = store
                .asset(AssetCommand::Fail(id, "UPLOAD_INTERRUPTED".into()))
                .await;
            let _ = tokio::task::spawn_blocking(move || crate::assets::cleanup(&root, &name)).await;
        });
    }
}
pub(super) async fn upload(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    match upload_inner(state, id, request).await {
        Ok(value) => Json(value).into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}
async fn upload_inner(
    state: Arc<AppState>,
    id: Uuid,
    request: Request,
) -> crate::Result<serde_json::Value> {
    let claims = capability(&state, &request).map_err(|_| {
        Failure::new(
            401,
            "AUTHENTICATION_REQUIRED",
            "Valid upload capability required",
        )
    })?;
    let proof = leases::proof(request.headers()).map_err(|_| crate::store::lease_lost())?;
    if claims.lease_id.as_ref() != Some(&proof.id) || claims.fence.as_ref() != Some(&proof.fence) {
        return Err(crate::store::lease_lost().into());
    }
    let content = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok());
    if content != Some("application/octet-stream") {
        return Err(Failure::new(
            415,
            "UNSUPPORTED_MEDIA_TYPE",
            "Upload raw application/octet-stream bytes",
        )
        .into());
    }
    let semaphore = if claims.role == Role::Thumbnail {
        state.thumbnail_uploads.clone()
    } else {
        state.original_uploads.clone()
    };
    let _permit = semaphore
        .try_acquire_owned()
        .map_err(|_| Failure::new(503, "SERVER_BUSY", "Upload capacity exhausted"))?;
    let name = format!("{id}-{}", Uuid::new_v4());
    let s = match state
        .store
        .asset(AssetCommand::Begin(id, proof.clone(), name.clone()))
        .await?
    {
        AssetReply::Session(s) => s,
        _ => return Err(Error::RecoveryFailed),
    };
    let mut cleanup = UploadCleanup {
        store: state.store.clone(),
        root: state.config.data_dir.clone(),
        id,
        name: name.clone(),
        finished: false,
    };
    let result = finish_upload(&state, id, request, proof, s, claims).await;
    if let Err(error) = &result {
        let code = match error {
            Error::Protocol(f) => f.code,
            _ => "INTERNAL_ERROR",
        };
        let _ = state.store.asset(AssetCommand::Fail(id, code.into())).await;
        let root = state.config.data_dir.clone();
        let _ = tokio::task::spawn_blocking(move || crate::assets::cleanup(&root, &name)).await;
    }
    cleanup.finished = true;
    result
}
async fn finish_upload(
    state: &Arc<AppState>,
    id: Uuid,
    request: Request,
    proof: crate::domain::leases::LeaseProof,
    s: crate::domain::assets::Session,
    claims: Capability,
) -> crate::Result<serde_json::Value> {
    if s.role != claims.role || Some(&s.intent) != claims.intent.as_ref() {
        return Err(Failure::bad().into());
    }
    if let Some(length) = request.headers().get(header::CONTENT_LENGTH)
        && length.to_str().ok().and_then(|s| s.parse::<u64>().ok()) != Some(s.expected_size)
    {
        return Err(Failure::new(
            400,
            "UPLOAD_SIZE_MISMATCH",
            "Content-Length differs from reservation",
        )
        .into());
    }
    let expected = s.expected_size;
    let (sender, receiver) = mpsc::channel::<Bytes>(4);
    let sink = crate::assets::Sink {
        root: state.config.data_dir.clone(),
        session: s,
        receiver,
        metrics: state.asset_metrics.clone(),
    };
    let mut writer = tokio::task::spawn_blocking(move || sink.receive());
    let mut stream = request.into_body().into_data_stream();
    let total = tokio::time::sleep(Duration::from_secs(
        state.config.limits.upload_total_timeout_secs,
    ));
    let idle = tokio::time::sleep(Duration::from_secs(
        state.config.limits.upload_idle_timeout_secs,
    ));
    tokio::pin!(total, idle);
    let mut keep_valid = tokio::time::interval(Duration::from_millis(250));
    keep_valid.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut received = 0u64;
    let received_result:crate::Result<()>=async{
        loop{tokio::select!{biased;
            _=&mut total=>return Err(Failure::new(408,"UPLOAD_TIMEOUT","Upload total timeout reached").into()),
            _=&mut idle=>return Err(Failure::new(408,"UPLOAD_TIMEOUT","Upload idle timeout reached").into()),
            _=keep_valid.tick()=>{if !state.ready.load(Ordering::Acquire) {return Err(crate::store::lease_lost().into());}state.store.asset(AssetCommand::Validate(id,proof.clone())).await?;},
            chunk=stream.next()=>{let Some(chunk)=chunk else{break;};let chunk=chunk.map_err(|_|Failure::new(400,"UPLOAD_INTERRUPTED","Upload body was interrupted"))?;
                received=received.checked_add(chunk.len() as u64).ok_or_else(Failure::limit)?;if received>expected{return Err(Failure::limit().into());}
                // Bound worker input chunks even if the HTTP layer supplies a large frame.
                for piece in chunk.chunks(64*1024){let chunk=Bytes::copy_from_slice(piece);loop {tokio::select!{_=&mut total=>return Err(Failure::new(408,"UPLOAD_TIMEOUT","Upload total timeout reached").into()),permit=sender.reserve()=>{permit.map_err(|_|Failure::new(400,"UPLOAD_INTERRUPTED","Upload sink failed"))?.send(chunk.clone());break;}
                        _=keep_valid.tick()=>{if !state.ready.load(Ordering::Acquire) {return Err(crate::store::lease_lost().into());}state.store.asset(AssetCommand::Validate(id,proof.clone())).await?;continue;}
                        _=&mut idle=>return Err(Failure::new(408,"UPLOAD_TIMEOUT","Upload sink idle timeout reached").into()),}}}
                idle.as_mut().reset(tokio::time::Instant::now()+Duration::from_secs(state.config.limits.upload_idle_timeout_secs));
            }
        }}Ok(())
    }.await;
    drop(sender);
    let verified = wait_work(state, id, &proof, &mut writer, total.as_ref().deadline()).await;
    received_result?;
    let verified = verified?;
    let verified = match state
        .store
        .asset(AssetCommand::Prepare(verified, proof.clone()))
        .await?
    {
        AssetReply::Verified(v) => v,
        _ => return Err(Error::RecoveryFailed),
    };
    // Capture the summary before the publisher consumes the verified asset.
    let (asset_id, session_id, asset_bytes, content_digest) = (
        verified.asset.asset_id,
        verified.session_id,
        verified.asset.size,
        verified.asset.sha256.clone(),
    );
    state
        .store
        .asset(AssetCommand::Validate(id, proof.clone()))
        .await?;
    let root = state.config.data_dir.clone();
    let ownership = state.publication.clone();
    let mut publication = tokio::task::spawn_blocking(move || {
        // Serialize only the short hard-link/unlink window. Another publisher
        // must not mistake its temporary second link for a corrupt object.
        let _owner = ownership.lock().map_err(|_| Error::WorkerFailed)?;
        crate::assets::publish(&root, &verified)
    });
    wait_work(
        state,
        id,
        &proof,
        &mut publication,
        total.as_ref().deadline(),
    )
    .await?;
    let reply = state.store.asset(AssetCommand::Complete(id, proof)).await?;
    match reply {
        AssetReply::Json(value) => {
            tracing::info!(
                // The account is what makes this attributable: the asset id alone says
                // which object, not whose library it landed in.
                user_id = state.user.as_ref().map(|user| user.id.as_str()).unwrap_or(""),
                upload_id = %session_id,
                asset_id = %asset_id,
                asset_role = claims.role.as_str(),
                asset_bytes,
                content_digest = %content_digest,
                "asset verified and published"
            );
            Ok(value)
        }
        _ => Err(Error::RecoveryFailed),
    }
}
fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<Option<&'a str>, ApiError> {
    let mut values = headers.get_all(name).iter();
    let result = values
        .next()
        .map(|v| v.to_str().map_err(|_| ApiError::bad_request()))
        .transpose()?;
    if values.next().is_some() {
        return Err(ApiError::bad_request());
    }
    Ok(result)
}
fn etag_matches(value: &str, etag: &str, weak: bool) -> bool {
    value.split(',').map(str::trim).any(|candidate| {
        candidate == "*"
            || candidate == etag
            || (weak && candidate.strip_prefix("W/") == Some(etag))
    })
}
fn range(value: &str, size: u64) -> Option<(u64, u64)> {
    let range = value.strip_prefix("bytes=")?;
    if range.contains(',')
        || !range
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'-')
    {
        return None;
    }
    let (start, end) = range.split_once('-')?;
    if start.is_empty() {
        let suffix = end.parse::<u64>().ok()?;
        if suffix == 0 {
            return None;
        }
        return Some((size.saturating_sub(suffix), size - 1));
    }
    let start = start.parse::<u64>().ok()?;
    if start >= size {
        return None;
    }
    let end = if end.is_empty() {
        size - 1
    } else {
        end.parse::<u64>().ok()?.min(size - 1)
    };
    if end < start {
        return None;
    }
    Some((start, end))
}
pub(super) async fn download(
    axum::extract::Extension(state): axum::extract::Extension<Arc<AppState>>,
    Path(id): Path<String>,
    request: Request,
) -> Response {
    let id = match Uuid::parse_str(&id) {
        Ok(id) => id,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    match download_inner(state, id, request).await {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}
async fn download_inner(
    state: Arc<AppState>,
    id: Uuid,
    request: Request,
) -> Result<Response, ApiError> {
    let params = parameters(&request)?;
    let role = role(&params)?;
    if state.user.is_some() {
        capability(&state, &request).map_err(|_| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "AUTHENTICATION_REQUIRED",
                "A valid resource capability for this account is required",
            )
        })?;
    }
    let asset = match state
        .store
        .asset(AssetCommand::Metadata(id, role.clone()))
        .await
        .map_err(ApiError::from)?
    {
        AssetReply::Asset(asset) => asset,
        _ => {
            return Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "INTERNAL_ERROR",
                "Unexpected asset metadata",
            ));
        }
    };
    let etag = format!("\"sha256:{}\"", asset.sha256);
    let mut response = Response::new(Body::empty());
    response.headers_mut().insert(
        header::ETAG,
        etag.parse().map_err(|_| ApiError::bad_request())?,
    );
    response
        .headers_mut()
        .insert(header::ACCEPT_RANGES, "bytes".parse().unwrap());
    let if_match = one_header(request.headers(), "if-match")?;
    let if_none = one_header(request.headers(), "if-none-match")?;
    if if_match.is_some_and(|value| !etag_matches(value, &etag, false)) {
        *response.status_mut() = StatusCode::PRECONDITION_FAILED;
        return Ok(response);
    }
    if if_none.is_some_and(|value| etag_matches(value, &etag, true)) {
        *response.status_mut() = StatusCode::NOT_MODIFIED;
        return Ok(response);
    }
    let head = request.method() == Method::HEAD;
    let mut start = 0;
    let mut end = asset.size - 1;
    if !head && let Some(value) = one_header(request.headers(), "range")? {
        let if_range = one_header(request.headers(), "if-range")?;
        if if_range.is_none_or(|value| value == etag) {
            match range(value, asset.size) {
                Some((from, to)) => {
                    start = from;
                    end = to;
                    *response.status_mut() = StatusCode::PARTIAL_CONTENT;
                    response.headers_mut().insert(
                        header::CONTENT_RANGE,
                        format!("bytes {start}-{end}/{}", asset.size)
                            .parse()
                            .unwrap(),
                    );
                }
                None => {
                    *response.status_mut() = StatusCode::RANGE_NOT_SATISFIABLE;
                    response.headers_mut().insert(
                        header::CONTENT_RANGE,
                        format!("bytes */{}", asset.size).parse().unwrap(),
                    );
                    return Ok(response);
                }
            }
        }
    }
    let length = end - start + 1;
    response
        .headers_mut()
        .insert(header::CONTENT_LENGTH, length.to_string().parse().unwrap());
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        asset
            .mime_type
            .parse()
            .map_err(|_| ApiError::bad_request())?,
    );
    if role == Role::Original {
        response
            .headers_mut()
            .insert(header::CONTENT_DISPOSITION, "attachment".parse().unwrap());
    }
    if head {
        return Ok(response);
    }
    let semaphore = if role == Role::Thumbnail {
        state.thumbnail_downloads.clone()
    } else {
        state.original_downloads.clone()
    };
    let permit = semaphore.try_acquire_owned().map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "SERVER_BUSY",
            "Download capacity exhausted",
        )
    })?;
    let path = crate::assets::object(&state.config.data_dir, &asset.sha256);
    let expected = asset.size;
    let file = tokio::task::spawn_blocking(move || {
        let file = crate::store::files::open_private(&path, false)?;
        if file.metadata()?.len() != expected {
            return Err(Error::RecoveryFailed);
        }
        Ok::<_, Error>(file)
    })
    .await
    .map_err(|_| ApiError::from(Error::WorkerFailed))?
    .map_err(ApiError::from)?;
    let mut file = tokio::fs::File::from_std(file);
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(Error::from)
        .map_err(ApiError::from)?;
    let stream = tokio_util::io::ReaderStream::with_capacity(file.take(length), 64 * 1024);
    let metrics = state.asset_metrics.clone();
    let stream = futures_util::stream::unfold(
        (stream, permit, metrics),
        |(mut stream, permit, metrics)| async move {
            let next = stream.next().await?;
            if let Ok(chunk) = &next {
                metrics
                    .bytes_read
                    .fetch_add(chunk.len() as u64, Ordering::Relaxed);
            }
            Some((next, (stream, permit, metrics)))
        },
    );
    *response.body_mut() = Body::from_stream(stream);
    Ok(response)
}

async fn wait_work<T>(
    state: &AppState,
    id: Uuid,
    proof: &crate::domain::leases::LeaseProof,
    job: &mut tokio::task::JoinHandle<crate::Result<T>>,
    deadline: tokio::time::Instant,
) -> crate::Result<T> {
    let mut valid = tokio::time::interval(Duration::from_millis(250));
    valid.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {biased;
            _=tokio::time::sleep_until(deadline)=>return Err(Failure::new(408,"UPLOAD_TIMEOUT","Upload total timeout reached").into()),
            _=valid.tick()=>{if !state.ready.load(Ordering::Acquire){return Err(crate::store::lease_lost().into());}state.store.asset(AssetCommand::Validate(id,proof.clone())).await?;},
            result=&mut *job=>return result.map_err(|_|Error::WorkerFailed)?,
        }
    }
}
