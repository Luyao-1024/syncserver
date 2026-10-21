//! Transport, authentication and the pinned connection.
//!
//! Every request path goes through this module so the bearer token, redirect
//! policy, response size ceiling and error envelope are handled in one place.

use crate::endpoint::Endpoint;
use crate::error::{Error, map_http};
use crate::lease::LeaseProof;
use crate::scope::{Capabilities, Namespace, PinnedScope, SessionUser, validate_capabilities};
use reqwest::header::CONTENT_TYPE;
use reqwest::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::sync::{Arc, RwLock};
use std::time::Duration;
use url::Url;

/// Cap on a JSON response body. The service caps a metadata result at 8 MiB;
/// anything larger is not a response this client can reason about.
pub(crate) const MAX_JSON_BYTES: usize = 8 * 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub access_token: String,
    pub token_type: String,
    /// Seconds since the Unix epoch, as issued by the service.
    pub expires_at: i64,
    pub user: SessionUser,
}

impl Session {
    pub fn is_expired(&self, now_seconds: i64) -> bool {
        now_seconds >= self.expires_at
    }
}

/// Configures a [`Client`]. The token may be supplied up front (an API key or a
/// previously issued session) or obtained through [`Client::login`].
pub struct ClientBuilder {
    endpoint: Endpoint,
    token: Option<String>,
    request_timeout: Option<Duration>,
    user_agent: String,
}

impl ClientBuilder {
    pub fn new(endpoint: Endpoint) -> Self {
        Self {
            endpoint,
            token: None,
            request_timeout: Some(DEFAULT_REQUEST_TIMEOUT),
            user_agent: concat!("sync-client/", env!("CARGO_PKG_VERSION")).to_string(),
        }
    }

    /// The bearer token used on every authenticated request.
    pub fn access_token(mut self, token: impl Into<String>) -> Self {
        self.token = Some(token.into());
        self
    }

    /// Timeout applied to JSON control requests. Transfers deliberately do not
    /// use it: their duration is bounded by the service's upload limits.
    pub fn request_timeout(mut self, timeout: Duration) -> Self {
        self.request_timeout = Some(timeout);
        self
    }

    pub fn without_request_timeout(mut self) -> Self {
        self.request_timeout = None;
        self
    }

    pub fn user_agent(mut self, user_agent: impl Into<String>) -> Self {
        self.user_agent = user_agent.into();
        self
    }

    pub fn build(self) -> Result<Client, Error> {
        let http = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            // A redirect could carry the bearer token to a host the caller never
            // configured. The service never needs one.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(self.user_agent)
            .build()
            .map_err(|error| Error::Transport(format!("cannot build HTTP client: {error}")))?;
        Ok(Client {
            inner: Arc::new(ClientInner {
                endpoint: self.endpoint,
                http,
                token: RwLock::new(self.token),
                scope: RwLock::new(None),
                request_timeout: self.request_timeout,
            }),
        })
    }
}

#[derive(Clone)]
pub struct Client {
    inner: Arc<ClientInner>,
}

struct ClientInner {
    endpoint: Endpoint,
    http: reqwest::Client,
    token: RwLock<Option<String>>,
    scope: RwLock<Option<PinnedScope>>,
    request_timeout: Option<Duration>,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Client")
            .field("endpoint", &self.inner.endpoint)
            .field("has_token", &self.access_token().is_some())
            .finish()
    }
}

impl Client {
    pub fn builder(endpoint: &str) -> Result<ClientBuilder, Error> {
        Ok(ClientBuilder::new(Endpoint::parse(endpoint)?))
    }

    pub fn from_endpoint(endpoint: Endpoint) -> ClientBuilder {
        ClientBuilder::new(endpoint)
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    pub fn access_token(&self) -> Option<String> {
        self.inner.token.read().ok().and_then(|token| token.clone())
    }

    pub fn set_access_token(&self, token: impl Into<String>) -> Result<(), Error> {
        let mut slot = self
            .inner
            .token
            .write()
            .map_err(|_| Error::Protocol("access token lock is poisoned".into()))?;
        *slot = Some(token.into());
        Ok(())
    }

    pub fn clear_access_token(&self) {
        if let Ok(mut slot) = self.inner.token.write() {
            *slot = None;
        }
    }

    pub fn cached_scope(&self) -> Option<PinnedScope> {
        self.inner.scope.read().ok().and_then(|scope| scope.clone())
    }

    /// Exchange credentials for a session and remember the bearer token.
    ///
    /// The credentials are used once and are not retained by the client.
    pub async fn login(&self, username: &str, password: &str) -> Result<Session, Error> {
        let url = self.url(&["v1", "auth", "login"])?;
        let request = self
            .request(Method::POST, &url, false)?
            .header(CONTENT_TYPE, "application/json")
            .body(
                serde_json::to_vec(&serde_json::json!({
                    "username": username,
                    "password": password,
                }))
                .map_err(|error| Error::Protocol(format!("login is not encodable: {error}")))?,
            );
        let response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        let session: Session = decode(&body)?;
        self.set_access_token(session.access_token.clone())?;
        Ok(session)
    }

    /// Revoke the current session. A token the service already rejects is also
    /// cleared: keeping it would only produce more unauthorized requests.
    pub async fn logout(&self) -> Result<(), Error> {
        let Some(token) = self.access_token() else {
            return Ok(());
        };
        let url = self.url(&["v1", "auth", "logout"])?;
        let response = self
            .request(Method::POST, &url, false)?
            .bearer_auth(token)
            .header(CONTENT_TYPE, "application/json")
            .body("{}")
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if status == StatusCode::NO_CONTENT || status == StatusCode::UNAUTHORIZED {
            self.clear_access_token();
            return Ok(());
        }
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        self.clear_access_token();
        Ok(())
    }

    /// `GET /readyz`, unauthenticated. A reachable but unready process is
    /// reported as a protocol failure rather than a transport failure.
    pub async fn ready(&self) -> Result<(), Error> {
        let url = self.url(&["readyz"])?;
        let response = self
            .request(Method::GET, &url, false)?
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        let value: Value = decode(&body)?;
        if value.get("status").and_then(Value::as_str) != Some("ready") {
            return Err(Error::Protocol(
                "the endpoint is reachable but not ready to serve".into(),
            ));
        }
        Ok(())
    }

    pub async fn capabilities(&self) -> Result<Capabilities, Error> {
        let url = self.url(&["v1", "capabilities"])?;
        let body = self.get_bytes(&url).await?;
        decode(&body)
    }

    /// Fetch and validate the capability document, pinning the result on this
    /// client. Later [`Client::connect`] calls reuse the pinned scope.
    pub async fn probe(&self) -> Result<PinnedScope, Error> {
        let capabilities = self.capabilities().await?;
        let scope = validate_capabilities(&self.inner.endpoint, &capabilities)?;
        self.store_scope(scope.clone())?;
        Ok(scope)
    }

    /// Probe the service and return a connection bound to the validated scope.
    /// A token is required: every namespace route is authenticated.
    pub async fn connect(&self) -> Result<Connection, Error> {
        self.require_token()?;
        if let Some(scope) = self.cached_scope() {
            return Ok(Connection::new(self.clone(), scope));
        }
        let scope = self.probe().await?;
        Ok(Connection::new(self.clone(), scope))
    }

    /// Reuse a scope pinned earlier (for example, persisted with a sync job).
    /// The public origin is checked against the configured endpoint.
    pub async fn connect_with_scope(&self, scope: PinnedScope) -> Result<Connection, Error> {
        self.require_token()?;
        let origin = Url::parse(&scope.public_base_url).map_err(|error| {
            Error::Protocol(format!("pinned publicBaseURL is invalid: {error}"))
        })?;
        self.inner.endpoint.check_origin(&origin)?;
        self.store_scope(scope.clone())?;
        Ok(Connection::new(self.clone(), scope))
    }

    fn store_scope(&self, scope: PinnedScope) -> Result<(), Error> {
        let mut slot = self
            .inner
            .scope
            .write()
            .map_err(|_| Error::Protocol("pinned scope lock is poisoned".into()))?;
        *slot = Some(scope);
        Ok(())
    }

    pub(crate) fn url(&self, segments: &[&str]) -> Result<Url, Error> {
        self.inner.endpoint.join(segments)
    }

    pub(crate) fn cloudkit_url(
        &self,
        namespace: &Namespace,
        operation: &str,
    ) -> Result<Url, Error> {
        let mut segments = vec![
            "database",
            "1",
            namespace.container.as_str(),
            namespace.environment.as_str(),
            namespace.database.as_str(),
        ];
        segments.extend(operation.split('/').filter(|segment| !segment.is_empty()));
        self.url(&segments)
    }

    pub(crate) fn require_token(&self) -> Result<String, Error> {
        self.access_token().ok_or_else(|| {
            Error::Authentication("no access token; call login or configure a token".into())
        })
    }

    /// Build a request. When `authorized` is true the bearer token is attached;
    /// resource capability requests are authorized too, because account mode
    /// requires both the session and the capability.
    pub(crate) fn request(
        &self,
        method: Method,
        url: &Url,
        authorized: bool,
    ) -> Result<reqwest::RequestBuilder, Error> {
        let mut request = self.inner.http.request(method, url.clone());
        if let Some(timeout) = self.inner.request_timeout {
            request = request.timeout(timeout);
        }
        if authorized {
            request = request.bearer_auth(self.require_token()?);
        }
        Ok(request)
    }

    pub(crate) fn http(&self) -> &reqwest::Client {
        &self.inner.http
    }

    pub(crate) async fn get_bytes(&self, url: &Url) -> Result<Vec<u8>, Error> {
        let response = self
            .request(Method::GET, url, true)?
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        Ok(body)
    }

    /// POST JSON under an optional lease proof and idempotency key. A non-2xx
    /// response is mapped through the shared error envelope.
    pub(crate) async fn post_json_bytes(
        &self,
        url: &Url,
        body: &Value,
        lease: Option<&LeaseProof>,
        idempotency_key: Option<&str>,
    ) -> Result<Vec<u8>, Error> {
        let mut request =
            self.request(Method::POST, url, true)?
                .header(CONTENT_TYPE, "application/json")
                .body(serde_json::to_vec(body).map_err(|error| {
                    Error::Protocol(format!("request is not encodable: {error}"))
                })?);
        if let Some(proof) = lease {
            for (name, value) in proof.headers() {
                request = request.header(name, value);
            }
        }
        if let Some(key) = idempotency_key {
            request = request.header("Idempotency-Key", key);
        }
        let response = request
            .send()
            .await
            .map_err(|error| Error::Transport(error.to_string()))?;
        let (status, body) = read_limited(response, MAX_JSON_BYTES).await?;
        if !status.is_success() {
            return Err(map_http(status.as_u16(), &body));
        }
        Ok(body)
    }

    pub(crate) fn check_origin(&self, url: &Url) -> Result<(), Error> {
        self.inner.endpoint.check_origin(url)
    }
}

/// A probe-validated session. Cloning is cheap and shares the client.
#[derive(Clone)]
pub struct Connection {
    client: Client,
    scope: Arc<PinnedScope>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Connection")
            .field("endpoint", self.client.endpoint())
            .field("namespace", &self.scope.namespace)
            .finish()
    }
}

impl Connection {
    pub(crate) fn new(client: Client, scope: PinnedScope) -> Self {
        Self {
            client,
            scope: Arc::new(scope),
        }
    }

    pub fn client(&self) -> &Client {
        &self.client
    }

    pub fn scope(&self) -> &PinnedScope {
        &self.scope
    }

    pub fn zone(&self, zone_name: &str) -> Result<crate::model::ZoneId, Error> {
        self.scope.zone(zone_name)
    }

    pub fn primary_zone(&self) -> Result<crate::model::ZoneId, Error> {
        self.scope.primary_zone()
    }

    pub(crate) fn check_zone(&self, zone: &crate::model::ZoneId) -> Result<(), Error> {
        zone.validate()?;
        if !self.scope.has_zone(&zone.zone_name) {
            return Err(Error::Protocol(format!(
                "zone {:?} is not configured on this server",
                zone.zone_name
            )));
        }
        Ok(())
    }

    pub(crate) fn cloudkit_url(&self, operation: &str) -> Result<Url, Error> {
        self.client.cloudkit_url(&self.scope.namespace, operation)
    }

    pub(crate) fn url(&self, segments: &[&str]) -> Result<Url, Error> {
        self.client.url(segments)
    }

    pub(crate) fn check_origin(&self, url: &Url) -> Result<(), Error> {
        self.client.check_origin(url)
    }
}

pub(crate) fn decode<T: DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    serde_json::from_slice(body)
        .map_err(|error| Error::Protocol(format!("response is not valid JSON: {error}")))
}

/// Read a response with a hard size ceiling. `Content-Length` is checked before
/// the body is read so an oversized answer is refused cheaply.
pub(crate) async fn read_limited(
    mut response: reqwest::Response,
    limit: usize,
) -> Result<(StatusCode, Vec<u8>), Error> {
    let status = response.status();
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Error::Protocol(format!(
            "response exceeds the {limit}-byte limit"
        )));
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| Error::Transport(error.to_string()))?
    {
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(Error::Protocol(format!(
                "response exceeds the {limit}-byte limit"
            )));
        }
        body.extend_from_slice(&chunk);
    }
    Ok((status, body))
}
