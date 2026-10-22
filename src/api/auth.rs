use super::*;
use axum::extract::Extension;
use serde::Deserialize;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginRequest {
    username: String,
    password: String,
}
fn no_cache(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert(header::PRAGMA, "no-cache".parse().unwrap());
    response
}
pub(super) async fn login(State(state): State<Arc<AppState>>, request: Request) -> Response {
    no_cache(login_inner(state, request).await)
}
async fn login_inner(state: Arc<AppState>, request: Request) -> Response {
    if !state.ready.load(Ordering::Acquire) {
        return ApiError::from(crate::Error::Unavailable).into_response();
    }
    let Some(authority) = state.authority.as_ref() else {
        return ApiError::from(crate::Error::Unavailable).into_response();
    };
    let source = login_source(&state, &request);
    match authority.admit_login(source) {
        Ok(true) => {}
        Ok(false) => return limited(),
        Err(e) => return ApiError::from(e).into_response(),
    }
    let Ok(body_permit) = authority.login_bodies.clone().try_acquire_owned() else {
        return limited();
    };
    let value = match read_json(request, 4096).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    drop(body_permit);
    let login = match serde_json::from_value::<LoginRequest>(value) {
        Ok(v) => v,
        Err(_) => return ApiError::bad_request().into_response(),
    };
    if login.password.len() > 1024 {
        return rejected();
    }
    let name = crate::users::canonical_username(&login.username).unwrap_or_default();
    let Ok(permit) = authority.passwords.clone().try_acquire_owned() else {
        return limited();
    };
    let users = authority.users.clone();
    let password = zeroize::Zeroizing::new(login.password);
    match tokio::task::spawn_blocking(move || {
        let _permit = permit;
        users.login(&login.username, password)
    })
    .await
    {
        Ok(Ok(Some(issued))) => {
            if let Err(e) = authority.login_outcome(source, &name, true) {
                return ApiError::from(e).into_response();
            }
            tracing::info!(
                user_id = %issued.user.id,
                username = %issued.user.username,
                expires_at = issued.expires_at,
                "session issued"
            );
            Json(issued).into_response()
        }
        Ok(Ok(None)) => {
            match authority.login_outcome(source, &name, false) {
                Ok(true) => {}
                Ok(false) => return limited(),
                Err(e) => return ApiError::from(e).into_response(),
            }
            // The username is only rendered when development detail logging is enabled.
            tracing::warn!(username = %name, "authentication rejected");
            rejected()
        }
        Ok(Err(e)) => ApiError::from(e).into_response(),
        Err(_) => ApiError::from(crate::Error::WorkerFailed).into_response(),
    }
}
fn rejected() -> Response {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "AUTHENTICATION_FAILED",
        "Invalid username or password",
    )
    .into_response()
}
fn limited() -> Response {
    let mut r = ApiError::new(
        StatusCode::TOO_MANY_REQUESTS,
        "LOGIN_RATE_LIMITED",
        "Login limit exceeded; retry later",
    )
    .into_response();
    r.headers_mut()
        .insert(header::RETRY_AFTER, "60".parse().unwrap());
    r
}
pub(super) async fn logout(
    State(root): State<Arc<AppState>>,
    Extension(current): Extension<CurrentUser>,
    request: Request,
) -> Response {
    if let Some(user) = current.0.as_ref() {
        tracing::info!(user_id = %user.id, username = %user.username, "session revocation requested");
    }
    let Some(authority) = root.authority.as_ref() else {
        return ApiError::from(crate::Error::Unavailable).into_response();
    };
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer "))
        .unwrap_or_default();
    let secret = zeroize::Zeroizing::new(token.to_owned());
    let users = authority.users.clone();
    no_cache(
        match tokio::task::spawn_blocking(move || users.logout(&secret)).await {
            Ok(Ok(())) => StatusCode::NO_CONTENT.into_response(),
            Ok(Err(e)) => ApiError::from(e).into_response(),
            Err(_) => ApiError::from(crate::Error::WorkerFailed).into_response(),
        },
    )
}

fn login_source(state: &AppState, request: &Request) -> std::net::IpAddr {
    use axum::extract::ConnectInfo;
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip())
        .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
    if state.config.trusted_proxy_ips.contains(&peer) {
        let mut headers = request.headers().get_all("x-forwarded-for").iter();
        if let Some(ip) = headers
            .next()
            .and_then(|h| h.to_str().ok())
            .and_then(|s| s.parse::<IpAddr>().ok())
            && headers.next().is_none()
        {
            return ip;
        }
    }
    peer
}
