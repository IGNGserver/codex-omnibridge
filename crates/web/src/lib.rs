//! Web interface, static asset embedding, REST API and access control for Codex MultiProvider.

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codex_mp_core::{
    ModelCapabilities, ModelEdit, ProviderProtocol, ProviderRegistry, resolve_executable,
};
use codex_mp_desktop::{DesktopInstallOptions, DesktopPaths, status_for};
use codex_mp_integration::{IntegrationPaths, build_and_install};
use codex_mp_manager::{
    AccountManager, AccountTokens, DiscoveredModel, ProviderManager, RouterSupervisor,
};
use codex_mp_router::default_router_endpoint_path;
use rust_embed::RustEmbed;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock};
use tower_http::cors::CorsLayer;
use uuid::Uuid;

#[derive(RustEmbed)]
#[folder = "../../apps/panel"]
struct PanelAssets;

#[derive(Debug, Error)]
pub enum WebError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unauthorized: {0}")]
    Unauthorized(String),
    #[error("remote access not allowed without password")]
    RemoteNotAllowedWithoutPassword,
    #[error("internal error: {0}")]
    Internal(String),
}

/// PBKDF2-like salted SHA-256 password hash helper
pub fn hash_password(password: &str) -> String {
    let salt = Uuid::new_v4().simple().to_string();
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    let hash: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    format!("{salt}${hash}")
}

pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    let Some((salt, expected_hash)) = stored_hash.split_once('$') else {
        return false;
    };
    let mut hasher = Sha256::new();
    hasher.update(salt.as_bytes());
    hasher.update(b":");
    hasher.update(password.as_bytes());
    let calculated: String = hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    constant_time_equal(calculated.as_bytes(), expected_hash.as_bytes())
}

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

#[derive(Clone)]
pub struct WebState {
    pub providers: Arc<ProviderManager>,
    pub supervisor: Arc<Mutex<RouterSupervisor>>,
    pub accounts: Arc<AccountManager>,
    pub codex_binary: PathBuf,
    pub sessions: Arc<RwLock<HashSet<String>>>,
    pub listen_addr: Arc<RwLock<SocketAddr>>,
    /// Secret loopback token for desktop/local IPC direct access without password
    pub local_token: Arc<String>,
}

impl WebState {
    pub fn new(
        registry_path: PathBuf,
        endpoint_file: PathBuf,
        router_bin: PathBuf,
        codex_bin: PathBuf,
    ) -> Self {
        Self::with_local_token(
            registry_path,
            endpoint_file,
            router_bin,
            codex_bin,
            Uuid::new_v4().to_string(),
        )
    }

    pub fn with_local_token(
        registry_path: PathBuf,
        endpoint_file: PathBuf,
        router_bin: PathBuf,
        codex_bin: PathBuf,
        local_token: String,
    ) -> Self {
        let providers = Arc::new(ProviderManager::new(registry_path.clone()));
        let supervisor = Arc::new(Mutex::new(RouterSupervisor::new(
            router_bin,
            registry_path,
            endpoint_file,
        )));
        let accounts = Arc::new(AccountManager::new().unwrap_or_else(|_| {
            let store = codex_mp_manager::default_accounts_path();
            let home =
                codex_mp_manager::default_codex_home().unwrap_or_else(|| PathBuf::from(".codex"));
            AccountManager::with_paths(store, home)
        }));
        Self {
            providers,
            supervisor,
            accounts,
            codex_binary: codex_bin,
            sessions: Arc::new(RwLock::new(HashSet::new())),
            listen_addr: Arc::new(RwLock::new(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                codex_mp_core::DEFAULT_WEB_PORT,
            ))),
            local_token: Arc::new(local_token),
        }
    }

    pub fn registry_path(&self) -> &Path {
        self.providers.registry_path()
    }

    pub fn catalog_binary(&self) -> Result<PathBuf, String> {
        if self.codex_binary != Path::new("codex") {
            return Ok(self.codex_binary.clone());
        }
        let Ok(paths) = DesktopPaths::discover() else {
            return Ok(self.codex_binary.clone());
        };
        let manifest = DesktopPaths::manifest_path_for_registry(self.registry_path());
        if manifest.exists() {
            return paths.catalog_binary(&manifest).map_err(|e| e.to_string());
        }
        Ok(paths.entrypoint)
    }
}

pub fn create_web_router(state: WebState) -> Router {
    let api_router = Router::new()
        // 状态相关
        .route("/router/status", get(api_router_status))
        .route("/desktop/status", get(api_desktop_status))
        .route("/desktop/install", post(api_desktop_install))
        .route("/desktop/restore", post(api_desktop_restore))
        // Catalog 同步
        .route("/catalog/sync", post(api_catalog_sync))
        // Provider 管理
        .route("/providers", get(api_list_providers))
        .route("/providers/add", post(api_add_provider))
        .route("/providers/edit", post(api_edit_provider))
        .route("/providers/remove", post(api_remove_provider))
        .route("/providers/{id}/discover", get(api_discover_models))
        // Model 管理
        .route("/models/add", post(api_add_model))
        .route("/models/import", post(api_import_models))
        .route("/models/edit", post(api_edit_model))
        .route("/models/enabled", post(api_set_model_enabled))
        .route("/models/remove", post(api_remove_model))
        // 安全控制
        .route("/security/update", post(api_security_update))
        // 官方账号与额度管理
        .route("/accounts", get(api_list_accounts))
        .route("/accounts/active", get(api_active_account))
        .route("/accounts/capture", post(api_capture_account))
        .route("/accounts/import", post(api_import_account))
        .route("/accounts/switch", post(api_switch_account))
        .route("/accounts/rename", post(api_rename_account))
        .route("/accounts/delete", post(api_delete_account))
        .route("/accounts/restart-codex", post(api_restart_codex))
        .route("/accounts/{id}/usage", get(api_fetch_account_usage))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // 无需鉴权的接口（登录和静态资源）
    Router::new()
        .route("/api/v1/security/status", get(api_security_status))
        .route("/api/v1/security/login", post(api_security_login))
        .nest("/api/v1", api_router)
        .fallback(static_handler)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

// -------------------------------- Auth 中间件 -------------------------------- //

async fn auth_middleware(
    State(state): State<WebState>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let registry = match ProviderRegistry::load(state.registry_path()) {
        Ok(reg) => reg,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err.to_string()})),
            )
                .into_response();
        }
    };
    let sec = registry.web_security();

    // 检查是否有 Authorization 标头
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let custom_token_header = headers.get("x-local-token").and_then(|v| v.to_str().ok());

    let token = if let Some(auth) = auth_header {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            Some(token.trim().to_string())
        } else {
            None
        }
    } else if let Some(custom) = custom_token_header {
        Some(custom.trim().to_string())
    } else {
        None
    };

    // 1. 如果请求携带了本地桌面特权 Local Token，无条件免密放行
    if let Some(ref tok) = token {
        if constant_time_equal(tok.as_bytes(), state.local_token.as_bytes()) {
            return next.run(request).await;
        }
    }

    // 2. 如果未开启网页访问（web_enabled 为 false），禁止普通 Web/浏览器访问
    if !sec.web_enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "WebAccessDisabled",
                "message": "网页访问功能已禁用。如需通过网页访问，请在应用设置中开启并设置访问密码。"
            })),
        )
            .into_response();
    }

    // 3. 如果开启了网页访问，但没有设置密码，拒绝远程访问（仅允许已验证的会话或禁止）
    if sec.password_hash.is_none() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({
                "error": "PasswordRequired",
                "message": "启用网页访问必须先设置访问密码。"
            })),
        )
            .into_response();
    }

    // 4. 验证 Web 会话 Token
    if let Some(ref tok) = token {
        let sessions = state.sessions.read().await;
        if sessions.contains(tok) {
            return next.run(request).await;
        }
    }

    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "Unauthorized", "message": "请输入访问密码以继续"})),
    )
        .into_response()
}

// -------------------------------- 静态资源托管 -------------------------------- //

async fn static_handler(uri: axum::http::Uri) -> impl IntoResponse {
    let mut req_path = uri.path().to_string();
    if req_path.is_empty() || req_path == "/" {
        req_path = "index.html".to_string();
    }
    let req_path = req_path.trim_start_matches('/');

    match PanelAssets::get(req_path) {
        Some(content) => {
            let mime = mime_guess::from_path(req_path).first_or_octet_stream();
            (
                [(
                    header::CONTENT_TYPE,
                    HeaderValue::from_str(mime.as_ref()).unwrap(),
                )],
                content.data,
            )
                .into_response()
        }
        None => {
            // fallback 到 index.html 支持前端路由
            match PanelAssets::get("index.html") {
                Some(content) => (
                    [(
                        header::CONTENT_TYPE,
                        HeaderValue::from_static("text/html; charset=utf-8"),
                    )],
                    content.data,
                )
                    .into_response(),
                None => (StatusCode::NOT_FOUND, "404 Not Found").into_response(),
            }
        }
    }
}

// -------------------------------- REST API 实现 -------------------------------- //

#[derive(Serialize)]
struct SecurityStatusResponse {
    web_enabled: bool,
    password_set: bool,
    allow_remote: bool,
    bind_addr: String,
    port: u16,
}

async fn api_security_status(State(state): State<WebState>) -> Response {
    let registry = match ProviderRegistry::load(state.registry_path()) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let sec = registry.web_security();
    let addr = *state.listen_addr.read().await;
    Json(SecurityStatusResponse {
        web_enabled: sec.web_enabled,
        password_set: sec.password_hash.is_some(),
        allow_remote: sec.allow_remote,
        bind_addr: addr.ip().to_string(),
        port: addr.port(),
    })
    .into_response()
}

#[derive(Deserialize)]
struct LoginRequest {
    password: String,
}

#[derive(Serialize)]
struct LoginResponse {
    token: String,
}

async fn api_security_login(
    State(state): State<WebState>,
    Json(payload): Json<LoginRequest>,
) -> Response {
    let registry = match ProviderRegistry::load(state.registry_path()) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let sec = registry.web_security();
    if !sec.web_enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "网页访问功能已禁用，请在应用设置中启用"})),
        )
            .into_response();
    }
    if let Some(stored_hash) = &sec.password_hash {
        if verify_password(&payload.password, stored_hash) {
            let token = Uuid::new_v4().to_string();
            state.sessions.write().await.insert(token.clone());
            return Json(LoginResponse { token }).into_response();
        }
    }
    (
        StatusCode::UNAUTHORIZED,
        Json(serde_json::json!({"error": "密码错误，请重试"})),
    )
        .into_response()
}

#[derive(Deserialize)]
struct UpdateSecurityRequest {
    web_enabled: Option<bool>,
    password: Option<String>,
    allow_remote: Option<bool>,
}

async fn api_security_update(
    State(state): State<WebState>,
    Json(payload): Json<UpdateSecurityRequest>,
) -> Response {
    let mut registry = match ProviderRegistry::load(state.registry_path()) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let mut new_token = None;
    let has_existing_pwd = registry.web_security().password_hash.is_some();
    let is_setting_new_pwd = payload
        .password
        .as_deref()
        .map(str::trim)
        .is_some_and(|p| !p.is_empty());

    let target_web_enabled = payload
        .web_enabled
        .unwrap_or(registry.web_security().web_enabled);
    let target_allow_remote = payload
        .allow_remote
        .unwrap_or(registry.web_security().allow_remote);

    // 校验：启用网页访问或允许外网，必须有密码
    if (target_web_enabled || target_allow_remote) && !has_existing_pwd && !is_setting_new_pwd {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "开启网页访问或外网访问必须先设置访问密码"})),
        )
            .into_response();
    }

    let sec = registry.web_security_mut();
    if let Some(pwd) = payload.password {
        if !pwd.trim().is_empty() {
            sec.password_hash = Some(hash_password(pwd.trim()));
            let token = Uuid::new_v4().to_string();
            state.sessions.write().await.insert(token.clone());
            new_token = Some(token);
        }
    }
    sec.web_enabled = target_web_enabled;
    sec.allow_remote = target_allow_remote;
    // 如果关闭了网页访问，同时重置 allow_remote
    if !sec.web_enabled {
        sec.allow_remote = false;
    }

    let final_web_enabled = sec.web_enabled;
    let final_allow_remote = sec.allow_remote;

    if let Err(e) = registry.save() {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "status": "ok",
        "message": "安全配置已保存",
        "token": new_token,
        "web_enabled": final_web_enabled,
        "allow_remote": final_allow_remote,
    }))
    .into_response()
}

async fn api_router_status(State(state): State<WebState>) -> Response {
    match state.supervisor.lock().await.status().await {
        Ok(status) => Json(status).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_desktop_status(State(state): State<WebState>) -> Response {
    let paths = match DesktopPaths::discover() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::NOT_FOUND,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let manifest = DesktopPaths::manifest_path_for_registry(state.registry_path());
    match status_for(&paths, manifest) {
        Ok(status) => Json(status).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize, Default)]
struct DesktopInstallReq {
    app_server_binary: Option<String>,
    build_metadata: Option<String>,
}

async fn api_desktop_install(
    State(state): State<WebState>,
    axum::extract::Json(payload): axum::extract::Json<Option<DesktopInstallReq>>,
) -> Response {
    let desktop_paths = match DesktopPaths::discover() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let registry_path = state.registry_path().to_path_buf();
    let manifest_path = DesktopPaths::manifest_path_for_registry(&registry_path);

    let req = payload.unwrap_or_default();
    let app_server_binary = req
        .app_server_binary
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CODEX_MP_APP_SERVER_BIN").map(PathBuf::from))
        .or_else(|| {
            std::env::current_exe().ok().and_then(|p| {
                p.parent()
                    .map(|parent| parent.join("codex-mp-app-server-bin"))
            })
        })
        .or_else(|| {
            std::env::current_dir().ok().and_then(|cwd| {
                let candidate = cwd.join("dist/stock-codex/codex-mp-app-server-bin");
                if candidate.exists() {
                    Some(candidate)
                } else {
                    None
                }
            })
        })
        .map(|p| resolve_executable(&p));
    let Some(app_server_binary) = app_server_binary else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "未找到 patched app-server 二进制。请设置环境变量 CODEX_MP_APP_SERVER_BIN 或将 codex-mp-app-server-bin 放在程序运行目录下"})),
        )
            .into_response();
    };

    let build_metadata = req
        .build_metadata
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CODEX_MP_BUILD_METADATA").map(PathBuf::from))
        .or_else(|| {
            app_server_binary
                .parent()
                .map(|p| p.join("codex-mp-build.json"))
        });
    let Some(build_metadata) = build_metadata else {
        return (
            StatusCode::BAD_REQUEST,
            Json(
                serde_json::json!({"error": "app-server 没有上级目录或未找到 codex-mp-build.json"}),
            ),
        )
            .into_response();
    };

    let codex_mp_binary = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("codex-mp"));
    let endpoint_file = default_router_endpoint_path();

    let result = match codex_mp_desktop::install(&DesktopInstallOptions {
        manifest_path: manifest_path.clone(),
        app_server_binary,
        build_metadata,
        codex_mp_binary,
        endpoint_file,
    }) {
        Ok(res) => res,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };

    let registry = match ProviderRegistry::load(&registry_path) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let integration_paths = IntegrationPaths::default();
    if let Err(e) = build_and_install(&integration_paths, &registry, &result.catalog_binary) {
        let _ = codex_mp_desktop::restore(&desktop_paths, &manifest_path);
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }

    match status_for(&desktop_paths, manifest_path) {
        Ok(status) => Json(status).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_desktop_restore(State(state): State<WebState>) -> Response {
    let paths = match DesktopPaths::discover() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let manifest = DesktopPaths::manifest_path_for_registry(state.registry_path());
    match codex_mp_desktop::restore(&paths, manifest) {
        Ok(restored) => Json(serde_json::json!({"restored": restored})).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_catalog_sync(State(state): State<WebState>) -> Response {
    let registry = match ProviderRegistry::load(state.registry_path()) {
        Ok(r) => r,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let paths = IntegrationPaths::default();
    let catalog_binary = match state.catalog_binary() {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };
    if let Err(e) = build_and_install(&paths, &registry, &catalog_binary) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response();
    }
    Json(serde_json::json!({"catalog_path": paths.catalog.display().to_string()})).into_response()
}

async fn api_list_providers(State(state): State<WebState>) -> Response {
    match state.providers.list_providers() {
        Ok(list) => Json(list).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn reload_router_if_running(state: &WebState) {
    let supervisor = state.supervisor.lock().await;
    let _ = supervisor.reload().await;
}

#[derive(Deserialize)]
struct AddProviderReq {
    name: String,
    base_url: String,
    protocol: ProviderProtocol,
    api_key: Option<String>,
}

async fn api_add_provider(
    State(state): State<WebState>,
    Json(payload): Json<AddProviderReq>,
) -> Response {
    match state.providers.add_provider(
        &payload.name,
        &payload.base_url,
        payload.protocol,
        payload.api_key.map(SecretString::from),
    ) {
        Ok(summary) => {
            reload_router_if_running(&state).await;
            Json(summary).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct EditProviderReq {
    id: String,
    name: Option<String>,
    base_url: Option<String>,
    protocol: Option<ProviderProtocol>,
    enabled: Option<bool>,
    api_key: Option<String>,
}

async fn api_edit_provider(
    State(state): State<WebState>,
    Json(payload): Json<EditProviderReq>,
) -> Response {
    match state.providers.edit_provider(
        &payload.id,
        payload.name,
        payload.base_url,
        payload.protocol,
        payload.enabled,
        payload.api_key.map(SecretString::from),
    ) {
        Ok(summary) => {
            reload_router_if_running(&state).await;
            Json(summary).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RemoveProviderReq {
    id: String,
    purge_credential: bool,
}

async fn api_remove_provider(
    State(state): State<WebState>,
    Json(payload): Json<RemoveProviderReq>,
) -> Response {
    match state
        .providers
        .remove_provider(&payload.id, payload.purge_credential)
    {
        Ok(()) => {
            reload_router_if_running(&state).await;
            Json(serde_json::json!({"status": "ok"})).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_discover_models(
    State(state): State<WebState>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Response {
    match state.providers.discover_models(&id).await {
        Ok(models) => Json(models).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct AddModelReq {
    provider_id: String,
    upstream_model_id: String,
    display_name: String,
    context_window: Option<u64>,
    images: bool,
    tools: bool,
}

async fn api_add_model(
    State(state): State<WebState>,
    Json(payload): Json<AddModelReq>,
) -> Response {
    let mut capabilities = ModelCapabilities::default();
    capabilities.images = payload.images;
    capabilities.tools = payload.tools;
    match state.providers.add_model(
        &payload.provider_id,
        &payload.upstream_model_id,
        &payload.display_name,
        payload.context_window,
        capabilities,
    ) {
        Ok(summary) => {
            reload_router_if_running(&state).await;
            Json(summary).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ImportModelsReq {
    provider_id: String,
    discovered: Vec<DiscoveredModel>,
    selected_ids: Vec<String>,
}

async fn api_import_models(
    State(state): State<WebState>,
    Json(payload): Json<ImportModelsReq>,
) -> Response {
    match state.providers.import_models(
        &payload.provider_id,
        &payload.discovered,
        &payload.selected_ids,
    ) {
        Ok(summaries) => {
            reload_router_if_running(&state).await;
            Json(summaries).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct EditModelReq {
    logical_model_id: String,
    display_name: Option<String>,
    context_window: Option<u64>,
    clear_context_window: bool,
}

async fn api_edit_model(
    State(state): State<WebState>,
    Json(payload): Json<EditModelReq>,
) -> Response {
    let edit = ModelEdit {
        display_name: payload.display_name,
        context_window: if payload.clear_context_window {
            Some(None)
        } else {
            payload.context_window.map(Some)
        },
    };
    match state.providers.edit_model(&payload.logical_model_id, edit) {
        Ok(summary) => {
            reload_router_if_running(&state).await;
            Json(summary).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct SetModelEnabledReq {
    logical_model_id: String,
    enabled: bool,
}

async fn api_set_model_enabled(
    State(state): State<WebState>,
    Json(payload): Json<SetModelEnabledReq>,
) -> Response {
    match state
        .providers
        .set_model_enabled(&payload.logical_model_id, payload.enabled)
    {
        Ok(summary) => {
            reload_router_if_running(&state).await;
            Json(summary).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RemoveModelReq {
    logical_model_id: String,
}

async fn api_remove_model(
    State(state): State<WebState>,
    Json(payload): Json<RemoveModelReq>,
) -> Response {
    match state.providers.remove_model(&payload.logical_model_id) {
        Ok(()) => {
            reload_router_if_running(&state).await;
            Json(serde_json::json!({"status": "ok"})).into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// -------------------------------- 官方账号与额度 API -------------------------------- //

async fn api_list_accounts(State(state): State<WebState>) -> Response {
    match state.accounts.list_accounts() {
        Ok(list) => Json(list).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_active_account(State(state): State<WebState>) -> Response {
    match state.accounts.check_active_status() {
        Ok(status) => Json(status).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct CaptureAccountReq {
    name: Option<String>,
}

async fn api_capture_account(
    State(state): State<WebState>,
    Json(payload): Json<CaptureAccountReq>,
) -> Response {
    match state.accounts.capture_current_auth(payload.name) {
        Ok(acc) => Json(acc).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ImportAccountReq {
    name: Option<String>,
    auth_json: Option<serde_json::Value>,
    tokens: Option<AccountTokens>,
}

async fn api_import_account(
    State(state): State<WebState>,
    Json(payload): Json<ImportAccountReq>,
) -> Response {
    let (tokens, last_refresh) = if let Some(t) = payload.tokens {
        (t, None)
    } else if let Some(aj) = payload.auth_json {
        let tokens_obj = match aj.get("tokens") {
            Some(tok) => tok.clone(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "auth_json missing `tokens` object"})),
                )
                    .into_response();
            }
        };
        let t: AccountTokens = match serde_json::from_value(tokens_obj) {
            Ok(toks) => toks,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": format!("invalid tokens structure: {e}")})),
                )
                    .into_response();
            }
        };
        let lr = aj
            .get("last_refresh")
            .and_then(|v| v.as_str())
            .map(String::from);
        (t, lr)
    } else {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Either `tokens` or `auth_json` must be provided"})),
        )
            .into_response();
    };

    match state
        .accounts
        .import_or_update_account(tokens, last_refresh, payload.name)
    {
        Ok(acc) => Json(acc).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct SwitchAccountReq {
    account_id: String,
    #[serde(default)]
    restart_codex: bool,
}

async fn api_switch_account(
    State(state): State<WebState>,
    Json(payload): Json<SwitchAccountReq>,
) -> Response {
    match state.accounts.switch_to_account(&payload.account_id) {
        Ok(acc) => {
            let mut restart_report = None;
            if payload.restart_codex {
                restart_report = state.accounts.restart_codex_processes().ok();
            }
            Json(serde_json::json!({
                "account": acc,
                "restart_report": restart_report,
            }))
            .into_response()
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RenameAccountReq {
    account_id: String,
    name: String,
}

async fn api_rename_account(
    State(state): State<WebState>,
    Json(payload): Json<RenameAccountReq>,
) -> Response {
    match state
        .accounts
        .rename_account(&payload.account_id, &payload.name)
    {
        Ok(()) => Json(serde_json::json!({"status": "ok"})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct DeleteAccountReq {
    account_id: String,
}

async fn api_delete_account(
    State(state): State<WebState>,
    Json(payload): Json<DeleteAccountReq>,
) -> Response {
    match state.accounts.delete_account(&payload.account_id) {
        Ok(deleted) => Json(serde_json::json!({"deleted": deleted})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_restart_codex(State(state): State<WebState>) -> Response {
    match state.accounts.restart_codex_processes() {
        Ok(report) => Json(report).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_fetch_account_usage(
    State(state): State<WebState>,
    axum::extract::Path(account_id): axum::extract::Path<String>,
) -> Response {
    match state.accounts.fetch_usage(&account_id).await {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// -------------------------------- Web Server 运行入口 -------------------------------- //

pub async fn run_web_server(
    registry_path: PathBuf,
    endpoint_file: PathBuf,
    router_bin: PathBuf,
    codex_bin: PathBuf,
    port_override: Option<u16>,
    allow_remote_override: Option<bool>,
) -> Result<(), WebError> {
    run_web_server_with_local_token(
        registry_path,
        endpoint_file,
        router_bin,
        codex_bin,
        port_override,
        allow_remote_override,
        None,
    )
    .await
}

pub async fn run_web_server_with_local_token(
    registry_path: PathBuf,
    endpoint_file: PathBuf,
    router_bin: PathBuf,
    codex_bin: PathBuf,
    port_override: Option<u16>,
    allow_remote_override: Option<bool>,
    local_token: Option<String>,
) -> Result<(), WebError> {
    let registry =
        ProviderRegistry::load(&registry_path).map_err(|e| WebError::Internal(e.to_string()))?;
    let sec = registry.web_security();

    let allow_remote = allow_remote_override.unwrap_or(sec.allow_remote);
    let port = port_override.unwrap_or(sec.port);

    if allow_remote && sec.password_hash.is_none() {
        return Err(WebError::RemoteNotAllowedWithoutPassword);
    }

    let bind_ip: IpAddr = if allow_remote {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };

    let bind_addr = SocketAddr::new(bind_ip, port);
    let listener = tokio::net::TcpListener::bind(bind_addr).await?;
    let local_addr = listener.local_addr()?;

    let token = local_token.unwrap_or_else(|| Uuid::new_v4().to_string());
    let state = WebState::with_local_token(
        registry_path,
        endpoint_file,
        router_bin,
        codex_bin,
        token.clone(),
    );
    *state.listen_addr.write().await = local_addr;

    // 自动在后台守护启动 Router（如果尚未启动）
    let supervisor_clone = state.supervisor.clone();
    tokio::spawn(async move {
        let _ = supervisor_clone.lock().await.start().await;
    });

    println!("============================================================");
    println!(" Codex MultiProvider Web 控制面板已启动！");
    println!(" 监听地址: http://{local_addr}");
    if allow_remote {
        println!(" 外网/局域网访问: [已开启]");
    } else {
        println!(" 外网/局域网访问: [已关闭 - 仅限本机 loopback 访问]");
    }
    if sec.web_enabled {
        println!(" 网页端访问: [已启用]");
    } else {
        println!(" 网页端访问: [已禁用 (仅限应用内免密通信)]");
    }
    if sec.password_hash.is_some() {
        println!(" 访问控制: [已设置访问密码保护]");
    } else {
        println!(" 访问控制: [未设置访问密码]");
    }
    println!("============================================================");

    let app = create_web_router(state);
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_password_hash_and_verify() {
        let raw = "super-secret-123";
        let hash = hash_password(raw);
        assert!(verify_password(raw, &hash));
        assert!(!verify_password("wrong-password", &hash));
    }

    #[tokio::test]
    async fn test_web_security_remote_access_guard() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        let endpoint_file = dir.path().join("endpoint.json");

        // 1. 无密码时开启 remote 应拒绝
        let res = run_web_server(
            registry_path.clone(),
            endpoint_file.clone(),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            Some(39191),
            Some(true),
        )
        .await;
        assert!(matches!(
            res,
            Err(WebError::RemoteNotAllowedWithoutPassword)
        ));

        // 2. 配置密码后再尝试
        let mut reg = ProviderRegistry::load(&registry_path).unwrap();
        reg.web_security_mut().password_hash = Some(hash_password("admin123"));
        reg.save().unwrap();

        // 验证验证逻辑
        let sec = reg.web_security();
        assert!(sec.password_hash.is_some());
        assert!(verify_password(
            "admin123",
            sec.password_hash.as_ref().unwrap()
        ));
    }

    #[tokio::test]
    async fn test_web_account_endpoints() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        let endpoint_file = dir.path().join("endpoint.json");
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();

        let auth_sample = serde_json::json!({
            "auth_mode": "chatgpt",
            "tokens": {
                "id_token": "dummy.eyJlbWFpbCI6ImFkbWluQGV4YW1wbGUuY29tIiwibmFtZSI6IkFkbWluIiwic3ViIjoiMSIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3BsYW5fdHlwZSI6InBsdXMiLCJjaGF0Z3B0X2FjY291bnRfaWQiOiJhLTEifX0.dummy",
                "access_token": "token-1",
                "refresh_token": "ref-1",
                "account_id": "a-1"
            }
        });
        fs::write(
            codex_home.join("auth.json"),
            serde_json::to_string(&auth_sample).unwrap(),
        )
        .unwrap();

        let mut state = WebState::new(
            registry_path,
            endpoint_file,
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
        );
        state.accounts = Arc::new(AccountManager::with_paths(
            dir.path().join("accounts.json"),
            &codex_home,
        ));

        // 1. Capture account
        let captured = state
            .accounts
            .capture_current_auth(Some("Primary Admin".into()))
            .unwrap();
        assert_eq!(captured.name, "Primary Admin");

        // 2. Active status
        let active = state.accounts.check_active_status().unwrap();
        assert!(active.is_logged_in);
        assert_eq!(active.email.as_deref(), Some("admin@example.com"));
        assert_eq!(
            active.matched_account_id.as_deref(),
            Some(captured.id.as_str())
        );

        // 3. List accounts
        let list = state.accounts.list_accounts().unwrap();
        assert_eq!(list.len(), 1);
        assert!(list[0].is_active);
    }
}
