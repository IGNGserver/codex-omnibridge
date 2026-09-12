//! Local, loopback-only model router.
//!
//! Custom model requests are routed by namespaced logical id. Official model
//! ids are deliberately not rewritten or sent through a third-party adapter;
//! the Codex Core integration must keep its official backend path for those
//! turns. This crate returns an explicit 501 for an official id when no
//! official passthrough target has been supplied, rather than silently
//! misrouting ChatGPT traffic.

use std::fs;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codex_mp_core::{
    LogicalModelRoute, ProviderProtocol, ProviderRegistry, atomic_replace, default_registry_path,
    set_private_permissions,
};
use codex_mp_credentials::CredentialStore;
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::sync::{Notify, RwLock};
use tracing::debug;
use url::Url;
use uuid::Uuid;

pub mod compatibility;

pub const DEFAULT_BIND_IP: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

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

#[derive(Clone)]
pub struct RouterState {
    pub registry: Arc<RwLock<ProviderRegistry>>,
    pub credentials: Arc<dyn CredentialStore>,
    pub http_client: Client,
    capability_token: Option<Arc<SecretString>>,
    shutdown: Arc<Notify>,
}

impl RouterState {
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
            http_client: Client::new(),
            capability_token: capability_token.map(Arc::new),
            shutdown: Arc::new(Notify::new()),
        }
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
    #[error("official model traffic is not handled by the third-party adapter")]
    OfficialPassthroughRequired,
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
    let metadata =
        fs::metadata(path).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
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
    let temp = path.with_extension("json.tmp");
    let file = RouterEndpointFile {
        schema_version: ROUTER_ENDPOINT_SCHEMA_VERSION,
        base_url: format!("http://{addr}"),
        capability_token: capability_token.expose_secret().to_owned(),
    };
    fs::write(&temp, serde_json::to_vec_pretty(&file).unwrap())
        .map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    set_private_permissions(&temp).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
    atomic_replace(temp, path).map_err(|error| RouterError::EndpointFile(error.to_string()))?;
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
pub struct RouteDescription {
    pub logical_model_id: String,
    pub provider_id: Option<String>,
    pub upstream_model_id: Option<String>,
    pub protocol: Option<ProviderProtocol>,
}

pub fn resolve_route(registry: &ProviderRegistry, logical_model_id: &str) -> RouteDescription {
    match registry.resolve_logical_model_route(logical_model_id) {
        Ok(LogicalModelRoute::Custom {
            provider_id,
            upstream_model_id,
            protocol,
            ..
        }) => RouteDescription {
            logical_model_id: logical_model_id.to_owned(),
            provider_id: Some(provider_id),
            upstream_model_id: Some(upstream_model_id),
            protocol: Some(protocol),
        },
        _ => RouteDescription {
            logical_model_id: logical_model_id.to_owned(),
            provider_id: None,
            upstream_model_id: None,
            protocol: None,
        },
    }
}

pub fn app(state: RouterState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/v1/models", get(models))
        .route("/models", get(models))
        .route("/v1/responses", post(responses))
        .route("/responses", post(responses))
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
    let listener = tokio::net::TcpListener::bind(config.socket_addr())
        .await
        .map_err(|error| RouterError::Upstream(error.to_string()))?;
    let endpoint_file = config.endpoint_file.clone();
    if let Some(endpoint_file) = endpoint_file.as_ref() {
        let addr = listener
            .local_addr()
            .map_err(|error| RouterError::Upstream(error.to_string()))?;
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
    let data: Vec<Value> = registry
        .enabled_custom_models()
        .map(|(provider, model)| {
            json!({
                "id": model.logical_model_id,
                "object": "model",
                "owned_by": provider.name,
                "display_name": model.display_name,
                "capabilities": model.capabilities,
            })
        })
        .collect();
    Json(json!({"object": "list", "data": data})).into_response()
}

async fn responses(State(state): State<RouterState>, headers: HeaderMap, body: Body) -> Response {
    forward_request(state, headers, body, Endpoint::Responses).await
}

async fn chat_completions(
    State(state): State<RouterState>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    forward_request(state, headers, body, Endpoint::ChatCompletions).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Endpoint {
    Responses,
    ChatCompletions,
}

async fn forward_request(
    state: RouterState,
    headers: HeaderMap,
    body: Body,
    endpoint: Endpoint,
) -> Response {
    if let Some(response) = authorize(&state, &headers) {
        return response;
    }
    if let Some(response) = require_json_content_type(&headers) {
        return response;
    }
    let bytes = match axum::body::to_bytes(body, 16 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.to_string()),
    };
    let mut request: Value = match serde_json::from_slice(&bytes) {
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
    let registry = state.registry.read().await;
    let route = resolve_route(&registry, &logical_model_id);
    let Some(provider_id) = route.provider_id.as_deref() else {
        if !logical_model_id.contains('/') {
            return error_response(
                StatusCode::NOT_IMPLEMENTED,
                RouterError::OfficialPassthroughRequired.to_string(),
            );
        }
        return error_response(
            StatusCode::NOT_FOUND,
            RouterError::UnknownModel(logical_model_id).to_string(),
        );
    };
    let provider = registry
        .provider(provider_id)
        .expect("route provider was resolved from the same registry lock");
    let model = provider
        .models
        .iter()
        .find(|candidate| candidate.logical_model_id == route.logical_model_id && candidate.enabled)
        .expect("route model was resolved from the same registry lock");
    if !provider.enabled {
        return error_response(
            StatusCode::NOT_FOUND,
            RouterError::UnknownProvider(provider.id.clone()).to_string(),
        );
    }
    let provider = provider.clone();
    let model = model.clone();
    drop(registry);

    let credential = match state.credentials.get(&provider.credential_reference) {
        Ok(secret) => secret,
        Err(error) => {
            return error_response(
                StatusCode::BAD_GATEWAY,
                RouterError::Credential(error.to_string()).to_string(),
            );
        }
    };
    let (transformed, upstream_path) = match (endpoint, provider.protocol) {
        (Endpoint::Responses, ProviderProtocol::Responses) => (
            compatibility::prepare_responses_request(&mut request, &model),
            "responses",
        ),
        (Endpoint::Responses, ProviderProtocol::ChatCompletions) => (
            compatibility::responses_to_chat_request(&request, &model),
            "chat/completions",
        ),
        (Endpoint::ChatCompletions, ProviderProtocol::Responses) => (
            compatibility::chat_to_responses_request(&request, &model),
            "responses",
        ),
        (Endpoint::ChatCompletions, ProviderProtocol::ChatCompletions) => (
            compatibility::prepare_chat_request(&request, &model),
            "chat/completions",
        ),
    };
    let transformed = match transformed {
        Ok(request) => request,
        Err(error) => return error_response(StatusCode::BAD_REQUEST, error.to_string()),
    };
    let upstream_url = match join_url(&provider.base_url, upstream_path) {
        Ok(url) => url,
        Err(error) => return error_response(StatusCode::BAD_GATEWAY, error.to_string()),
    };
    debug!(logical_model = %logical_model_id, provider = %provider.id, "routing custom model request");

    let mut builder = state
        .http_client
        .post(upstream_url)
        .header(
            header::AUTHORIZATION,
            format!(
                "Bearer {}",
                secrecy::ExposeSecret::expose_secret(&credential)
            ),
        )
        .header(header::CONTENT_TYPE, "application/json");
    if headers.get(header::ACCEPT).is_some() {
        builder = builder.header(
            header::ACCEPT,
            headers.get(header::ACCEPT).expect("checked"),
        );
    }
    let upstream = match builder.json(&transformed).send().await {
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
    let is_chat_to_responses_stream = endpoint == Endpoint::Responses
        && provider.protocol == ProviderProtocol::ChatCompletions
        && transformed.get("stream").and_then(Value::as_bool) == Some(true);

    if is_chat_to_responses_stream && status.is_success() {
        use bytes::Bytes;
        use futures::StreamExt;
        use tokio_util::codec::{FramedRead, LinesCodec};
        use tokio_util::io::StreamReader;

        let byte_stream = upstream
            .bytes_stream()
            .map(|res| res.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
        let reader = StreamReader::new(byte_stream);
        let lines = FramedRead::new(reader, LinesCodec::new());

        struct StreamState {
            response_id: String,
            item_id: String,
            started: bool,
            accumulated: String,
        }
        let mut stream_state = StreamState {
            response_id: "resp_custom".into(),
            item_id: "msg_custom".into(),
            started: false,
            accumulated: String::new(),
        };

        let sse_stream = async_stream::stream! {
            let mut lines = lines;
            while let Some(line_res) = lines.next().await {
                let Ok(line) = line_res else { break; };
                let trimmed = line.trim();
                if trimmed.is_empty() || !trimmed.starts_with("data:") {
                    continue;
                }
                let data = trimmed[5..].trim();
                if data.is_empty() {
                    continue;
                }
                if data == "[DONE]" {
                    if stream_state.started {
                        let item_done = json!({
                            "type": "response.output_item.done",
                            "item": {
                                "id": stream_state.item_id,
                                "type": "message",
                                "role": "assistant",
                                "content": [{"type": "output_text", "text": stream_state.accumulated}]
                            }
                        });
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.output_item.done\ndata: {item_done}\n\n")));
                    }
                    let completed = json!({
                        "type": "response.completed",
                        "response": {
                            "id": stream_state.response_id,
                            "status": "completed"
                        }
                    });
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.completed\ndata: {completed}\n\n")));
                    break;
                }

                let Ok(chunk) = serde_json::from_str::<Value>(data) else { continue; };
                if let Some(id) = chunk.get("id").and_then(Value::as_str) {
                    stream_state.response_id = id.to_owned();
                    stream_state.item_id = format!("msg_{id}");
                }

                let Some(choices) = chunk.get("choices").and_then(Value::as_array) else { continue; };
                if choices.is_empty() { continue; };
                let delta = choices[0].get("delta").and_then(Value::as_object);
                let Some(delta) = delta else { continue; };

                if !stream_state.started {
                    stream_state.started = true;
                    let created = json!({
                        "type": "response.created",
                        "response": {"id": stream_state.response_id}
                    });
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.created\ndata: {created}\n\n")));
                    let added = json!({
                        "type": "response.output_item.added",
                        "item": {
                            "id": stream_state.item_id,
                            "type": "message",
                            "role": "assistant",
                            "content": []
                        }
                    });
                    yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.output_item.added\ndata: {added}\n\n")));
                }

                if let Some(reasoning) = delta.get("reasoning_content").and_then(Value::as_str) {
                    if !reasoning.is_empty() {
                        let reasoning_ev = json!({
                            "type": "response.reasoning_summary_text.delta",
                            "delta": reasoning,
                            "summary_index": 0
                        });
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.reasoning_summary_text.delta\ndata: {reasoning_ev}\n\n")));
                    }
                }

                if let Some(content) = delta.get("content").and_then(Value::as_str) {
                    if !content.is_empty() {
                        stream_state.accumulated.push_str(content);
                        let text_delta = json!({
                            "type": "response.output_text.delta",
                            "delta": content
                        });
                        yield Ok::<Bytes, std::io::Error>(Bytes::from(format!("event: response.output_text.delta\ndata: {text_delta}\n\n")));
                    }
                }
            }
        };

        let mut response = Response::new(Body::from_stream(sse_stream));
        *response.status_mut() = StatusCode::OK;
        response.headers_mut().insert(header::CONTENT_TYPE, header::HeaderValue::from_static("text/event-stream"));
        return response;
    }

    let stream = upstream.bytes_stream();
    let mut response = Response::new(Body::from_stream(stream));
    *response.status_mut() = status;
    if let Some(content_type) = content_type {
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, content_type);
    }
    response
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

fn authorize(state: &RouterState, headers: &HeaderMap) -> Option<Response> {
    if let Some(response) = validate_request_origin(headers) {
        return Some(response);
    }
    let expected = state.capability_token.as_deref()?;
    let Some(value) = headers.get(header::AUTHORIZATION) else {
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
    let expected = format!("Bearer {}", expected.expose_secret());
    if value != expected {
        return Some(error_response(
            StatusCode::UNAUTHORIZED,
            RouterError::Unauthorized.to_string(),
        ));
    }
    None
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
    use codex_mp_credentials::MemoryCredentialStore;
    use secrecy::SecretString;

    #[test]
    fn route_namespaced_models_without_touching_official_ids() {
        let mut registry = ProviderRegistry::empty("/tmp/providers.json");
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();
        assert!(
            resolve_route(&registry, "newapi/qwen3.8")
                .provider_id
                .is_some()
        );
        assert!(
            resolve_route(&registry, "gpt-5.6-sol")
                .provider_id
                .is_none()
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
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
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
        capture.body = Some(body);
        Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "text/event-stream")
            .body(Body::from("data: {\"ok\":true}\n\n"))
            .expect("valid mock response")
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
            .header(header::AUTHORIZATION, "Bearer core-capability")
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
            assert_eq!(capture_guard.body.as_ref().unwrap()["model"], "qwen3.8");
            assert_eq!(capture_guard.body.as_ref().unwrap()["stream"], true);
        }

        let official = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .header(header::AUTHORIZATION, "Bearer core-capability")
            .json(&json!({"model": "gpt-5.6-sol", "input": "hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(official.status(), StatusCode::NOT_IMPLEMENTED);
        assert_eq!(capture_path(&capture), Some("/v1/responses".to_owned()));

        router_task.abort();
        upstream_task.abort();
    }

    fn capture_path(capture: &Arc<std::sync::Mutex<UpstreamCapture>>) -> Option<String> {
        capture.lock().unwrap().path.clone()
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
        provider.credential_reference = "provider:chat-test".into();
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("chatapi", "model-x", "ChatAPI / model-x").unwrap())
            .unwrap();
        let credentials = MemoryCredentialStore::default();
        credentials
            .set("provider:chat-test", &SecretString::from("chat-secret"))
            .unwrap();
        let router_app = app(RouterState::new(registry, Arc::new(credentials)));
        let router_listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let router_addr = router_listener.local_addr().unwrap();
        let router_task = tokio::spawn(async move {
            let _ = axum::serve(router_listener, router_app).await;
        });

        let response = reqwest::Client::new()
            .post(format!("http://{router_addr}/v1/responses"))
            .json(&json!({
                "model": "chatapi/model-x",
                "input": "hello",
                "stream": false
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let capture = capture.lock().unwrap();
        assert_eq!(capture.path.as_deref(), Some("/v1/chat/completions"));
        assert_eq!(capture.authorization.as_deref(), Some("Bearer chat-secret"));
        assert_eq!(capture.body.as_ref().unwrap()["model"], "model-x");
        assert_eq!(
            capture.body.as_ref().unwrap()["messages"][0]["content"],
            "hello"
        );

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
            .header("authorization", "Bearer capability-secret")
            .header("content-type", "text/plain")
            .body(Body::from("{}"))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app(state), request)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNSUPPORTED_MEDIA_TYPE);
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
            .header("authorization", "Bearer wrong")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"model":"newapi/qwen3.8"}"#))
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, wrong).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
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
            .header("authorization", "Bearer capability-secret")
            .body(Body::empty())
            .unwrap();
        let response = tower::ServiceExt::oneshot(app, authorized).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
