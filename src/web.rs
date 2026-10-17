//! Serves the console's built assets from inside the binary.
//!
//! The build output is committed under `web/dist` and embedded at compile time, so a
//! deployment is still one file to copy and nothing on the server's filesystem can be
//! used to influence what the console is made of.
//!
//! These routes carry no user data. The shell is the same for a signed-in visitor and
//! for an anonymous one; everything account-specific is fetched over the authenticated
//! API with the session's Bearer token.
use axum::{
    Router,
    body::Body,
    extract::Path,
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::get,
};
use std::sync::LazyLock;

/// Compiled-in console assets. A build without `web/dist` yields an empty set, which
/// the routes below report as a clear 503 rather than failing at startup.
static ASSETS: LazyLock<include_dir::Dir<'static>> =
    LazyLock::new(|| include_dir::include_dir!("$CARGO_MANIFEST_DIR/web/dist"));

/// The embedded tree, borrowed for the life of the process. Leaking it here is what
/// lets an asset reference outlive the lookup, which is required because the files
/// are compiled into the binary and are never freed.
fn assets() -> &'static include_dir::Dir<'static> {
    LazyLock::force(&ASSETS)
}

/// Asset filenames are chosen by the bundler, so anything not addressed by name is a
/// client-side route and gets the shell.
fn resolve(path: &str) -> Option<&'static include_dir::File<'static>> {
    let cleaned = path.trim_start_matches('/');
    if cleaned.is_empty() || cleaned.split('/').any(|part| part == "..") {
        return None;
    }
    assets().get_file(cleaned)
}

fn shell() -> Option<&'static include_dir::File<'static>> {
    assets().get_file("index.html")
}

async fn index() -> Response {
    let Some(file) = shell() else {
        return missing_assets();
    };
    // The shell names hashed bundles, so it must not be cached: a deploy that swaps
    // the bundle but not the shell would otherwise keep loading a stale page.
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
            // The console renders only text and its own blobs, and talks only to this
            // origin. Locking that down here means a deployment gets the policy
            // without having to remember to add it to the proxy.
            (
                header::HeaderName::from_static("content-security-policy"),
                "default-src 'self'; img-src 'self' blob: data:; connect-src 'self'; \
                 object-src 'none'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'",
            ),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (header::REFERRER_POLICY, "no-referrer"),
        ],
        Body::from(file.contents()),
    )
        .into_response()
}

async fn asset(Path(path): Path<String>) -> Response {
    let Some(file) = resolve(&path) else {
        return index().await;
    };
    let is_html = file.path().ends_with(".html");
    // Hashed bundles can be kept forever; anything else follows the shell's policy.
    let cache = if is_html {
        "no-store"
    } else {
        "public, max-age=31536000, immutable"
    };
    let content_type = if path.ends_with(".css") {
        "text/css; charset=utf-8"
    } else if path.ends_with(".js") || path.ends_with(".mjs") {
        "text/javascript; charset=utf-8"
    } else if path.ends_with(".svg") {
        "image/svg+xml"
    } else if path.ends_with(".woff2") {
        "font/woff2"
    } else if path.ends_with(".png") {
        "image/png"
    } else if path.ends_with(".ico") {
        "image/x-icon"
    } else {
        "application/octet-stream"
    };
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, cache),
            (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        Body::from(file.contents()),
    )
        .into_response()
}

fn missing_assets() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        [
            (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        "The web console is not present in this build. Run `npm ci && npm run build` in web/ and rebuild.\n",
    )
        .into_response()
}

/// The prefix is written out here rather than nested, so the bare `/console` path is
/// a route of its own instead of depending on how nesting resolves an empty tail.
pub(crate) fn router<S: Clone + Send + Sync + 'static>() -> Router<S> {
    Router::new()
        .route("/console", get(index))
        .route("/console/", get(index))
        .route("/console/{*path}", get(asset))
}
