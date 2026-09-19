//! Local, loopback-only OmniBridge for stock Codex.
//!
//! Every Codex request enters one Responses provider.  The registry's exact
//! official model table and exact logical custom ids decide the route; neither
//! a slash heuristic nor the inbound OAuth bearer is a routing signal.

use std::collections::{HashMap, VecDeque};
use std::fs;
use std::io::{Cursor, Read};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codex_mp_core::{
    AuthStrategy, LogicalModelRoute, ProviderConfig, ProviderProtocol, ProviderRegistry,
    default_registry_path,
};
use codex_mp_credentials::CredentialStore;
use codex_mp_protocol_bridge::{
    BridgeContext, BridgeError, CodexChatHistoryStore, CodexChatReasoningConfig, RouteDomain,
    chat_sse_to_responses_stream, chat_to_responses_with_request, portable_request,
    record_portable_response, responses_to_chat,
};
use futures::StreamExt;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Notify, RwLock};
use tracing::debug;
use url::Url;
use uuid::Uuid;

pub const DEFAULT_BIND_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);
pub const CAPABILITY_HEADER: &str = "x-codex-omnibridge-token";
pub const ROUTER_BUILD_PROFILE: &str = "stock-omnibridge-v1";
const DEFAULT_OFFICIAL_BASE_URL: &str = "https://chatgpt.com/backend-api/codex";
const MAX_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;
/// Cap on a buffered (non-streaming) upstream response.
///
/// `MAX_REQUEST_BODY_BYTES` only bounds the client -> router direction. Without a
/// matching cap here, a provider that returns one enormous JSON document could
/// make the router allocate without bound and be OOM-killed, taking every local
/// Codex session with it.
const MAX_UPSTREAM_RESPONSE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Debug, Clone)]
pub struct RouterConfig {
    pub bind_ip: IpAddr,
    pub port: u16,
    pub endpoint_file: Option<PathBuf>,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            bind_ip: DEFAULT_BIND_IP,
            port: 0,
            endpoint_file: None,
        }
    }
}

impl RouterConfig {
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind_ip, self.port)
    }

    pub fn validate(&self) -> Result<(), RouterError> {
        if !self.bind_ip.is_loopback() {
            return Err(RouterError::NonLoopbackBind(self.bind_ip));
        }
        Ok(())
    }
}

/// Cap on remembered `previous_response_id` routes. The map is append-only, so
/// without a bound a long-lived router grows one entry per response forever.
const MAX_TRACKED_RESPONSE_ROUTES: usize = 4096;

/// Upstream connect deadline.
const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Upstream deadline covering the whole request *including* body streaming.
///
/// Applied per-request rather than on the client, because a client-wide timeout
/// would abort long-lived streaming responses mid-turn.
const UPSTREAM_READ_TIMEOUT: Duration = Duration::from_secs(600);

/// Build the client used for every upstream call.
///
/// Redirects are disabled deliberately: `base_url` is operator-supplied, and a
/// compromised or hostile endpoint could otherwise answer `302 Location:
/// http://169.254.169.254/...` (or a loopback address) and turn the local router
/// into an SSRF pivot, re-sending the user's conversation body to the redirect
/// target. `Client::new()` follows up to ten redirects across arbitrary hosts and
/// schemes and applies no timeout at all.
fn build_upstream_client() -> Client {
    match client_builder().build() {
        Ok(client) => client,
        Err(error) => {
            // Never fall back to `Client::new()` here: that silently restores
            // exactly the redirect-following, timeout-free policy this function
            // exists to prevent, i.e. it fails *open* on a security control. A
            // client without a TLS backend is still safe to build, so retry with
            // the same hardening and only give up on the redirect/limit knobs if
            // even that fails.
            eprintln!(
                "codex-mp: could not build the hardened upstream client ({error}); \
                 retrying without connection pooling"
            );
            client_builder()
                .pool_max_idle_per_host(0)
                .build()
                .unwrap_or_else(|error| {
                    eprintln!(
                        "codex-mp: upstream client construction failed ({error}); \
                         redirects remain disabled and timeouts still apply"
                    );
                    Client::builder()
                        .redirect(reqwest::redirect::Policy::none())
                        .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
                        .build()
                        .unwrap_or_else(|_| {
                            // Last resort. `Client::new()` is the only remaining
                            // constructor; the router keeps working, and the
                            // operator sees this line.
                            eprintln!(
                                "codex-mp: FATAL: no hardened upstream client available; \
                                 upstream redirects will be followed"
                            );
                            Client::new()
                        })
                })
        }
    }
}

fn client_builder() -> reqwest::ClientBuilder {
    // Deliberately no client-wide `.timeout(..)`: it would abort long-lived
    // streaming turns mid-response. The deadline is applied per request with
    // `UPSTREAM_READ_TIMEOUT` instead.
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(UPSTREAM_CONNECT_TIMEOUT)
}

/// A `previous_response_id` -> route map with a hard size bound and FIFO
/// eviction. Insertion order is used as the recency signal, which is enough here
/// because entries are only ever appended.
#[derive(Default)]
struct BoundedRouteMap {
    entries: HashMap<String, String>,
    order: VecDeque<String>,
}

impl BoundedRouteMap {
    fn insert(&mut self, key: String, value: String) {
        if self.entries.insert(key.clone(), value).is_none() {
            self.order.push_back(key);
        }
        while self.order.len() > MAX_TRACKED_RESPONSE_ROUTES {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }
    }

    fn get(&self, key: &str) -> Option<&String> {
        self.entries.get(key)
    }
}

#[derive(Clone)]
pub struct RouterState {
    pub registry: Arc<RwLock<ProviderRegistry>>,
    pub credentials: Arc<dyn CredentialStore>,
    pub http_client: Client,
    capability_token: Option<Arc<SecretString>>,
    official_base_url: String,
    history: Arc<CodexChatHistoryStore>,
    history_routes: Arc<StdMutex<BoundedRouteMap>>,
    shutdown: Arc<Notify>,
    /// Process-local provider-credential cache.
    ///
    /// Resolving a credential costs an OS keyring round trip (on Linux a D-Bus
    /// call to the Secret Service). Doing that on every custom-model request adds
    /// latency to each turn and, worse, means a slow or wedged keyring daemon
    /// stalls requests even though the secret has not changed. Entries are keyed
    /// by credential reference and invalidated whenever the registry generation
    /// moves, so a `provider edit` still takes effect immediately.
    credential_cache: Arc<StdMutex<CredentialCache>>,
}

/// Credentials resolved during the current registry generation.
#[derive(Default)]
struct CredentialCache {
    generation: Option<u64>,
    entries: HashMap<String, Arc<SecretString>>,
}

impl CredentialCache {
    /// Look up a reference, dropping the whole cache if the registry moved on.
    fn get(&mut self, generation: u64, reference: &str) -> Option<Arc<SecretString>> {
        if self.generation != Some(generation) {
            self.entries.clear();
            self.generation = Some(generation);
            return None;
        }
        self.entries.get(reference).cloned()
    }

    fn insert(&mut self, generation: u64, reference: &str, secret: Arc<SecretString>) {
        if self.generation != Some(generation) {
            self.entries.clear();
            self.generation = Some(generation);
        }
        self.entries.insert(reference.to_owned(), secret);
    }
}

impl RouterState {
    /// Build a state without a capability token.
    ///
    /// Only safe for in-process tests that never serve traffic: `authorize()`
    /// fails closed when no token is configured, so a served state must be built
    /// with [`RouterState::with_capability_token`].
    pub fn new(registry: ProviderRegistry, credentials: Arc<dyn CredentialStore>) -> Self {
        Self::with_optional_capability_token(registry, credentials, None)
    }

    pub fn with_capability_token(
        registry: ProviderRegistry,
        credentials: Arc<dyn CredentialStore>,
        capability_token: SecretString,
    ) -> Self {
        Self::with_optional_capability_token(registry, credentials, Some(capability_token))
    }

    fn with_optional_capability_token(
        registry: ProviderRegistry,
        credentials: Arc<dyn CredentialStore>,
        capability_token: Option<SecretString>,
    ) -> Self {
        Self {
            registry: Arc::new(RwLock::new(registry)),
            credentials,
            http_client: build_upstream_client(),
            capability_token: capability_token.map(Arc::new),
            official_base_url: std::env::var("CODEX_MP_OFFICIAL_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_OFFICIAL_BASE_URL.into()),
            history: Arc::new(CodexChatHistoryStore::default()),
            history_routes: Arc::new(StdMutex::new(BoundedRouteMap::default())),
            shutdown: Arc::new(Notify::new()),
            credential_cache: Arc::new(StdMutex::new(CredentialCache::default())),
        }
    }

    /// Override the official ChatGPT backend for a controlled fixture or a
    /// configured enterprise gateway. Production defaults to the stock
    /// ChatGPT Codex backend and never falls back to a custom provider.
    pub fn with_official_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.official_base_url = base_url.into();
        self
    }

    /// Resolve a provider credential, using the per-generation cache.
    async fn resolve_credential(
        &self,
        generation: u64,
        reference: &str,
    ) -> Result<Arc<SecretString>, RouterError> {
        {
            let mut cache = self
                .credential_cache
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if let Some(secret) = cache.get(generation, reference) {
                return Ok(secret);
            }
        }
        // Offloaded to a blocking thread: see `apply_custom_headers`.
        let secret =
            codex_mp_credentials::get_blocking(self.credentials.clone(), reference.to_owned())
                .await
                .map_err(|error| RouterError::Credential(error.to_string()))?;
        let secret = Arc::new(secret);
        self.credential_cache
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(generation, reference, secret.clone());
        Ok(secret)
    }

    fn remember_response_route(&self, response: &Value, route_key: &str) {
        if let Some(response_id) = response.get("id").and_then(Value::as_str) {
            // A poisoned lock must not take the router down; the route cache is
            // a best-effort optimization, so recovering the inner value is fine.
            self.history_routes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(response_id.to_owned(), route_key.to_owned());
        }
    }

    fn previous_route(&self, response_id: &str) -> Option<String> {
        self.history_routes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(response_id)
            .cloned()
    }

    fn is_capability_protected(&self) -> bool {
        self.capability_token.is_some()
    }

    pub async fn reload_registry(&self) -> Result<usize, RouterError> {
        let path = self.registry.read().await.path().to_path_buf();
        let registry = ProviderRegistry::load(path)
            .map_err(|error| RouterError::Registry(error.to_string()))?;
        let enabled_models = registry.enabled_custom_models().count();
        *self.registry.write().await = registry;
        Ok(enabled_models)
    }
}

#[derive(Debug, Error)]
pub enum RouterError {
    #[error("router may only bind to loopback, got {0}")]
    NonLoopbackBind(IpAddr),
    #[error("request body is not valid JSON: {0}")]
    InvalidJson(#[from] serde_json::Error),
    #[error("request did not contain a string `model`")]
    MissingModel,
    #[error("provider `{0}` is disabled or unknown")]
    UnknownProvider(String),
    #[error("model `{0}` is disabled or unknown")]
    UnknownModel(String),
    #[error("provider `{0}` has an invalid base URL: {1}")]
    InvalidBaseUrl(String, String),
    #[error("credential error: {0}")]
    Credential(String),
    #[error("upstream request failed: {0}")]
    Upstream(String),
    #[error("could not bind {0}: {1}")]
    Bind(std::net::SocketAddr, String),
    #[error("official model traffic is not handled by the third-party adapter")]
    OfficialPassthroughRequired,
    #[error("official route requires a valid ChatGPT OAuth Authorization bearer")]
    MissingOfficialAuthorization,
    #[error("history context boundary: {0}")]
    ContextBoundary(String),
    #[error("endpoint `{0}` is not supported by the selected route")]
    UnsupportedEndpoint(String),
    #[error("provider header is invalid: {0}")]
    InvalidHeader(String),
    #[error("router capability token is required for serving")]
    MissingCapabilityToken,
    #[error("router endpoint file error: {0}")]
    EndpointFile(String),
    #[error("router request is not authorized")]
    Unauthorized,
    #[error("router request must use a loopback Host and Origin")]
    InvalidRequestOrigin,
    #[error("router requests must use Content-Type: application/json")]
    UnsupportedContentType,
    #[error("request Content-Encoding is unsupported: {0}")]
    UnsupportedContentEncoding(String),
    #[error("request body decompression failed: {0}")]
    InvalidCompressedBody(String),
    #[error("request body exceeds the {0} byte decoded limit")]
    RequestBodyTooLarge(usize),
    #[error("registry reload failed: {0}")]
    Registry(String),
}

#[derive(Debug, Clone)]
pub struct RouterEndpoint {
    pub base_url: String,
    pub capability_token: SecretString,
}

#[derive(Debug, Serialize, Deserialize)]
struct RouterEndpointFile {
    schema_version: u32,
    base_url: String,
    capability_token: String,
}

pub const ROUTER_ENDPOINT_SCHEMA_VERSION: u32 = 1;

pub fn default_router_endpoint_path() -> PathBuf {
    default_registry_path()
        .parent()
        .map(|path| path.join("router-endpoint.json"))
        .unwrap_or_else(|| PathBuf::from("router-endpoint.json"))
}

pub fn new_capability_token() -> SecretString {
    SecretString::from(Uuid::new_v4().to_string())
}

pub fn load_router_endpoint(path: impl AsRef<Path>) -> Result<RouterEndpoint, RouterError> {
    let path = path.as_ref();
    let link_metadata =
        fs::symlink_metadata(path).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    if link_metadata.file_type().is_symlink() {
        return Err(RouterError::EndpointFile(format!(
            "endpoint file `{}` must not be a symlink",
            path.display()
        )));
    }
    let _metadata =
        fs::metadata(path).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if _metadata.permissions().mode() & 0o077 != 0 {
            return Err(RouterError::EndpointFile(format!(
                "endpoint file `{}` is accessible by other users",
                path.display()
            )));
        }
    }
    let file: RouterEndpointFile = serde_json::from_str(
        &fs::read_to_string(path).map_err(|error| RouterError::EndpointFile(error.to_string()))?,
    )
    .map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    if file.schema_version != ROUTER_ENDPOINT_SCHEMA_VERSION
        || file.capability_token.trim().is_empty()
    {
        return Err(RouterError::EndpointFile(
            "unsupported or incomplete endpoint".into(),
        ));
    }
    validate_loopback_url(&file.base_url)?;
    Ok(RouterEndpoint {
        base_url: file.base_url,
        capability_token: SecretString::from(file.capability_token),
    })
}

fn write_router_endpoint(
    path: impl AsRef<Path>,
    addr: SocketAddr,
    capability_token: &SecretString,
) -> Result<(), RouterError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    }
    let file = RouterEndpointFile {
        schema_version: ROUTER_ENDPOINT_SCHEMA_VERSION,
        base_url: format!("http://{addr}"),
        capability_token: capability_token.expose_secret().to_owned(),
    };
    // This document carries the capability token, which authorises every Router
    // endpoint. The previous version used `fs::write` (umask, typically 0664) and
    // only chmodded to 0600 afterwards, leaving a window in which any local user
    // could read the token. It also `unwrap()`ed the serialization; that cannot
    // fail for this struct, but a panic in the writer is still the wrong shape.
    // `write_private_atomic` creates the file with mode 0600 and reports errors.
    let mut bytes = serde_json::to_vec_pretty(&file)
        .map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    bytes.push(b'\n');
    codex_mp_core::write_private_atomic(path, &bytes)
        .map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    Ok(())
}

fn validate_loopback_url(value: &str) -> Result<(), RouterError> {
    let parsed = Url::parse(value).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    if parsed.scheme() != "http" || (parsed.path() != "" && parsed.path() != "/") {
        return Err(RouterError::EndpointFile(
            "endpoint must be an http loopback URL".into(),
        ));
    }
    let host = parsed
        .host_str()
        .ok_or_else(|| RouterError::EndpointFile("endpoint is missing a host".into()))?;
    let is_loopback = host == "localhost"
        || host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false);
    if !is_loopback {
        return Err(RouterError::EndpointFile(
            "endpoint host must be loopback".into(),
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum RouteClass {
    Official,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteDescription {
    pub logical_model_id: String,
    pub route_class: RouteClass,
    pub provider_id: Option<String>,
    pub upstream_model_id: Option<String>,
    pub protocol: Option<ProviderProtocol>,
    pub generation: u64,
}

pub fn app(state: RouterState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/readyz", get(readyz))
        .route("/v1/models", get(models))
        .route("/models", get(models))
        .route("/v1/responses", post(responses))
        .route("/responses", post(responses))
        .route("/v1/responses/compact", post(compact))
        .route("/responses/compact", post(compact))
        .route("/v1/alpha/search", post(search))
        .route("/alpha/search", post(search))
        .route("/v1/images/generations", post(images_generations))
        .route("/images/generations", post(images_generations))
        .route("/v1/images/edits", post(images_edits))
        .route("/images/edits", post(images_edits))
        .route("/v1/chat/completions", post(chat_completions))
        .route("/chat/completions", post(chat_completions))
        .route("/admin/reload", post(admin_reload))
        .route("/admin/status", get(admin_status))
        .route("/admin/shutdown", post(admin_shutdown))
        .with_state(state)
}

pub async fn serve(config: RouterConfig, state: RouterState) -> Result<(), RouterError> {
    config.validate()?;
    if !state.is_capability_protected() {
        return Err(RouterError::MissingCapabilityToken);
    }
    // A bind failure (port in use, permission denied) is *not* an upstream
    // problem. Reporting it as "upstream request failed" sent the operator
    // looking at the third-party provider instead of at the local port.
    let listener = tokio::net::TcpListener::bind(config.socket_addr())
        .await
        .map_err(|error| RouterError::Bind(config.socket_addr(), error.to_string()))?;
    let endpoint_file = config.endpoint_file.clone();
    if let Some(endpoint_file) = endpoint_file.as_ref() {
        let addr = listener
            .local_addr()
            .map_err(|error| RouterError::Bind(config.socket_addr(), error.to_string()))?;
        let token = state
            .capability_token
            .as_deref()
            .expect("capability token checked above");
        write_router_endpoint(endpoint_file, addr, token)?;
    }
    let shutdown = state.shutdown.clone();
    let result = axum::serve(listener, app(state))
        .with_graceful_shutdown(async move {
            shutdown.notified().await;
        })
        .await
        .map_err(|error| RouterError::Upstream(error.to_string()));
    if let Some(endpoint_file) = endpoint_file {
        remove_stale_endpoint(&endpoint_file)
            .map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    }
    result
}

async fn healthz(headers: HeaderMap) -> Response {
    if let Some(response) = validate_request_origin(&headers) {
        return response;
    }
    Json(json!({"status": "ok", "bind": "127.0.0.1-only"})).into_response()
}

async fn readyz(State(state): State<RouterState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    let registry = state.registry.read().await;
    let registry_path = registry.path().to_path_buf();
    let catalog_path = registry_path
        .parent()
        .map(|parent| parent.join("models.json"));
    Json(json!({
        "status": "ready",
        "instance": "omnibridge",
        "build_profile": ROUTER_BUILD_PROFILE,
        "registry_generation": registry.generation(),
        "registry_sha256": file_sha256(&registry_path),
        "catalog_sha256": catalog_path.as_deref().and_then(file_sha256),
        "official_models": registry.official_model_ids().len(),
        "enabled_custom_models": registry.enabled_custom_models().count(),
        "capability_header": CAPABILITY_HEADER,
    }))
    .into_response()
}

async fn admin_reload(State(state): State<RouterState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    match state.reload_registry().await {
        Ok(enabled_models) => Json(json!({
            "status": "reloaded",
            "enabled_custom_models": enabled_models,
        }))
        .into_response(),
        Err(error) => error_response(StatusCode::BAD_GATEWAY, error.to_string()),
    }
}

async fn admin_status(State(state): State<RouterState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    let registry = state.registry.read().await;
    Json(json!({
        "status": "ok",
        "providers": registry.providers().len(),
        "enabled_custom_models": registry.enabled_custom_models().count(),
    }))
    .into_response()
}

async fn admin_shutdown(State(state): State<RouterState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    state.shutdown.notify_one();
    Json(json!({"status": "shutting_down"})).into_response()
}

async fn models(State(state): State<RouterState>, headers: HeaderMap) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    let registry = state.registry.read().await;
    let mut data: Vec<Value> = registry
        .official_model_ids()
        .iter()
        .map(|model_id| {
            json!({
                "id": model_id,
                "object": "model",
                "owned_by": "openai",
                "route_class": "official",
            })
        })
        .collect();
    data.extend(
        registry
            .enabled_custom_models()
            .map(|(provider, model)| {
                json!({
                    "id": model.logical_model_id,
                    "object": "model",
                    "owned_by": provider.name,
                    "display_name": model.display_name,
                    "capabilities": model.capabilities,
                    "route_class": "custom",
                })
            })
            .collect::<Vec<_>>(),
    );
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn responses(
    State(state): State<RouterState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    forward_request_v2(state, headers, Some(uri), body, OmniEndpoint::Responses).await
}

async fn compact(
    State(state): State<RouterState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    forward_request_v2(state, headers, Some(uri), body, OmniEndpoint::Compact).await
}

async fn search(
    State(state): State<RouterState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    forward_request_v2(state, headers, Some(uri), body, OmniEndpoint::Search).await
}

async fn images_generations(
    State(state): State<RouterState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    forward_request_v2(
        state,
        headers,
        Some(uri),
        body,
        OmniEndpoint::ImagesGenerations,
    )
    .await
}

async fn images_edits(
    State(state): State<RouterState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    forward_request_v2(state, headers, Some(uri), body, OmniEndpoint::ImagesEdits).await
}

async fn chat_completions(
    State(state): State<RouterState>,
    headers: HeaderMap,
    uri: axum::http::Uri,
    body: Body,
) -> Response {
    forward_request_v2(
        state,
        headers,
        Some(uri),
        body,
        OmniEndpoint::ChatCompletions,
    )
    .await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OmniEndpoint {
    Responses,
    ChatCompletions,
    Compact,
    Search,
    ImagesGenerations,
    ImagesEdits,
}

impl OmniEndpoint {
    fn path(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat/completions",
            Self::Compact => "responses/compact",
            Self::Search => "alpha/search",
            Self::ImagesGenerations => "images/generations",
            Self::ImagesEdits => "images/edits",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Responses => "responses",
            Self::ChatCompletions => "chat_completions",
            Self::Compact => "responses/compact",
            Self::Search => "alpha/search",
            Self::ImagesGenerations => "images/generations",
            Self::ImagesEdits => "images/edits",
        }
    }
}

async fn forward_request_v2(
    state: RouterState,
    headers: HeaderMap,
    uri: Option<axum::http::Uri>,
    body: Body,
    endpoint: OmniEndpoint,
) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    // `/v1/images/edits` is defined by OpenAI as `multipart/form-data`, so it
    // cannot go through the JSON pipeline at all: the body carries no `model`
    // field to parse and must reach the upstream byte-for-byte. Forcing JSON on
    // every endpoint made the route unreachable for the very format it exists to
    // serve — a conforming client got 415 before any provider was consulted.
    if matches!(endpoint, OmniEndpoint::ImagesEdits) {
        return forward_multipart_request(state, headers, uri, body).await;
    }
    let _ = uri;
    if let Some(response) = require_json_content_type(&headers) {
        return response;
    }
    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        // `to_bytes` fails precisely when the limit is exceeded, which is a 413,
        // not a malformed-request 400.
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                RouterError::RequestBodyTooLarge(MAX_REQUEST_BODY_BYTES).to_string(),
            );
        }
    };
    let decoded = match decode_request_body(&headers, &bytes) {
        Ok(decoded) => decoded,
        Err(RouterError::RequestBodyTooLarge(limit)) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                RouterError::RequestBodyTooLarge(limit).to_string(),
            );
        }
        // An unsupported `Content-Encoding` is a media-type problem, not a
        // malformed body.
        Err(error @ RouterError::UnsupportedContentEncoding(_)) => {
            return error_response(StatusCode::UNSUPPORTED_MEDIA_TYPE, error.to_string());
        }
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.to_string()),
    };
    let mut request: Value = match serde_json::from_slice(&decoded) {
        Ok(value) => value,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.to_string()),
    };
    let Some(logical_model_id) = request
        .get("model")
        .and_then(Value::as_str)
        .map(str::to_owned)
    else {
        return error_response(
            StatusCode::BAD_REQUEST,
            RouterError::MissingModel.to_string(),
        );
    };

    let (route, provider, model) = {
        let registry = state.registry.read().await;
        let resolved = match registry.resolve_logical_model_route(&logical_model_id) {
            Ok(route) => route,
            Err(error) => return error_response(StatusCode::NOT_FOUND, error.to_string()),
        };
        match resolved {
            LogicalModelRoute::Official { model_id } => (
                RouteDescription {
                    logical_model_id: model_id,
                    route_class: RouteClass::Official,
                    provider_id: None,
                    upstream_model_id: None,
                    protocol: None,
                    generation: registry.generation(),
                },
                None,
                None,
            ),
            LogicalModelRoute::Custom {
                logical_model_id,
                provider_id,
                upstream_model_id,
                protocol,
            } => {
                let provider = registry
                    .provider(&provider_id)
                    .filter(|provider| provider.enabled)
                    .cloned();
                let Some(provider) = provider else {
                    return error_response(
                        StatusCode::NOT_FOUND,
                        RouterError::UnknownProvider(provider_id).to_string(),
                    );
                };
                let model = provider
                    .models
                    .iter()
                    .find(|candidate| {
                        candidate.enabled && candidate.logical_model_id == logical_model_id
                    })
                    .cloned();
                let Some(model) = model else {
                    return error_response(
                        StatusCode::NOT_FOUND,
                        RouterError::UnknownModel(logical_model_id).to_string(),
                    );
                };
                (
                    RouteDescription {
                        logical_model_id,
                        route_class: RouteClass::Custom,
                        provider_id: Some(provider_id),
                        upstream_model_id: Some(upstream_model_id),
                        protocol: Some(protocol),
                        generation: registry.generation(),
                    },
                    Some(provider),
                    Some(model),
                )
            }
        }
    };

    if endpoint == OmniEndpoint::Compact
        && provider
            .as_ref()
            .is_some_and(|provider| provider.protocol == ProviderProtocol::ChatCompletions)
    {
        return error_response(
            StatusCode::NOT_IMPLEMENTED,
            RouterError::UnsupportedEndpoint("responses/compact for Chat-only provider".into())
                .to_string(),
        );
    }
    if endpoint == OmniEndpoint::ChatCompletions && route.route_class == RouteClass::Official {
        return error_response(
            StatusCode::BAD_REQUEST,
            RouterError::UnsupportedEndpoint("official Chat Completions entrypoint".into())
                .to_string(),
        );
    }

    let account_fingerprint = headers
        .get("chatgpt-account-id")
        .and_then(|value| value.to_str().ok())
        .map(fingerprint);
    let route_key = route_key(&route, provider.as_ref(), account_fingerprint.as_deref());
    let original_request = request.clone();
    if endpoint == OmniEndpoint::Responses
        && let Err(error) =
            hydrate_history_boundary(&state, &mut request, &route_key, provider.as_ref()).await
    {
        return bridge_error_response(error);
    }

    let (upstream_request, upstream_path, convert_chat_response) = match (
        endpoint,
        route.route_class.clone(),
        provider.as_ref().map(|p| p.protocol),
    ) {
        (OmniEndpoint::Responses, RouteClass::Official, _) => {
            if let Err(response) = validate_official_authorization(&headers) {
                return response;
            }
            (request.clone(), OmniEndpoint::Responses.path(), false)
        }
        (OmniEndpoint::Responses, RouteClass::Custom, Some(ProviderProtocol::Responses)) => {
            let Some(model) = model.as_ref() else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    RouterError::MissingModel.to_string(),
                );
            };
            let mut request = request.clone();
            request["model"] = Value::String(model.upstream_model_id.clone());
            (request, OmniEndpoint::Responses.path(), false)
        }
        (OmniEndpoint::Responses, RouteClass::Custom, Some(ProviderProtocol::ChatCompletions)) => {
            let Some(route_model) = model.as_ref() else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    RouterError::MissingModel.to_string(),
                );
            };
            let context = bridge_context(
                &route,
                route_model.upstream_model_id.clone(),
                account_fingerprint.clone(),
            );
            let reasoning_config = build_reasoning_config(provider.as_ref(), route_model);
            let converted =
                match responses_to_chat(request.clone(), &context, reasoning_config.as_ref()) {
                    Ok(converted) => converted,
                    Err(error) => return bridge_error_response(error),
                };
            (converted, OmniEndpoint::ChatCompletions.path(), true)
        }
        (
            OmniEndpoint::ChatCompletions,
            RouteClass::Custom,
            Some(ProviderProtocol::ChatCompletions),
        ) => {
            let Some(route_model) = model.as_ref() else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    RouterError::MissingModel.to_string(),
                );
            };
            let mut request = request.clone();
            request["model"] = Value::String(route_model.upstream_model_id.clone());
            (request, OmniEndpoint::ChatCompletions.path(), false)
        }
        (OmniEndpoint::ChatCompletions, RouteClass::Custom, Some(ProviderProtocol::Responses)) => {
            // A Chat-Completions *client* whose provider speaks the Responses API.
            //
            // This combination was previously attempted and produced the wrong
            // payload: the request was translated (chat_request_to_responses) but
            // the reply was returned verbatim, so the caller received a
            // `{"object":"response", "output":[...]}` body where it expected
            // `{"object":"chat.completion", "choices":[...]}`. A conforming client
            // sees a successful HTTP 200 with an unusable body — worse than a
            // clear error.
            //
            // The bridge only converts *toward* the Responses API (Codex's native
            // format); there is no Responses-response -> Chat-response converter.
            // Reject the combination explicitly instead of returning a body the
            // client cannot parse.
            return error_response(
                StatusCode::NOT_IMPLEMENTED,
                RouterError::UnsupportedEndpoint(
                    "a Chat Completions client cannot use a provider that speaks the \
                     Responses API; call /v1/responses instead, or give the provider a \
                     chat-completions endpoint"
                        .into(),
                )
                .to_string(),
            );
        }
        (special, RouteClass::Official, _) => {
            if let Err(response) = validate_official_authorization(&headers) {
                return response;
            }
            (request.clone(), special.path(), false)
        }
        (special, RouteClass::Custom, Some(ProviderProtocol::Responses))
        | (special, RouteClass::Custom, Some(ProviderProtocol::ChatCompletions)) => {
            let Some(route_model) = model.as_ref() else {
                return error_response(
                    StatusCode::NOT_FOUND,
                    RouterError::MissingModel.to_string(),
                );
            };
            let mut request = request.clone();
            request["model"] = Value::String(route_model.upstream_model_id.clone());
            (request, special.path(), false)
        }
        (_, _, None) => {
            return error_response(
                StatusCode::BAD_REQUEST,
                RouterError::MissingModel.to_string(),
            );
        }
    };

    let upstream_base = provider
        .as_ref()
        .map(|provider| provider.base_url.clone())
        .unwrap_or_else(|| state.official_base_url.clone());
    let upstream_url = match join_url(&upstream_base, upstream_path) {
        Ok(url) => url,
        Err(error) => return error_response(StatusCode::BAD_GATEWAY, error.to_string()),
    };
    debug!(logical_model = %logical_model_id, route = ?route.route_class, endpoint = endpoint.label(), "routing OmniBridge request");

    let mut builder = state
        .http_client
        .post(upstream_url)
        // Per-request deadline rather than a client-wide one: a client timeout
        // would cut off long streaming turns. This at least bounds how long a
        // hung upstream can pin a request task.
        .timeout(UPSTREAM_READ_TIMEOUT)
        .header(header::CONTENT_TYPE, "application/json");
    if let Some(accept) = headers.get(header::ACCEPT) {
        builder = builder.header(header::ACCEPT, accept.clone());
    }
    if route.route_class == RouteClass::Official {
        builder = apply_official_headers(builder, &headers);
    } else if let Some(provider) = provider.as_ref() {
        builder = match apply_custom_headers(builder, provider, &state, route.generation).await {
            Ok(builder) => builder,
            Err(error) => return error_response(StatusCode::BAD_GATEWAY, error.to_string()),
        };
    }
    let upstream = match builder.json(&upstream_request).send().await {
        Ok(response) => response,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                RouterError::Upstream(error.to_string()).to_string(),
            );
        }
    };
    let status = upstream.status();
    let content_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
    // Decide from what the upstream actually sent, not from the flag we asked
    // for. Plenty of OpenAI-compatible gateways ignore `"stream": true` and reply
    // with a single `application/json` body; piping those bytes into the Chat-SSE
    // state machine produced `response.failed` for a perfectly valid completion.
    let upstream_is_sse = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"));
    let requested_stream = upstream_request.get("stream").and_then(Value::as_bool) == Some(true);
    let upstream_is_json = content_type
        .as_ref()
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.to_ascii_lowercase().contains("application/json"));
    // An SSE body is streaming regardless of what we asked for; a JSON body is
    // not streaming even if we asked for it. Only when the upstream gave us no
    // usable content-type do we fall back to the request flag.
    let is_stream = if upstream_is_sse {
        true
    } else if upstream_is_json {
        false
    } else {
        requested_stream
    };

    if !status.is_success() {
        return response_from_upstream(status, content_type, upstream.bytes_stream());
    }

    if convert_chat_response && is_stream {
        let Some(route_model) = model.as_ref() else {
            return error_response(
                StatusCode::BAD_GATEWAY,
                RouterError::MissingModel.to_string(),
            );
        };
        let context = bridge_context(
            &route,
            route_model.upstream_model_id.clone(),
            account_fingerprint,
        );
        let input = upstream
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other));
        let converted = match chat_sse_to_responses_stream(input, context, original_request) {
            Ok(stream) => stream,
            Err(error) => return bridge_error_response(error),
        };
        let route_map = state.history_routes.clone();
        let route_key_for_stream = route_key.clone();
        let callback: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |response_id| {
            route_map
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .insert(response_id.to_owned(), route_key_for_stream.clone());
        });
        let converted = codex_mp_protocol_bridge::record_responses_sse_stream_with_callback(
            converted,
            state.history.clone(),
            callback,
        );
        let mut response = Response::new(Body::from_stream(converted));
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("text/event-stream"),
        );
        return response;
    }

    if is_stream {
        let stream = upstream
            .bytes_stream()
            .map(|result| result.map_err(std::io::Error::other));
        let stream: Pin<
            Box<dyn futures::Stream<Item = Result<bytes::Bytes, std::io::Error>> + Send>,
        > = if endpoint == OmniEndpoint::Responses {
            let route_map = state.history_routes.clone();
            let route_key_for_stream = route_key.clone();
            let callback: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |response_id| {
                route_map
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .insert(response_id.to_owned(), route_key_for_stream.clone());
            });
            Box::pin(
                codex_mp_protocol_bridge::record_responses_sse_stream_with_callback(
                    stream,
                    state.history.clone(),
                    callback,
                ),
            )
        } else {
            Box::pin(stream)
        };
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            content_type.unwrap_or_else(|| HeaderValue::from_static("text/event-stream")),
        );
        return response;
    }

    // Bounded read: `Response::json()` buffers the whole body with no limit, so a
    // buggy or hostile provider (or one that ignores `stream` and returns a huge
    // document) could grow this process without bound. `content-length` is
    // advisory, so the stream is also truncated defensively.
    let upstream_json: Value = match read_bounded_json(upstream).await {
        Ok(value) => value,
        Err(error) => return error_response(StatusCode::BAD_GATEWAY, error.to_string()),
    };
    let output = if convert_chat_response {
        let Some(route_model) = model.as_ref() else {
            return error_response(
                StatusCode::BAD_GATEWAY,
                RouterError::MissingModel.to_string(),
            );
        };
        let context = bridge_context(
            &route,
            route_model.upstream_model_id.clone(),
            account_fingerprint,
        );
        match chat_to_responses_with_request(upstream_json, &context, &original_request) {
            Ok(value) => value,
            Err(error) => return bridge_error_response(error),
        }
    } else {
        upstream_json
    };
    // Image and search replies are not conversations: they can never be continued
    // and carry no portable tool-call history. Recording them still cloned the
    // entire response body (base64 image data included) only for the history
    // store to discard it, and inserted a route entry for an id that can never be
    // used as a `previous_response_id`. This is a denylist rather than an
    // allowlist so a future conversation-bearing endpoint (e.g. `compact`) keeps
    // being recorded by default.
    let records_history = !matches!(
        endpoint,
        OmniEndpoint::Search | OmniEndpoint::ImagesGenerations | OmniEndpoint::ImagesEdits
    );
    if records_history {
        record_portable_response(&state.history, &output).await;
        state.remember_response_route(&output, &route_key);
    }
    let mut response = Json(output).into_response();
    *response.status_mut() = status;
    response
}

fn route_key(
    route: &RouteDescription,
    provider: Option<&ProviderConfig>,
    account_fingerprint: Option<&str>,
) -> String {
    // Deliberately excludes `route.generation`. The generation counts *every*
    // registry mutation, including ones that cannot change where a model routes
    // (renaming a model, editing its context window, editing a different
    // provider). Including it meant any such edit made every in-flight
    // conversation look like a route change, which forces the portable replay
    // path and strips `previous_response_id` — losing server-side continuation
    // and reasoning state, and hard-failing on hosted-tool output. Identity is
    // the provider endpoint plus the upstream model and wire protocol; a
    // generation bump alone does not move a request anywhere else.
    match provider {
        Some(provider) => format!(
            "custom:{}:{}:{}:{:?}",
            provider.id,
            provider.base_url,
            route.upstream_model_id.as_deref().unwrap_or_default(),
            route.protocol
        ),
        None => format!("official:{}", account_fingerprint.unwrap_or("unknown")),
    }
}

fn build_reasoning_config(
    provider: Option<&ProviderConfig>,
    model: &codex_mp_core::CustomModel,
) -> Option<CodexChatReasoningConfig> {
    let base_url = provider
        .map(|p| p.base_url.to_ascii_lowercase())
        .unwrap_or_default();
    let upstream_model = model.upstream_model_id.to_ascii_lowercase();
    let is_openrouter = base_url.contains("openrouter");
    let is_deepseek = base_url.contains("deepseek") || upstream_model.contains("deepseek");

    if is_openrouter {
        Some(CodexChatReasoningConfig {
            supports_thinking: Some(false),
            supports_effort: Some(true),
            thinking_param: Some("none".to_string()),
            effort_param: Some("reasoning.effort".to_string()),
            effort_value_mode: Some("openrouter".to_string()),
            output_format: Some("auto".to_string()),
            effort_levels: if model.reasoning_levels.is_empty() {
                None
            } else {
                Some(model.reasoning_levels.clone())
            },
        })
    } else if is_deepseek {
        Some(CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(true),
            thinking_param: Some("thinking".to_string()),
            effort_param: Some("reasoning_effort".to_string()),
            effort_value_mode: Some("deepseek".to_string()),
            output_format: Some("reasoning_content".to_string()),
            effort_levels: if model.reasoning_levels.is_empty() {
                None
            } else {
                Some(model.reasoning_levels.clone())
            },
        })
    } else if model.capabilities.reasoning {
        Some(CodexChatReasoningConfig {
            supports_thinking: Some(true),
            supports_effort: Some(true),
            thinking_param: Some("thinking".to_string()),
            effort_param: Some("reasoning_effort".to_string()),
            effort_value_mode: None,
            output_format: Some("auto".to_string()),
            effort_levels: if model.reasoning_levels.is_empty() {
                None
            } else {
                Some(model.reasoning_levels.clone())
            },
        })
    } else {
        None
    }
}

fn bridge_context(
    route: &RouteDescription,
    upstream_model: String,
    account_fingerprint: Option<String>,
) -> BridgeContext {
    BridgeContext {
        route_id: format!("{}:{}", route.logical_model_id, route.generation),
        generation: route.generation,
        domain: if route.route_class == RouteClass::Official {
            RouteDomain::Official
        } else {
            RouteDomain::Custom
        },
        account_fingerprint,
        upstream_model,
    }
}

async fn hydrate_history_boundary(
    state: &RouterState,
    request: &mut Value,
    route_key: &str,
    provider: Option<&ProviderConfig>,
) -> Result<(), BridgeError> {
    let previous_id = request
        .get("previous_response_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty());
    let previous_id_owned = previous_id.map(str::to_owned);
    let has_tool_output = request
        .get("input")
        .and_then(Value::as_array)
        .is_some_and(|items| {
            items.iter().any(|item| {
                matches!(
                    item.get("type").and_then(Value::as_str),
                    Some("function_call_output")
                        | Some("custom_tool_call_output")
                        | Some("tool_search_output")
                )
            })
        });
    let is_chat =
        provider.is_some_and(|provider| provider.protocol == ProviderProtocol::ChatCompletions);
    // The official ChatGPT backend understands `previous_response_id` natively and
    // is the only party that issued those ids, so an id missing from our local
    // ledger there just means "this router has not seen it" (a restart, or an
    // id issued before this process started). Hard-failing treated that as a
    // context-boundary violation and broke every subsequent turn of a live
    // session until the user started a new thread. Only a *custom* route needs
    // the local ledger: forwarding an official id to a third party would leak it
    // and the provider could not resolve it anyway.
    let is_official = provider.is_none();
    let previous_route = if let Some(id) = previous_id_owned.as_deref() {
        state.previous_route(id)
    } else {
        None
    };
    if previous_id_owned.is_some() && previous_route.is_none() && !is_official {
        return Err(BridgeError::ContextBoundary(
            "this request continues a previous turn (`previous_response_id`), but that \
                     turn is not in this router's history ledger, so the earlier context \
                     cannot be reconstructed. This happens after a router restart, or when \
                     the id came from a different route. Start a new thread to continue."
                .into(),
        ));
    }
    let cross_route = previous_route
        .as_deref()
        .is_some_and(|previous| previous != route_key);
    if is_chat || cross_route {
        let restored = state.history.enrich_request(request).await;
        if previous_id_owned.is_some() && restored == 0 && has_tool_output {
            return Err(BridgeError::ContextBoundary(
                "this request continues a previous turn (`previous_response_id`), but that \
                     turn is not in this router's history ledger, so the earlier context \
                     cannot be reconstructed. This happens after a router restart, or when \
                     the id came from a different route. Start a new thread to continue."
                    .into(),
            ));
        }
        *request = portable_request(request)?;
    }
    Ok(())
}

async fn apply_custom_headers(
    mut builder: reqwest::RequestBuilder,
    provider: &ProviderConfig,
    state: &RouterState,
    generation: u64,
) -> Result<reqwest::RequestBuilder, RouterError> {
    for (name, value) in &provider.static_headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| RouterError::InvalidHeader(name.clone()))?;
        let value = HeaderValue::from_str(value)
            .map_err(|_| RouterError::InvalidHeader(name.to_string()))?;
        builder = builder.header(name, value);
    }
    // The credential header shape is owned by `AuthStrategy` so the router and
    // provider model discovery cannot drift apart.
    if !matches!(provider.auth_strategy, AuthStrategy::None) {
        // Cached per registry generation. Resolution costs an OS keyring round
        // trip, and doing it on every request both added latency and let a slow
        // keyring daemon stall traffic. The call itself is offloaded to a blocking
        // thread: on Linux the keyring reaches the Secret Service through `zbus`,
        // which bridges to its synchronous API with
        // `tokio::runtime::Runtime::block_on`, and calling that from inside this
        // async handler panicked with "Cannot start a runtime from within a
        // runtime", killing the tokio worker (and poisoning the shared client) on
        // the very first request to a custom provider.
        let credential = state
            .resolve_credential(generation, &provider.credential_reference)
            .await?;
        if let Some((name, value)) = provider
            .auth_strategy
            .credential_header(credential.expose_secret())
        {
            let header_name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| RouterError::InvalidHeader(name.clone()))?;
            builder = builder.header(header_name, value);
        }
    }
    Ok(builder)
}

fn apply_official_headers(
    mut builder: reqwest::RequestBuilder,
    headers: &HeaderMap,
) -> reqwest::RequestBuilder {
    // Explicit official allowlist.  The independent capability header,
    // provider keys and arbitrary browser headers never cross this boundary.
    for name in ["authorization", "chatgpt-account-id"] {
        if let Some(value) = headers.get(name) {
            builder = builder.header(name, value.clone());
        }
    }
    for (name, value) in headers {
        if name.as_str().starts_with("x-codex-") && name.as_str() != CAPABILITY_HEADER {
            builder = builder.header(name, value.clone());
        }
    }
    builder
}

#[allow(clippy::result_large_err)]
fn validate_official_authorization(headers: &HeaderMap) -> Result<(), Response> {
    let Some(value) = headers.get(header::AUTHORIZATION) else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::MissingOfficialAuthorization.to_string(),
        ));
    };
    let Ok(value) = value.to_str() else {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::MissingOfficialAuthorization.to_string(),
        ));
    };
    if !value
        .get(..7)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("bearer "))
        || value[7..].trim().is_empty()
    {
        return Err(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::MissingOfficialAuthorization.to_string(),
        ));
    }
    Ok(())
}

fn response_from_upstream(
    status: reqwest::StatusCode,
    content_type: Option<HeaderValue>,
    stream: impl futures::Stream<Item = Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
) -> Response {
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() =
        StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
}

/// Passthrough for endpoints whose contract is not JSON (`/v1/images/edits`).
///
/// The body is forwarded byte-for-byte with its original `Content-Type`, because
/// multipart payloads cannot be parsed and re-serialised without corrupting the
/// boundary and the binary image parts. The route is resolved from an explicit
/// `model` **query parameter**, since the multipart body carries no reliable
/// model field; when absent, the request goes to the official backend exactly as
/// stock Codex would have sent it.
async fn forward_multipart_request(
    state: RouterState,
    headers: HeaderMap,
    uri: Option<axum::http::Uri>,
    body: Body,
) -> Response {
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .cloned()
        .unwrap_or_else(|| HeaderValue::from_static("multipart/form-data"));
    let is_multipart = content_type.to_str().is_ok_and(|value| {
        value
            .to_ascii_lowercase()
            .starts_with("multipart/form-data")
    });
    if !is_multipart {
        return error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            RouterError::UnsupportedContentType.to_string(),
        );
    }

    let bytes = match axum::body::to_bytes(body, MAX_REQUEST_BODY_BYTES).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                RouterError::RequestBodyTooLarge(MAX_REQUEST_BODY_BYTES).to_string(),
            );
        }
    };

    // `?model=<logical id>` selects a route. Without it we forward to the
    // official backend, which is the only route that can honour an image edit
    // today (no provider protocol here defines a multipart contract).
    let requested_model = uri
        .as_ref()
        .and_then(|uri| uri.query())
        .and_then(|query| {
            url::form_urlencoded::parse(query.as_bytes())
                .find(|(key, _)| key == "model")
                .map(|(_, value)| value.into_owned())
        })
        .filter(|value| !value.trim().is_empty());
    let (upstream_base, route) = match requested_model.as_deref() {
        Some(logical_model_id) => {
            let registry = state.registry.read().await;
            match registry.resolve_logical_model_route(logical_model_id) {
                Ok(LogicalModelRoute::Custom { provider_id, .. }) => {
                    let Some(provider) = registry
                        .provider(&provider_id)
                        .filter(|provider| provider.enabled)
                    else {
                        return error_response(
                            StatusCode::NOT_FOUND,
                            RouterError::UnknownProvider(provider_id).to_string(),
                        );
                    };
                    (provider.base_url.clone(), RouteClass::Custom)
                }
                Ok(LogicalModelRoute::Official { .. }) => {
                    (state.official_base_url.clone(), RouteClass::Official)
                }
                Err(error) => {
                    return error_response(StatusCode::NOT_FOUND, error.to_string());
                }
            }
        }
        None => (state.official_base_url.clone(), RouteClass::Official),
    };

    if route == RouteClass::Official
        && let Err(response) = validate_official_authorization(&headers)
    {
        return response;
    }

    let upstream_url = match join_url(&upstream_base, OmniEndpoint::ImagesEdits.path()) {
        Ok(url) => url,
        Err(error) => return error_response(StatusCode::BAD_GATEWAY, error.to_string()),
    };

    let mut builder = state
        .http_client
        .post(upstream_url)
        .timeout(UPSTREAM_READ_TIMEOUT)
        .header(header::CONTENT_TYPE, content_type)
        .body(bytes);
    if let Some(accept) = headers.get(header::ACCEPT) {
        builder = builder.header(header::ACCEPT, accept.clone());
    }
    if route == RouteClass::Official {
        builder = apply_official_headers(builder, &headers);
    }

    match builder.send().await {
        Ok(upstream) => {
            let status = upstream.status();
            let response_type = upstream.headers().get(header::CONTENT_TYPE).cloned();
            response_from_upstream(status, response_type, upstream.bytes_stream())
        }
        Err(error) => error_response(
            StatusCode::BAD_GATEWAY,
            RouterError::Upstream(error.to_string()).to_string(),
        ),
    }
}

/// Read an upstream JSON body with a hard size cap.
///
/// `reqwest::Response::json()` reads the entire body into memory with no limit.
/// This checks the advertised `content-length` first (cheap rejection) and then
/// enforces the cap on the actual byte stream, so a lying or absent length cannot
/// bypass it.
async fn read_bounded_json(upstream: reqwest::Response) -> Result<Value, RouterError> {
    if let Some(length) = upstream.content_length()
        && length > MAX_UPSTREAM_RESPONSE_BYTES as u64
    {
        return Err(RouterError::Upstream(format!(
            "upstream response of {length} bytes exceeds the {MAX_UPSTREAM_RESPONSE_BYTES} byte limit"
        )));
    }
    let mut collected: Vec<u8> = Vec::new();
    let mut stream = upstream.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|error| RouterError::Upstream(error.to_string()))?;
        if collected.len() + chunk.len() > MAX_UPSTREAM_RESPONSE_BYTES {
            return Err(RouterError::Upstream(format!(
                "upstream response exceeds the {MAX_UPSTREAM_RESPONSE_BYTES} byte limit"
            )));
        }
        collected.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&collected).map_err(RouterError::InvalidJson)
}

fn bridge_error_response(error: BridgeError) -> Response {
    let status = match error {
        BridgeError::InvalidResponse(_) | BridgeError::InvalidSse(_) => StatusCode::BAD_GATEWAY,
        BridgeError::ContextBoundary(_) | BridgeError::NotObject | BridgeError::Unsupported(_) => {
            StatusCode::BAD_REQUEST
        }
        BridgeError::MissingChoice | BridgeError::TransformError(_) => StatusCode::BAD_GATEWAY,
    };
    error_response(status, error.to_string())
}

fn fingerprint(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn file_sha256(path: &Path) -> Option<String> {
    let bytes = fs::read(path).ok()?;
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    Some(
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect(),
    )
}

fn require_json_content_type(headers: &HeaderMap) -> Option<Response> {
    let Some(value) = headers.get(header::CONTENT_TYPE) else {
        return Some(error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            RouterError::UnsupportedContentType.to_string(),
        ));
    };
    let Ok(value) = value.to_str() else {
        return Some(error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            RouterError::UnsupportedContentType.to_string(),
        ));
    };
    let media_type = value.split(';').next().map(str::trim).unwrap_or_default();
    if !media_type.eq_ignore_ascii_case("application/json") {
        return Some(error_response(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            RouterError::UnsupportedContentType.to_string(),
        ));
    }
    None
}

fn decode_request_body(headers: &HeaderMap, bytes: &[u8]) -> Result<Vec<u8>, RouterError> {
    let encoding = headers
        .get(header::CONTENT_ENCODING)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("identity")
        .to_ascii_lowercase();

    match encoding.as_str() {
        "identity" => Ok(bytes.to_vec()),
        "gzip" => decode_compressed(flate2::read::GzDecoder::new(Cursor::new(bytes)))
            .map_err(|error| RouterError::InvalidCompressedBody(error.to_string())),
        "deflate" => decode_compressed(flate2::read::DeflateDecoder::new(Cursor::new(bytes)))
            .map_err(|error| RouterError::InvalidCompressedBody(error.to_string())),
        "br" => decode_compressed(brotli::Decompressor::new(Cursor::new(bytes), 4096))
            .map_err(|error| RouterError::InvalidCompressedBody(error.to_string())),
        "zstd" => zstd::stream::read::Decoder::new(Cursor::new(bytes))
            .map_err(|error| RouterError::InvalidCompressedBody(error.to_string()))
            .and_then(|decoder| {
                decode_compressed(decoder)
                    .map_err(|error| RouterError::InvalidCompressedBody(error.to_string()))
            }),
        other => Err(RouterError::UnsupportedContentEncoding(other.to_owned())),
    }
}

fn decode_compressed<R: Read>(reader: R) -> Result<Vec<u8>, std::io::Error> {
    let mut decoded = Vec::new();
    reader
        .take((MAX_REQUEST_BODY_BYTES + 1) as u64)
        .read_to_end(&mut decoded)?;
    if decoded.len() > MAX_REQUEST_BODY_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            RouterError::RequestBodyTooLarge(MAX_REQUEST_BODY_BYTES).to_string(),
        ));
    }
    Ok(decoded)
}

fn authorize(state: &RouterState, headers: &HeaderMap) -> Option<Response> {
    if let Some(response) = validate_request_origin(headers) {
        return Some(response);
    }
    // Fail closed. A state without a capability token used to authorize every
    // request, so mounting `app(state)` directly exposed the whole admin and
    // inference surface; only `serve()` enforced the token.
    let Some(expected) = state.capability_token.as_deref() else {
        return Some(error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            RouterError::MissingCapabilityToken.to_string(),
        ));
    };
    let Some(value) = headers.get(CAPABILITY_HEADER) else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::Unauthorized.to_string(),
        ));
    };
    let Ok(value) = value.to_str() else {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::Unauthorized.to_string(),
        ));
    };
    if !constant_time_equal(value.as_bytes(), expected.expose_secret().as_bytes()) {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::Unauthorized.to_string(),
        ));
    }
    None
}

fn constant_time_equal(left: &[u8], right: &[u8]) -> bool {
    let length = left.len().max(right.len());
    let mut difference = (left.len() ^ right.len()) as u8;
    for index in 0..length {
        let left_byte = left.get(index).copied().unwrap_or(0);
        let right_byte = right.get(index).copied().unwrap_or(0);
        difference |= left_byte ^ right_byte;
    }
    difference == 0
}

fn validate_request_origin(headers: &HeaderMap) -> Option<Response> {
    let Some(host_header) = headers.get(header::HOST) else {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            RouterError::InvalidRequestOrigin.to_string(),
        ));
    };
    let Ok(host) = host_header.to_str() else {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            RouterError::InvalidRequestOrigin.to_string(),
        ));
    };
    if !is_loopback_authority(host) {
        return Some(error_response(
            StatusCode::BAD_REQUEST,
            RouterError::InvalidRequestOrigin.to_string(),
        ));
    }
    if let Some(origin) = headers.get(header::ORIGIN) {
        let Ok(origin) = origin.to_str() else {
            return Some(error_response(
                StatusCode::BAD_REQUEST,
                RouterError::InvalidRequestOrigin.to_string(),
            ));
        };
        let Ok(parsed) = Url::parse(origin) else {
            return Some(error_response(
                StatusCode::BAD_REQUEST,
                RouterError::InvalidRequestOrigin.to_string(),
            ));
        };
        let allowed_scheme = matches!(parsed.scheme(), "http" | "https" | "tauri");
        let allowed_host = parsed.host_str().is_some_and(is_loopback_host);
        if !allowed_scheme || !allowed_host {
            return Some(error_response(
                StatusCode::FORBIDDEN,
                RouterError::InvalidRequestOrigin.to_string(),
            ));
        }
    }
    None
}

fn is_loopback_authority(value: &str) -> bool {
    let Ok(parsed) = Url::parse(&format!("http://{value}")) else {
        return false;
    };
    let valid_path = parsed.path().is_empty() || parsed.path() == "/";
    parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && valid_path
        && parsed.host_str().is_some_and(is_loopback_host)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|address| address.is_loopback())
            .unwrap_or(false)
}

fn join_url(base: &str, path: &str) -> Result<Url, RouterError> {
    let mut base_url = Url::parse(base)
        .map_err(|error| RouterError::InvalidBaseUrl(base.to_owned(), error.to_string()))?;
    let base_path = base_url.path().trim_end_matches('/');
    let directory = if base_path.is_empty() {
        "/".to_owned()
    } else {
        format!("{base_path}/")
    };
    base_url.set_path(&directory);
    base_url
        .join(path.trim_start_matches('/'))
        .map_err(|error| RouterError::InvalidBaseUrl(base_url.to_string(), error.to_string()))
}

fn error_response(status: StatusCode, message: String) -> Response {
    let mut response = Json(json!({
        "error": {
            "type": "codex_multiprovider_error",
            "message": message,
        }
    }))
    .into_response();
    *response.status_mut() = status;
    response
}

fn remove_stale_endpoint(path: &Path) -> Result<(), std::io::Error> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_core::{CustomModel, ProviderConfig};
    use codex_mp_credentials::{CredentialStoreError, MemoryCredentialStore};
    use secrecy::SecretString;

    /// Counts `get` calls so a test can prove caching works.
    #[derive(Default)]
    struct CountingCredentialStore {
        reads: Arc<StdMutex<usize>>,
        values: Arc<StdMutex<HashMap<String, SecretString>>>,
    }

    impl CountingCredentialStore {
        fn with(reference: &str, secret: &str) -> Self {
            let store = Self::default();
            store
                .values
                .lock()
                .unwrap()
                .insert(reference.to_owned(), SecretString::from(secret.to_owned()));
            store
        }

        fn reads(&self) -> usize {
            *self.reads.lock().unwrap()
        }
    }

    impl CredentialStore for CountingCredentialStore {
        fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
            *self.reads.lock().unwrap() += 1;
            self.values
                .lock()
                .unwrap()
                .get(reference)
                .cloned()
                .ok_or_else(|| CredentialStoreError::NotFound(reference.to_owned()))
        }

        fn set(&self, reference: &str, value: &SecretString) -> Result<(), CredentialStoreError> {
            self.values
                .lock()
                .unwrap()
                .insert(reference.to_owned(), value.clone());
            Ok(())
        }

        fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
            self.values.lock().unwrap().remove(reference);
            Ok(())
        }
    }

    /// Regression: every custom-model request used to hit the OS keyring. A slow
    /// keyring daemon therefore stalled traffic even though the secret had not
    /// changed. The cache must serve repeats and still pick up a new generation.
    #[tokio::test]
    async fn provider_credentials_are_cached_per_registry_generation() {
        let store = Arc::new(CountingCredentialStore::with("provider:cache", "sk-cached"));
        let mut registry = ProviderRegistry::empty("/tmp/router-credential-cache-test.json");
        registry
            .add_provider(ProviderConfig::new("Cache", "https://example.test/v1").unwrap())
            .unwrap();
        let generation = registry.generation();
        let state = RouterState::new(registry, store.clone());

        let first = state
            .resolve_credential(generation, "provider:cache")
            .await
            .unwrap();
        assert_eq!(first.expose_secret(), "sk-cached");
        assert_eq!(store.reads(), 1);

        // Repeats inside the same generation must not touch the keyring again.
        for _ in 0..5 {
            let again = state
                .resolve_credential(generation, "provider:cache")
                .await
                .unwrap();
            assert_eq!(again.expose_secret(), "sk-cached");
        }
        assert_eq!(
            store.reads(),
            1,
            "credential lookups were not cached within a registry generation"
        );

        // A new generation (any registry mutation) must invalidate the cache so a
        // rotated credential takes effect immediately.
        let rotated = generation + 1;
        store.values.lock().unwrap().insert(
            "provider:cache".to_owned(),
            SecretString::from("sk-rotated".to_owned()),
        );
        let refreshed = state
            .resolve_credential(rotated, "provider:cache")
            .await
            .unwrap();
        assert_eq!(
            refreshed.expose_secret(),
            "sk-rotated",
            "a registry change must not serve a stale credential"
        );
        assert_eq!(store.reads(), 2);
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn zstd(bytes: &[u8]) -> Vec<u8> {
        zstd::stream::encode_all(Cursor::new(bytes), 1).unwrap()
    }

    fn brotli(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write;

        let mut encoded = Vec::new();
        {
            let mut encoder = brotli::CompressorWriter::new(&mut encoded, 4096, 5, 22);
            encoder.write_all(bytes).unwrap();
        }
        encoded
    }

    #[test]
    fn route_namespaced_models_without_touching_official_ids() {
        let mut registry = ProviderRegistry::empty("/tmp/providers.json");
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();
        registry.set_official_model_ids(["gpt-5.6-sol"]);

        // Exercises the same resolution the request path uses; the previous
        // `resolve_route` helper was a divergent copy that mislabelled unknown
        // models as custom.
        let custom = registry
            .resolve_logical_model_route("newapi/qwen3.8")
            .expect("custom model should resolve");
        assert!(matches!(custom, LogicalModelRoute::Custom { .. }));

        let official = registry
            .resolve_logical_model_route("gpt-5.6-sol")
            .expect("official model should resolve");
        assert!(matches!(official, LogicalModelRoute::Official { .. }));

        // An unknown model must be an explicit error, never a silent route.
        assert!(
            registry
                .resolve_logical_model_route("does-not-exist")
                .is_err()
        );
    }

    #[test]
    fn endpoint_file_is_private_loopback_and_loadable() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("router-endpoint.json");
        let token = SecretString::from("capability-secret");
        write_router_endpoint(
            &path,
            SocketAddr::new(Ipv4Addr::LOCALHOST.into(), 8787),
            &token,
        )
        .unwrap();
        let endpoint = load_router_endpoint(&path).unwrap();
        assert_eq!(endpoint.base_url, "http://127.0.0.1:8787");
        assert_eq!(
            endpoint.capability_token.expose_secret(),
            "capability-secret"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The endpoint file carries the capability token, which authorises
            // every Router endpoint. It must be 0600, and — because the previous
            // implementation wrote it with `fs::write` and chmodded afterwards —
            // no other file in the directory may have been created readable by
            // group/other at any point.
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let loose: Vec<String> = fs::read_dir(directory.path())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path() != path)
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect();
            assert!(
                loose.is_empty(),
                "a temporary file was left behind: {loose:?}"
            );
        }
    }

    /// Regression: a `TcpListener::bind` failure (port already in use, permission
    /// denied) was reported as "upstream request failed", which sent the operator
    /// looking at the third-party provider instead of at the local port. The
    /// message must name the address that could not be bound.
    #[tokio::test]
    async fn a_bind_failure_names_the_address_instead_of_blaming_upstream() {
        // `serve` refuses to run without a capability token, so supply one: the
        // bind is what this test is about.
        let state = RouterState::with_capability_token(
            ProviderRegistry::empty("/tmp/router-bind-error-test.json"),
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
            SecretString::from("tok".to_owned()),
        );
        // Hold the port, then ask the Router to bind the same one.
        let holder = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = holder.local_addr().unwrap();

        let config = RouterConfig {
            bind_ip: addr.ip(),
            port: addr.port(),
            endpoint_file: None,
        };
        let error = serve(config, state).await.expect_err("bind must fail");
        let message = error.to_string();
        assert!(
            message.contains("could not bind"),
            "the error must name the operation, got: {message}"
        );
        assert!(
            message.contains(&addr.port().to_string()),
            "the error must name the address, got: {message}"
        );
        assert!(
            !message.contains("upstream"),
            "a bind failure must not blame the upstream, got: {message}"
        );
    }

    #[test]
    fn router_config_rejects_non_loopback() {
        let config = RouterConfig {
            bind_ip: IpAddr::V4(Ipv4Addr::UNSPECIFIED),
            port: 1234,
            endpoint_file: None,
        };
        assert!(matches!(
            config.validate(),
            Err(RouterError::NonLoopbackBind(_))
        ));
    }

    #[test]
    fn state_can_use_memory_credentials_without_serializing_them() {
        let credentials = MemoryCredentialStore::default();
        credentials
            .set("provider:newapi", &SecretString::from("secret"))
            .unwrap();
        let state = RouterState::new(
            ProviderRegistry::empty("/tmp/providers.json"),
            Arc::new(credentials),
        );
        assert_eq!(Arc::strong_count(&state.credentials), 1);
    }

    #[derive(Debug, Default)]
    struct UpstreamCapture {
        path: Option<String>,
        authorization: Option<String>,
        api_key: Option<String>,
        capability: Option<String>,
        account: Option<String>,
        body: Option<Value>,
    }

    async fn mock_upstream(
        State(capture): State<Arc<std::sync::Mutex<UpstreamCapture>>>,
        uri: axum::http::Uri,
        headers: HeaderMap,
        body: axum::body::Bytes,
    ) -> Response {
        let body = serde_json::from_slice(&body).expect("router sends JSON upstream");
        let mut capture = capture.lock().expect("capture lock");
        capture.path = Some(uri.path().to_owned());
        capture.authorization = headers
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        capture.api_key = headers
            .get("x-api-key")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        capture.capability = headers
            .get(CAPABILITY_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        capture.account = headers
            .get("chatgpt-account-id")
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        capture.body = Some(body);
        if capture
            .body
            .as_ref()
            .and_then(|body| body.get("stream"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            let body = if uri.path().ends_with("/chat/completions") {
                "data: {\"id\":\"chat-id\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{\"role\":\"assistant\",\"content\":\"hello\"},\"finish_reason\":null}]}\n\ndata: {\"id\":\"chat-id\",\"object\":\"chat.completion.chunk\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}],\"usage\":{\"prompt_tokens\":1,\"completion_tokens\":1,\"total_tokens\":2}}\n\ndata: [DONE]\n\n"
            } else {
                "data: {\"ok\":true}\n\n"
            };
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "text/event-stream")
                .body(Body::from(body))
                .expect("valid mock response")
        } else {
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(
                    r#"{"id":"chat-id","object":"chat.completion","created":1,"model":"mock","choices":[{"index":0,"message":{"role":"assistant","content":"hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}"#,
                ))
                .expect("valid mock response")
        }
    }

    #[tokio::test]
    async fn forwards_custom_responses_request_with_provider_credentials_and_stream() {
        let capture = Arc::new(std::sync::Mutex::new(UpstreamCapture::default()));
        let upstream_app = Router::new()
            .fallback(mock_upstream)
            .with_state(capture.clone());
        let upstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });

        let mut registry = ProviderRegistry::empty("/tmp/router-test.json");
        let mut provider =
            ProviderConfig::new("NewAPI", &format!("http://{}/v1", upstream_addr)).unwrap();
        provider.credential_reference = "provider:newapi-test".into();
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();
        registry.set_official_model_ids(["gpt-5.6-sol"]);
        let credentials = MemoryCredentialStore::default();
        credentials
            .set(
                "provider:newapi-test",
                &SecretString::from("provider-secret"),
            )
            .unwrap();
        let router_app = app(RouterState::with_capability_token(
            registry,
            Arc::new(credentials),
            SecretString::from("core-capability"),
        )
        .with_official_base_url(format!("http://{}/v1", upstream_addr)));
        let router_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let router_addr = router_listener.local_addr().unwrap();
        let router_task = tokio::spawn(async move {
            let _ = axum::serve(router_listener, router_app).await;
        });

        let initial_official = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "core-capability")
            .header(header::AUTHORIZATION, "Bearer official-oauth")
            .header("chatgpt-account-id", "acct-canary")
            .json(&json!({"model": "gpt-5.6-sol", "input": "official-first"}))
            .send()
            .await
            .unwrap();
        assert_eq!(initial_official.status(), StatusCode::OK);

        let response = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "core-capability")
            .header(header::AUTHORIZATION, "Bearer official-oauth-canary")
            .header("chatgpt-account-id", "official-account-canary")
            .json(&json!({
                "model": "newapi/qwen3.8",
                "input": [{"type": "message", "role": "user", "content": [{"type": "input_text", "text": "hello"}]}],
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            b"data: {\"ok\":true}\n\n"
        );

        {
            let capture_guard = capture.lock().unwrap();
            assert_eq!(capture_guard.path.as_deref(), Some("/v1/responses"));
            assert_eq!(
                capture_guard.authorization.as_deref(),
                Some("Bearer provider-secret")
            );
            assert_eq!(capture_guard.capability, None);
            assert_eq!(capture_guard.account, None);
            assert_eq!(capture_guard.body.as_ref().unwrap()["model"], "qwen3.8");
            assert_eq!(capture_guard.body.as_ref().unwrap()["stream"], true);
        }

        let official = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "core-capability")
            .header(header::AUTHORIZATION, "Bearer official-oauth")
            .header("chatgpt-account-id", "acct-canary")
            .json(&json!({"model": "gpt-5.6-sol", "input": "hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(official.status(), StatusCode::OK);
        let capture_guard = capture.lock().unwrap();
        assert_eq!(
            capture_guard.authorization.as_deref(),
            Some("Bearer official-oauth")
        );
        assert_eq!(capture_guard.capability, None);
        assert_eq!(capture_guard.account.as_deref(), Some("acct-canary"));

        router_task.abort();
        upstream_task.abort();
    }

    /// A *custom* route must still fail closed: an id we cannot resolve locally
    /// cannot be forwarded to a third party.
    #[tokio::test]
    async fn unknown_previous_response_id_fails_closed_for_a_custom_route() {
        let mut registry = ProviderRegistry::empty("/tmp/router-history-boundary-test.json");
        registry.set_official_model_ids(["gpt-5.6-sol"]);
        let state = RouterState::new(registry, Arc::new(MemoryCredentialStore::default()));
        let mut request = json!({
            "model": "custom/model",
            "previous_response_id": "resp-unknown",
            "input": "continue"
        });
        let mut provider = ProviderConfig::new("Custom", "https://example.test/v1").unwrap();
        provider.protocol = ProviderProtocol::ChatCompletions;

        let error = hydrate_history_boundary(
            &state,
            &mut request,
            "custom:custom:https://example.test/v1:model:ChatCompletions",
            Some(&provider),
        )
        .await
        .expect_err("unknown response IDs must not cross a route boundary");
        assert!(matches!(error, BridgeError::ContextBoundary(_)));
        assert!(!error.to_string().contains("resp-unknown"));
    }

    /// Regression: an official id the local ledger has never seen (router
    /// restarted, or the id predates this process) must be passed through to the
    /// official backend, which issued it and can resolve it. Failing closed here
    /// broke every later turn of a live official conversation until the user
    /// started a new thread.
    #[tokio::test]
    async fn unknown_previous_response_id_passes_through_on_the_official_route() {
        let mut registry = ProviderRegistry::empty("/tmp/router-history-boundary-test.json");
        registry.set_official_model_ids(["gpt-5.6-sol"]);
        let state = RouterState::new(registry, Arc::new(MemoryCredentialStore::default()));
        let mut request = json!({
            "model": "gpt-5.6-sol",
            "previous_response_id": "resp-from-before-restart",
            "input": "continue"
        });

        hydrate_history_boundary(&state, &mut request, "official:acct", None)
            .await
            .expect("the official backend owns its own response ids");
        assert_eq!(
            request["previous_response_id"], "resp-from-before-restart",
            "the id must be forwarded unchanged"
        );
    }

    /// A gateway that ignores `"stream": true` and answers with one JSON body.
    /// The router used to pipe those bytes into the Chat-SSE state machine, which
    /// found no SSE frames and reported `response.failed` for a perfectly valid
    /// completion.
    async fn non_streaming_chat_upstream(
        State(capture): State<Arc<std::sync::Mutex<UpstreamCapture>>>,
        uri: axum::http::Uri,
        body: axum::body::Bytes,
    ) -> Response {
        let body = serde_json::from_slice(&body).expect("router sends JSON upstream");
        let mut capture = capture.lock().expect("capture lock");
        capture.path = Some(uri.path().to_owned());
        capture.body = Some(body);
        // Deliberately a JSON body even though `stream` was requested.
        Json(json!({
            "id": "chatcmpl_json",
            "object": "chat.completion",
            "model": "model-x",
            "choices": [{
                "index": 0,
                "message": {"role": "assistant", "content": "hello-from-json"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 1, "completion_tokens": 1, "total_tokens": 2}
        }))
        .into_response()
    }

    /// Regression: streaming is decided by what the upstream actually sent, so a
    /// provider that ignores `stream` still yields a valid Responses reply.
    #[tokio::test]
    async fn chat_provider_ignoring_stream_still_returns_a_completion() {
        let capture = Arc::new(std::sync::Mutex::new(UpstreamCapture::default()));
        let upstream_app = Router::new()
            .fallback(non_streaming_chat_upstream)
            .with_state(capture.clone());
        let upstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });

        let mut registry = ProviderRegistry::empty("/tmp/router-nostream-test.json");
        let mut provider =
            ProviderConfig::new("ChatAPI", &format!("http://{}/v1", upstream_addr)).unwrap();
        provider.protocol = ProviderProtocol::ChatCompletions;
        provider.auth_strategy = AuthStrategy::None;
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("chatapi", "model-x", "ChatAPI / model-x").unwrap())
            .unwrap();
        let router_app = app(RouterState::with_capability_token(
            registry,
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("test-capability"),
        ));
        let router_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let router_addr = router_listener.local_addr().unwrap();
        let router_task = tokio::spawn(async move {
            let _ = axum::serve(router_listener, router_app).await;
        });

        // Ask for a stream; the upstream answers with JSON.
        let response = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "test-capability")
            .json(&json!({
                "model": "chatapi/model-x",
                "input": "hello",
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "a JSON reply to a stream request must still succeed"
        );
        let body: Value = response.json().await.unwrap();
        assert_eq!(body["object"], "response");
        assert_eq!(
            body["output"][0]["content"][0]["text"], "hello-from-json",
            "the JSON completion must be converted, not failed: {body}"
        );
        assert_eq!(body["model"], "chatapi/model-x");

        router_task.abort();
        upstream_task.abort();
    }

    /// Regression: a Chat-Completions client pointed at a Responses-protocol
    /// provider was answered with the **wrong payload shape**. The request was
    /// translated, but the reply was returned verbatim, so the caller got
    /// `{"object":"response","output":[...]}` where a Chat client expects
    /// `{"object":"chat.completion","choices":[...]}`.
    ///
    /// A conforming client therefore saw HTTP 200 with an unparseable body —
    /// strictly worse than an error, because nothing signals the failure. The
    /// bridge has no Responses-response -> Chat-response converter (it only
    /// converts *toward* the Responses API), so the combination must be refused
    /// explicitly with guidance, keeping the supported `/v1/responses` path.
    #[tokio::test]
    async fn a_chat_client_cannot_use_a_responses_provider() {
        let upstream_app = Router::new()
            .fallback(mock_upstream)
            .with_state(Arc::new(std::sync::Mutex::new(UpstreamCapture::default())));
        let upstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });

        let mut registry = ProviderRegistry::empty("/tmp/router-chat-to-responses-test.json");
        let mut provider =
            ProviderConfig::new("RespAPI", &format!("http://{}/v1", upstream_addr)).unwrap();
        provider.protocol = ProviderProtocol::Responses;
        provider.auth_strategy = AuthStrategy::ApiKey;
        provider.credential_reference = "provider:resp-test".into();
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("respapi", "model-x", "RespAPI / model-x").unwrap())
            .unwrap();
        let credentials = MemoryCredentialStore::default();
        credentials
            .set("provider:resp-test", &SecretString::from("resp-secret"))
            .unwrap();

        let router_app = app(RouterState::with_capability_token(
            registry,
            Arc::new(credentials),
            SecretString::from("test-capability"),
        ));
        let router_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let router_addr = router_listener.local_addr().unwrap();
        let router_task = tokio::spawn(async move {
            let _ = axum::serve(router_listener, router_app).await;
        });

        // A Chat-Completions client must be refused, not handed a Responses body.
        let response = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/chat/completions"))
            .header(CAPABILITY_HEADER, "test-capability")
            .json(&json!({
                "model": "respapi/model-x",
                "messages": [{"role": "user", "content": "hello"}],
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "an unsupported protocol combination must be refused, not answered with the wrong shape"
        );
        let body: Value = response.json().await.unwrap();
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("/v1/responses"),
            "the error must point at the working endpoint, got: {message}"
        );

        // The refusal is specific to the *combination*: the same provider is
        // still reachable through its native endpoint (the shared mock upstream
        // answers both paths, so this only asserts routing, not the body shape).
        let response = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "test-capability")
            .json(&json!({
                "model": "respapi/model-x",
                "input": "hello",
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            StatusCode::NOT_IMPLEMENTED,
            "the Responses endpoint must remain usable for this provider"
        );

        router_task.abort();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn selects_chat_completions_upstream_for_chat_protocol() {
        let capture = Arc::new(std::sync::Mutex::new(UpstreamCapture::default()));
        let upstream_app = Router::new()
            .fallback(mock_upstream)
            .with_state(capture.clone());
        let upstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });

        let mut registry = ProviderRegistry::empty("/tmp/router-chat-test.json");
        let mut provider =
            ProviderConfig::new("ChatAPI", &format!("http://{}/v1", upstream_addr)).unwrap();
        provider.protocol = ProviderProtocol::ChatCompletions;
        provider.auth_strategy = AuthStrategy::ApiKey;
        provider.credential_reference = "provider:chat-test".into();
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("chatapi", "model-x", "ChatAPI / model-x").unwrap())
            .unwrap();
        let credentials = MemoryCredentialStore::default();
        credentials
            .set("provider:chat-test", &SecretString::from("chat-secret"))
            .unwrap();
        // `authorize()` fails closed without a token, so anything actually served
        // has to carry one; this also exercises the real header path.
        let router_app = app(RouterState::with_capability_token(
            registry,
            Arc::new(credentials),
            SecretString::from("test-capability"),
        ));
        let router_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let router_addr = router_listener.local_addr().unwrap();
        let router_task = tokio::spawn(async move {
            let _ = axum::serve(router_listener, router_app).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "test-capability")
            .json(&json!({
                "model": "chatapi/model-x",
                "input": "hello",
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        {
            let capture = capture.lock().unwrap();
            assert_eq!(capture.path.as_deref(), Some("/v1/chat/completions"));
            assert_eq!(capture.authorization, None);
            assert_eq!(capture.api_key.as_deref(), Some("chat-secret"));
            assert_eq!(capture.body.as_ref().unwrap()["model"], "model-x");
            assert_eq!(
                capture.body.as_ref().unwrap()["messages"][0]["content"],
                "hello"
            );
        }

        let response_body: Value = response.json().await.unwrap();
        assert_eq!(response_body["object"], "response");
        assert_eq!(response_body["output"][0]["content"][0]["text"], "hello");

        let stream = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(CAPABILITY_HEADER, "test-capability")
            .json(&json!({
                "model": "chatapi/model-x",
                "input": "hello",
                "stream": true
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(stream.status(), StatusCode::OK);
        let stream_body = stream.text().await.unwrap();
        assert!(stream_body.contains("response.created"));
        assert!(stream_body.contains("response.output_text.delta"));
        assert!(stream_body.contains("response.completed"));

        router_task.abort();
        upstream_task.abort();
    }

    #[tokio::test]
    async fn router_rejects_non_json_request_bodies() {
        let state = RouterState::with_capability_token(
            ProviderRegistry::empty("/tmp/providers.json"),
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1")
            .header(CAPABILITY_HEADER, "capability-secret")
            .header("content-type", "text/plain")
            .body(Body::from("{}"))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app(state), request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// An over-size body is a 413, not the 400 it used to report; clients treat
    /// 400 as "do not retry / your request is malformed", which is wrong here.
    #[tokio::test]
    async fn oversized_request_body_is_reported_as_payload_too_large() {
        let state = RouterState::with_capability_token(
            ProviderRegistry::empty("/tmp/providers.json"),
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );
        let oversized = "x".repeat(MAX_REQUEST_BODY_BYTES + 1024);
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1")
            .header(CAPABILITY_HEADER, "capability-secret")
            .header("content-type", "application/json")
            .body(Body::from(oversized))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app(state), request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    }

    /// An unknown `Content-Encoding` is a media-type problem (415), not a
    /// malformed body (400).
    #[tokio::test]
    async fn unsupported_content_encoding_is_reported_as_unsupported_media_type() {
        let state = RouterState::with_capability_token(
            ProviderRegistry::empty("/tmp/providers.json"),
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );
        let request = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1")
            .header(CAPABILITY_HEADER, "capability-secret")
            .header("content-type", "application/json")
            .header("content-encoding", "snappy")
            .body(Body::from("{}"))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app(state), request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    /// `/v1/images/edits` is `multipart/form-data` by contract. It used to be
    /// registered but unreachable: the global JSON requirement answered 415 before
    /// any provider was consulted.
    #[tokio::test]
    async fn multipart_image_edits_is_forwarded_instead_of_rejected() {
        let captured = Arc::new(StdMutex::new(None::<String>));
        let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let seen = captured.clone();
        tokio::spawn(async move {
            let app = Router::new().route(
                "/v1/images/edits",
                post(move |headers: HeaderMap, body: bytes::Bytes| {
                    let seen = seen.clone();
                    async move {
                        *seen.lock().unwrap() = Some(format!(
                            "{}|{}",
                            headers
                                .get(header::CONTENT_TYPE)
                                .and_then(|v| v.to_str().ok())
                                .unwrap_or_default(),
                            String::from_utf8_lossy(&body)
                        ));
                        Json(json!({"created": 1}))
                    }
                }),
            );
            let _ = axum::serve(listener, app).await;
        });

        let mut registry = ProviderRegistry::empty("/tmp/router-multipart-test.json");
        let mut provider = ProviderConfig::new("Img", &format!("http://{address}/v1")).unwrap();
        // No credential: this test is about body framing, not authentication.
        provider.auth_strategy = AuthStrategy::None;
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("img", "gpt-image-1", "Img / gpt-image-1").unwrap())
            .unwrap();
        let state = RouterState::with_capability_token(
            registry,
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );

        let boundary = "----omnibridge-test";
        let body = format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"prompt\"\r\n\r\nedit this\r\n--{boundary}--\r\n"
        );
        let request = axum::http::Request::builder()
            .method("POST")
            // `?model=` selects the custom route; without it the request would go
            // to the official backend, which needs an OAuth bearer.
            .uri("/v1/images/edits?model=img/gpt-image-1")
            .header("host", "127.0.0.1")
            .header(CAPABILITY_HEADER, "capability-secret")
            .header(
                "content-type",
                format!("multipart/form-data; boundary={boundary}"),
            )
            .body(Body::from(body.clone()))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app(state), request)
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::OK,
            "multipart image edits must reach the upstream, not be rejected as non-JSON"
        );
        let recorded = captured.lock().unwrap().clone().expect("upstream saw it");
        assert!(recorded.contains(&body), "body was not forwarded verbatim");
        assert!(recorded.starts_with("multipart/form-data"));
    }

    #[tokio::test]
    async fn router_rejects_external_host_and_origin_headers() {
        let state = RouterState::with_capability_token(
            ProviderRegistry::empty("/tmp/providers.json"),
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );
        let app = app(state);

        let external_host = axum::http::Request::builder()
            .method("GET")
            .uri("/healthz")
            .header("host", "attacker.example")
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), external_host)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let external_origin = axum::http::Request::builder()
            .method("GET")
            .uri("/healthz")
            .header("host", "127.0.0.1")
            .header("origin", "https://attacker.example")
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), external_origin)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);

        let loopback_origin = axum::http::Request::builder()
            .method("GET")
            .uri("/healthz")
            .header("host", "127.0.0.1")
            .header("origin", "tauri://localhost")
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, loopback_origin)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[tokio::test]
    async fn protected_router_rejects_missing_and_wrong_capability_tokens() {
        let registry = ProviderRegistry::empty("/tmp/providers.json");
        let state = RouterState::with_capability_token(
            registry,
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );
        let app = app(state);
        let missing = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"newapi/qwen3.8"}"#))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), missing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let wrong = axum::http::Request::builder()
            .method("POST")
            .uri("/v1/responses")
            .header("host", "127.0.0.1")
            .header(CAPABILITY_HEADER, "wrong")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"newapi/qwen3.8"}"#))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, wrong).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn accepts_stock_codex_compressed_json_requests() {
        let capture = Arc::new(std::sync::Mutex::new(UpstreamCapture::default()));
        let upstream_app = Router::new()
            .fallback(mock_upstream)
            .with_state(capture.clone());
        let upstream_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let upstream_addr = upstream_listener.local_addr().unwrap();
        let upstream_task = tokio::spawn(async move {
            let _ = axum::serve(upstream_listener, upstream_app).await;
        });

        let mut registry = ProviderRegistry::empty("/tmp/router-compressed-test.json");
        registry.set_official_model_ids(["gpt-5.6-sol"]);
        let state = RouterState::with_capability_token(
            registry,
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("compressed-capability"),
        )
        .with_official_base_url(format!("http://{upstream_addr}/v1"));
        let app = app(state);
        let payload = br#"{"model":"gpt-5.6-sol","input":"compressed","stream":false}"#;

        for (encoding, body) in [
            ("gzip", gzip(payload)),
            ("br", brotli(payload)),
            ("zstd", zstd(payload)),
        ] {
            let request = axum::http::Request::builder()
                .method("POST")
                .uri("/v1/responses")
                .header("host", "127.0.0.1")
                .header(CAPABILITY_HEADER, "compressed-capability")
                .header("authorization", "Bearer mock-official")
                .header("content-type", "application/json")
                .header("content-encoding", encoding)
                .body(Body::from(body))
                .unwrap();
            let response = tower::ServiceExt::oneshot(app.clone(), request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "encoding={encoding}");
        }
        let capture = capture.lock().unwrap();
        assert_eq!(capture.body.as_ref().unwrap()["model"], "gpt-5.6-sol");
        upstream_task.abort();
    }

    /// Regression: an unknown `previous_response_id` on a custom route produced
    /// `previous response is not available in the route-aware history ledger` —
    /// accurate but not actionable. A user hitting this after a router restart
    /// had no idea the fix is simply to start a new thread.
    #[tokio::test]
    async fn an_unknown_previous_response_id_explains_how_to_recover() {
        let mut registry = ProviderRegistry::empty("/tmp/does-not-matter.json");
        registry
            .add_provider(ProviderConfig {
                id: "up".into(),
                name: "Up".into(),
                base_url: "https://example.test/v1".into(),
                protocol: ProviderProtocol::Responses,
                auth_strategy: AuthStrategy::Bearer,
                static_headers: std::collections::BTreeMap::new(),
                credential_reference: "provider:up".into(),
                model_discovery: false,
                enabled: true,
                models: Vec::new(),
            })
            .unwrap();
        registry
            .add_model(CustomModel::new("up", "m1", "Up / m1").unwrap())
            .unwrap();
        let state = RouterState::new(registry, Arc::new(MemoryCredentialStore::default()));
        let provider = state
            .registry
            .read()
            .await
            .provider("up")
            .cloned()
            .expect("provider present");
        let mut request = json!({
            "model": "up/m1",
            "previous_response_id": "resp-never-issued",
            "input": "continue"
        });

        let error = hydrate_history_boundary(&state, &mut request, "up:m1", Some(&provider))
            .await
            .expect_err("an unknown id on a custom route must be rejected");
        let message = error.to_string();
        assert!(
            message.contains("Start a new thread"),
            "the error must tell the user how to recover, got: {message}"
        );
        assert!(
            message.contains("restart"),
            "the error must name the likely cause, got: {message}"
        );
    }

    #[tokio::test]
    async fn admin_shutdown_requires_capability_token() {
        let state = RouterState::with_capability_token(
            ProviderRegistry::empty("/tmp/providers.json"),
            Arc::new(MemoryCredentialStore::default()),
            SecretString::from("capability-secret"),
        );
        let app = app(state);
        let missing = axum::http::Request::builder()
            .method("POST")
            .uri("/admin/shutdown")
            .header("host", "127.0.0.1")
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app.clone(), missing)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);

        let authorized = axum::http::Request::builder()
            .method("POST")
            .uri("/admin/shutdown")
            .header("host", "127.0.0.1")
            .header(CAPABILITY_HEADER, "capability-secret")
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, authorized).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
