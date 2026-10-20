//! Read-only console API for browsing one's own cloud photos.
//!
//! Every route here reads only the authenticated account's own namespace: the zone
//! is validated against the deployment's configured zones, and no parameter can
//! name another account, container or tenant. Nothing here mints a synchronization
//! token, advances a change cursor or opens an object body, so browsing the gallery
//! can never make the next real synchronization more expensive.
//!
//! Writes are deliberately absent. The console deletes and renames through
//! `records/modify` under an upload lease, exactly like the native client, so the
//! photo graph rules, pairing and idempotency each have one implementation.
use super::{AppState, error::ApiError};
use crate::{Error, store::console};
use axum::{
    Json,
    extract::{Extension, Path, Request},
    response::{IntoResponse, Response},
};
use std::{collections::BTreeMap, sync::Arc};

/// Strict query parsing: an unknown or repeated parameter is refused rather than
/// ignored, so a mistyped filter cannot silently widen a listing.
fn parameters(
    request: &Request,
    allowed: &[&str],
) -> std::result::Result<BTreeMap<String, String>, ApiError> {
    let mut params = BTreeMap::new();
    for (key, value) in
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
    {
        if !allowed.contains(&key.as_ref())
            || params
                .insert(key.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(ApiError::bad_request());
        }
    }
    Ok(params)
}

fn flag(
    params: &BTreeMap<String, String>,
    name: &str,
) -> std::result::Result<Option<bool>, ApiError> {
    match params.get(name).map(String::as_str) {
        None | Some("any") => Ok(None),
        Some("true") => Ok(Some(true)),
        Some("false") => Ok(Some(false)),
        Some(_) => Err(ApiError::bad_request()),
    }
}

fn number(
    params: &BTreeMap<String, String>,
    name: &str,
) -> std::result::Result<Option<i64>, ApiError> {
    params
        .get(name)
        .map(|value| value.parse::<i64>().map_err(|_| ApiError::bad_request()))
        .transpose()
}

fn text(
    params: &BTreeMap<String, String>,
    name: &str,
    limit: usize,
) -> std::result::Result<Option<String>, ApiError> {
    match params.get(name).map(String::as_str) {
        None => Ok(None),
        Some("") => Ok(None),
        Some(value) if value.len() <= limit => Ok(Some(value.to_owned())),
        Some(_) => Err(ApiError::bad_request()),
    }
}

/// The zone must be one this deployment actually has. A typo returns an empty
/// listing rather than an error, but an unknown zone is a bad request rather than a
/// silently empty result.
fn zone(
    state: &Arc<AppState>,
    requested: Option<&String>,
) -> std::result::Result<String, ApiError> {
    match requested.map(String::as_str) {
        None => state
            .config
            .zones
            .first()
            .cloned()
            .ok_or_else(|| ApiError::from(Error::Unavailable)),
        Some(name) if state.config.zones.iter().any(|zone| zone == name) => Ok(name.to_owned()),
        Some(_) => Err(ApiError::bad_request()),
    }
}

async fn reply(state: &Arc<AppState>, request: console::Request) -> Response {
    let mut response = match state.store.console(request).await {
        Ok(value) => Json(value).into_response(),
        // A rejected listing parameter is the caller's mistake, not a server fault.
        Err(Error::Config(_)) => ApiError::bad_request().into_response(),
        Err(error) => ApiError::from(error).into_response(),
    };
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        "no-store".parse().unwrap(),
    );
    response
}

pub(super) async fn photos(
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let params = match parameters(
        &request,
        &[
            "zone", "limit", "cursor", "media", "album", "favorite", "hidden", "trashed", "from",
            "to", "q",
        ],
    ) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let Ok(zone) = zone(&state, params.get("zone")) else {
        return ApiError::bad_request().into_response();
    };
    let limit = match params.get("limit").map(String::as_str) {
        None => console::DEFAULT_PAGE,
        Some(raw) => match raw.parse::<usize>() {
            Ok(value) if (1..=console::MAX_PAGE).contains(&value) => value,
            _ => return ApiError::bad_request().into_response(),
        },
    };
    let (Ok(favorite), Ok(hidden), Ok(from_ms), Ok(to_ms)) = (
        flag(&params, "favorite"),
        flag(&params, "hidden"),
        number(&params, "from"),
        number(&params, "to"),
    ) else {
        return ApiError::bad_request().into_response();
    };
    if from_ms.is_some_and(|from| to_ms.is_some_and(|to| from > to)) {
        return ApiError::bad_request().into_response();
    }
    // The ordinary gallery is the live library. Trash is a separate view, so an
    // omitted filter means "live only"; `trashed=true` is the trash album and
    // `trashed=any` keeps the unfiltered meaning for callers that need both.
    let trashed = match params.get("trashed").map(String::as_str) {
        None => Some(false),
        Some("any") => None,
        Some("true") => Some(true),
        Some("false") => Some(false),
        Some(_) => return ApiError::bad_request().into_response(),
    };
    let (Ok(media), Ok(album), Ok(query), Ok(cursor)) = (
        text(&params, "media", 16),
        text(&params, "album", 4096),
        text(&params, "q", 256),
        text(&params, "cursor", 4096),
    ) else {
        return ApiError::bad_request().into_response();
    };
    reply(
        &state,
        console::Request::Photos(console::Listing {
            zone,
            media,
            album,
            favorite,
            hidden,
            trashed,
            from_ms,
            to_ms,
            query,
            limit,
            cursor,
        }),
    )
    .await
}

pub(super) async fn photo(
    Extension(state): Extension<Arc<AppState>>,
    Path(photo_id): Path<String>,
) -> Response {
    // A record name is bounded by the record contract; an oversized id is refused
    // before it reaches a query rather than being bound as a parameter.
    if photo_id.is_empty() || photo_id.len() > 512 {
        return ApiError::bad_request().into_response();
    }
    let zone = match state.config.zones.first().cloned() {
        Some(zone) => zone,
        None => return ApiError::from(Error::Unavailable).into_response(),
    };
    reply(&state, console::Request::Photo { zone, photo_id }).await
}

pub(super) async fn albums(
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let params = match parameters(&request, &["zone", "prefix"]) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let Ok(zone) = zone(&state, params.get("zone")) else {
        return ApiError::bad_request().into_response();
    };
    let Ok(prefix) = text(&params, "prefix", 4096) else {
        return ApiError::bad_request().into_response();
    };
    reply(&state, console::Request::Albums { zone, prefix }).await
}

pub(super) async fn timeline(
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let params = match parameters(&request, &["zone", "months"]) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let Ok(zone) = zone(&state, params.get("zone")) else {
        return ApiError::bad_request().into_response();
    };
    let months = match number(&params, "months") {
        Ok(Some(value)) if (1..=console::MAX_MONTHS).contains(&value) => value,
        Ok(None) => 36,
        _ => return ApiError::bad_request().into_response(),
    };
    reply(&state, console::Request::Timeline { zone, months }).await
}

pub(super) async fn summary(
    Extension(state): Extension<Arc<AppState>>,
    request: Request,
) -> Response {
    let params = match parameters(&request, &["zone"]) {
        Ok(params) => params,
        Err(error) => return error.into_response(),
    };
    let Ok(zone) = zone(&state, params.get("zone")) else {
        return ApiError::bad_request().into_response();
    };
    reply(&state, console::Request::Summary { zone }).await
}
