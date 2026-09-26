//! Web interface, static asset embedding, REST API and access control for Codex MultiProvider.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use argon2::Argon2;
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use axum::body::Body;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use codex_mp_core::{
    AuthStrategy, ModelCapabilities, ModelEdit, ProviderProtocol, ProviderRegistry,
    clean_verbatim_path, resolve_executable,
};
use codex_mp_desktop::{DesktopInstallOptions, DesktopPaths, status_for};
use codex_mp_integration::{IntegrationPaths, build_and_install};
use codex_mp_manager::{
    AccountManager, AccountTokens, DiscoveredModel, ProviderManager, RouterSupervisor,
};
use codex_mp_router::default_router_endpoint_path;
use password_hash::rand_core::OsRng;
use rust_embed::RustEmbed;
use secrecy::SecretString;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tokio::sync::{Mutex, RwLock, Semaphore};
use tokio::time::timeout;
use tower::BoxError;
use tower::limit::ConcurrencyLimitLayer;
use tower::load_shed::LoadShedLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::limit::RequestBodyLimitLayer;
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
    /// The control panel could not bind its listening address.
    ///
    /// Carries the address so a conflict names the port, rather than surfacing
    /// only the OS errno.
    #[error(
        "could not bind the control panel to {addr}: {source}; another process is \
using that port — stop it, or start the panel on a different one with \
`codex-mp web start --port <PORT>`"
    )]
    Bind {
        addr: String,
        #[source]
        source: std::io::Error,
    },
    #[error("internal error: {0}")]
    Internal(String),
}

/// Marker prefix for the current password hash format.
const PASSWORD_HASH_PREFIX: &str = "argon2id$";

/// How long a browser session stays valid overall, and how long it may sit idle.
const SESSION_TTL: Duration = Duration::from_secs(12 * 60 * 60);
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// Failed-login budget. The panel can be exposed to a LAN, so the weak
/// first-round hash is no longer the only thing standing between an attacker and
/// an unlimited online guessing loop.
const LOGIN_WINDOW: Duration = Duration::from_secs(60);
const LOGIN_MAX_ATTEMPTS_PER_IP: usize = 5;
const LOGIN_MAX_ATTEMPTS_GLOBAL: usize = 30;

/// Upper bound on concurrent browser sessions.
///
/// The session map was unbounded: every successful login inserted a new entry,
/// and expired entries were only swept when some *other* request arrived. A
/// client that simply logs in repeatedly could grow the map for ever. The size
/// per session is small, so this is hygiene rather than a live threat, but it
/// matches the bound already applied to the login limiter — and an unbounded
/// map keyed by attacker-supplied values is the same class of problem this
/// project has fixed elsewhere.
const MAX_SESSIONS: usize = 1024;

/// Upper bound on distinct source addresses tracked by the login limiter.
///
/// Above this, entries whose window has fully expired are swept, so a flood from
/// rotating addresses cannot grow the map without bound.
const LOGIN_MAX_TRACKED_IPS: usize = 4096;
const WEB_BLOCKING_SLOTS: usize = 8;
const WEB_BLOCKING_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_WEB_REQUEST_BODY_BYTES: usize = 2 * 1024 * 1024;
const WEB_MAX_IN_FLIGHT_REQUESTS: usize = 128;
const WEB_NETWORK_SLOTS: usize = 8;
const ROUTER_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_secs(2);
const ROUTER_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(30);

static WEB_BLOCKING_POOL: OnceLock<Arc<Semaphore>> = OnceLock::new();
static WEB_NETWORK_POOL: OnceLock<Arc<Semaphore>> = OnceLock::new();

/// Throttle repeated Router start/restart attempts while the process is
/// persistently broken. A start can succeed at the process level while the
/// health endpoint remains unusable (for example because the registry is
/// corrupt or the executable immediately crashes), so resetting this delay on
/// a successful spawn would still create a stop/start storm.
#[derive(Debug)]
struct RouterRecoveryBackoff {
    next_attempt: Option<Instant>,
    delay: Duration,
}

impl Default for RouterRecoveryBackoff {
    fn default() -> Self {
        Self {
            next_attempt: None,
            delay: ROUTER_RECOVERY_INITIAL_BACKOFF,
        }
    }
}

impl RouterRecoveryBackoff {
    fn reset(&mut self) {
        self.next_attempt = None;
        self.delay = ROUTER_RECOVERY_INITIAL_BACKOFF;
    }

    fn due(&self, now: Instant) -> bool {
        self.next_attempt
            .is_none_or(|next_attempt| now >= next_attempt)
    }

    fn retry_after(&self) -> Duration {
        self.delay
    }

    fn attempted(&mut self, now: Instant) {
        self.next_attempt = Some(now + self.delay);
        self.delay = (self.delay * 2).min(ROUTER_RECOVERY_MAX_BACKOFF);
    }
}

async fn run_web_blocking<T, F>(operation: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let slots = WEB_BLOCKING_POOL
        .get_or_init(|| Arc::new(Semaphore::new(WEB_BLOCKING_SLOTS)))
        .clone();
    let permit = timeout(WEB_BLOCKING_TIMEOUT, slots.acquire_owned())
        .await
        .map_err(|_| "web blocking work is overloaded; retry shortly".to_owned())?
        .map_err(|error| error.to_string())?;
    let task = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        operation()
    });
    timeout(WEB_BLOCKING_TIMEOUT, task)
        .await
        .map_err(|_| "web blocking operation timed out".to_owned())?
        .map_err(|error| error.to_string())?
}

fn argon2_hasher() -> Argon2<'static> {
    // Argon2::default() is Argon2id v19 with Params::DEFAULT (19 MiB, t=2, p=1),
    // which matches the OWASP minimum recommendation.
    Argon2::default()
}

/// Hash a password with Argon2id.
///
/// The previous implementation called this "PBKDF2-like" but performed a single
/// unsalted-work-factor SHA-256 round, which a GPU can brute-force at billions of
/// guesses per second. Argon2id is memory-hard and deliberately slow.
///
/// The password is trimmed here rather than at each call site, and
/// [`verify_password`] trims identically. Every caller used to decide for itself:
/// the panel's *set* path trimmed, its *login* path did not, the CLI's `--stdin`
/// path trimmed and its inline `--password` path did not. So
/// `codex-mp web password --stdin` fed by a pipe (where the shell appends a
/// newline) stored a hash of the trimmed password while the panel verified the
/// pasted string *with* the newline — the user was locked out by a password they
/// had just set correctly, with no way to tell why.
pub fn hash_password(password: &str) -> String {
    let password = password.trim();
    let salt = SaltString::generate(&mut OsRng);
    match argon2_hasher().hash_password(password.as_bytes(), &salt) {
        Ok(hash) => format!("{PASSWORD_HASH_PREFIX}{hash}"),
        // Hashing with a fresh random salt cannot realistically fail. If it ever
        // does, emit a value that never verifies rather than an empty string that
        // could be mistaken for "no password configured".
        Err(_) => "argon2id$!hashing-failed".to_owned(),
    }
}

/// True when a stored hash predates the Argon2id format and should be replaced
/// on the next successful login.
pub fn password_hash_needs_upgrade(stored_hash: &str) -> bool {
    !stored_hash.starts_with(PASSWORD_HASH_PREFIX)
}

pub fn verify_password(password: &str, stored_hash: &str) -> bool {
    if let Some(phc) = stored_hash.strip_prefix(PASSWORD_HASH_PREFIX) {
        let Ok(parsed) = PasswordHash::new(phc) else {
            return false;
        };
        return argon2_hasher()
            .verify_password(password.trim().as_bytes(), &parsed)
            .is_ok();
    }
    verify_legacy_password(password, stored_hash)
}

/// Verification for hashes written by earlier releases: `salt$sha256(salt:password)`.
/// Kept so an existing installation can still log in and migrate.
///
/// Receives the password **untrimmed** and tries the trimmed form first, then the
/// raw one. Current set paths all trim, but a legacy hash may have been produced
/// by one that did not, and the whole point of this function is to let that user
/// reach the migration in [`api_security_login`] rather than lock them out of
/// their own panel. Trying both costs an attacker nothing: anyone able to supply
/// the raw string already knows the password.
fn verify_legacy_password(raw_password: &str, stored_hash: &str) -> bool {
    let Some((salt, expected_hash)) = stored_hash.split_once('$') else {
        return false;
    };
    let digest = |candidate: &str| {
        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(b":");
        hasher.update(candidate.as_bytes());
        hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    };
    let trimmed = digest(raw_password.trim());
    let raw = digest(raw_password);
    constant_time_equal(trimmed.as_bytes(), expected_hash.as_bytes())
        | constant_time_equal(raw.as_bytes(), expected_hash.as_bytes())
}

fn constant_time_equal(a: &[u8], b: &[u8]) -> bool {
    let length = a.len().max(b.len());
    let mut diff = (a.len() ^ b.len()) as u8;
    for index in 0..length {
        let x = a.get(index).copied().unwrap_or(0);
        let y = b.get(index).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

fn is_loopback_addr(addr: SocketAddr) -> bool {
    addr.ip().is_loopback()
}

/// Sliding-window failed-login limiter, keyed per source address plus a global
/// ceiling so a distributed source set cannot bypass it entirely.
#[derive(Default)]
pub struct LoginRateLimiter {
    per_ip: HashMap<IpAddr, VecDeque<Instant>>,
    global: VecDeque<Instant>,
}

impl LoginRateLimiter {
    fn prune(window: &mut VecDeque<Instant>, now: Instant) {
        while window
            .front()
            .is_some_and(|stamp| now.duration_since(*stamp) > LOGIN_WINDOW)
        {
            window.pop_front();
        }
    }

    /// Returns the remaining lockout duration when the caller must be rejected.
    pub fn check(&mut self, ip: IpAddr, now: Instant) -> Option<Duration> {
        // The global ceiling exists to stop a *distributed* remote source set from
        // guessing without bound. Counting loopback in it lets anything on the LAN
        // that can reach an `allow_remote` panel lock the owner out of their own
        // machine — including out of the very setting they would turn off to stop
        // it, since the panel is the only way to change it. Loopback is therefore
        // exempt from the global window but still subject to its own per-address
        // window, which keeps a local guessing loop throttled.
        if !ip.is_loopback() {
            Self::prune(&mut self.global, now);
            if self.global.len() >= LOGIN_MAX_ATTEMPTS_GLOBAL {
                return Some(LOGIN_WINDOW);
            }
        }
        if let Some(window) = self.per_ip.get_mut(&ip) {
            Self::prune(window, now);
            if window.len() >= LOGIN_MAX_ATTEMPTS_PER_IP {
                let oldest = window.front().copied().unwrap_or(now);
                return Some(LOGIN_WINDOW.saturating_sub(now.duration_since(oldest)));
            }
        }
        None
    }

    pub fn record_failure(&mut self, ip: IpAddr, now: Instant) {
        // Symmetric with `check`: a loopback failure must not consume the global
        // budget, or a local typo could lock a remote administrator out.
        if !ip.is_loopback() {
            Self::prune(&mut self.global, now);
            self.global.push_back(now);
        }
        let window = self.per_ip.entry(ip).or_default();
        Self::prune(window, now);
        window.push_back(now);
        // Bound the map.
        //
        // Entries were only ever removed by a *successful* login from the same
        // address, so a failed-login flood from rotating source addresses grew
        // this map without bound: the global attempt cap throttles how many
        // failures are *recorded* per window, not how many distinct addresses get
        // an entry. With a 60s window that is up to 30 new entries per minute
        // (~43k/day), each holding an `IpAddr` and a `VecDeque`, forever.
        //
        // Expired entries are reclaimed first. If the map is still at its cap
        // (a flood can fill it faster than one window expires), the oldest
        // entries are dropped: they are throttling state, not user data, and
        // forgetting the oldest attempt is the conservative failure mode — an
        // attacker cannot use it to gain unlimited tries, because the *global*
        // window still caps attempts.
        if self.per_ip.len() >= LOGIN_MAX_TRACKED_IPS {
            self.per_ip.retain(|_, window| {
                Self::prune(window, now);
                !window.is_empty()
            });
            while self.per_ip.len() > LOGIN_MAX_TRACKED_IPS {
                let Some(oldest) = self
                    .per_ip
                    .iter()
                    .min_by_key(|(_, window)| window.front().copied())
                    .map(|(addr, _)| *addr)
                else {
                    break;
                };
                self.per_ip.remove(&oldest);
            }
        }
    }

    pub fn record_success(&mut self, ip: IpAddr) {
        self.per_ip.remove(&ip);
    }
}

/// Insert a session, evicting the least recently used entry when at the cap.
///
/// Expired sessions are swept first; if the map is still full, the oldest by
/// `last_seen` is dropped, so a burst of logins cannot grow the map without
/// bound nor lock out a legitimate new session.
fn insert_session_bounded(
    sessions: &mut HashMap<String, Session>,
    token: String,
    session: Session,
    now: Instant,
    current_password_hash: Option<&str>,
) {
    sessions.retain(|_, existing| existing.is_valid(now, current_password_hash));
    while sessions.len() >= MAX_SESSIONS {
        let Some(oldest) = sessions
            .iter()
            .min_by_key(|(_, existing)| existing.last_seen)
            .map(|(token, _)| token.clone())
        else {
            break;
        };
        sessions.remove(&oldest);
    }
    sessions.insert(token, session);
}

/// A browser session token with an absolute lifetime and an idle timeout.
#[derive(Clone)]
pub struct Session {
    created: Instant,
    last_seen: Instant,
    /// The stored password hash in force when this session was issued.
    ///
    /// A session was previously valid for up to 12 hours purely by time, so a
    /// password change performed **by another process** (`codex-mp web password`)
    /// left every existing session working: revoking a leaked token required
    /// restarting the panel. Binding the session to the hash that issued it makes
    /// any password change invalidate all outstanding sessions, however it was
    /// made.
    password_hash: Option<String>,
}

impl Session {
    fn is_valid(&self, now: Instant, current_password_hash: Option<&str>) -> bool {
        now.duration_since(self.created) <= SESSION_TTL
            && now.duration_since(self.last_seen) <= SESSION_IDLE_TIMEOUT
            && self.password_hash.as_deref() == current_password_hash
    }
}

#[derive(Clone)]
pub struct WebState {
    pub providers: Arc<ProviderManager>,
    pub supervisor: Arc<Mutex<RouterSupervisor>>,
    pub accounts: Arc<AccountManager>,
    pub codex_binary: PathBuf,
    pub sessions: Arc<RwLock<HashMap<String, Session>>>,
    pub login_limiter: Arc<Mutex<LoginRateLimiter>>,
    pub listen_addr: Arc<RwLock<SocketAddr>>,
    /// Secret loopback token for desktop/local IPC direct access without password.
    pub local_token: Arc<String>,
    /// Paths derived from the *configured* registry, so the panel and the CLI
    /// always operate on the same catalog/manifest/config location.
    pub integration_paths: IntegrationPaths,
    router_desired: Arc<AtomicBool>,
}

async fn load_registry_for_web(state: &WebState) -> Result<ProviderRegistry, String> {
    let path = state.registry_path().to_path_buf();
    run_web_blocking(move || ProviderRegistry::load(path).map_err(|error| error.to_string())).await
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
        let integration_paths = IntegrationPaths::for_registry(&registry_path);
        // Stock Codex dials the `base_url` from `config.toml`; it never reads the
        // Router's endpoint file. A panel-started Router on an ephemeral port was
        // therefore unreachable for the Codex session it exists to serve.
        let router_port = codex_mp_integration::router_port_for_registry(&registry_path);
        let providers = Arc::new(ProviderManager::new(registry_path.clone()));
        let supervisor = Arc::new(Mutex::new(
            RouterSupervisor::new(router_bin, registry_path, endpoint_file).with_port(router_port),
        ));
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
            sessions: Arc::new(RwLock::new(HashMap::new())),
            login_limiter: Arc::new(Mutex::new(LoginRateLimiter::default())),
            listen_addr: Arc::new(RwLock::new(SocketAddr::new(
                IpAddr::V4(Ipv4Addr::LOCALHOST),
                codex_mp_core::DEFAULT_WEB_PORT,
            ))),
            local_token: Arc::new(local_token),
            integration_paths,
            router_desired: Arc::new(AtomicBool::new(true)),
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
        .route("/router/restart", post(api_router_restart))
        .route("/router/stop", post(api_router_stop))
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
        .route("/models/show", post(api_show_model))
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
        .route("/accounts/backups", get(api_list_auth_backups))
        .route("/accounts/backups/restore", post(api_restore_auth_backup))
        .route("/accounts/backups/clean", post(api_clean_auth_backups))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            auth_middleware,
        ));

    // 无需鉴权的接口（登录和静态资源）
    Router::new()
        .route("/healthz", get(api_web_healthz))
        .route("/api/v1/app/version", get(api_app_version))
        .route("/api/v1/security/status", get(api_security_status))
        .route("/api/v1/security/login", post(api_security_login))
        // Logout only removes the token that was presented, so it is safe to
        // expose outside the auth layer and lets an expired session clean up.
        .route("/api/v1/security/logout", post(api_security_logout))
        .nest("/api/v1", api_router)
        // An unknown `/api/...` path used to fall through to `static_handler`,
        // which answered **200 text/html** with the SPA. A client typo therefore
        // looked like success: the panel's `api()` only checks `response.ok`, so
        // it parsed an empty object and reported success for a request that never
        // ran. API paths must answer 404 as JSON; everything else still gets the
        // SPA so client-side routing keeps working.
        .fallback(api_aware_fallback)
        .layer(cors_layer())
        .layer(RequestBodyLimitLayer::new(MAX_WEB_REQUEST_BODY_BYTES))
        // A final in-flight cap prevents a burst of future endpoints from
        // allocating independently even when their individual paths have no
        // expensive blocking operation.
        // `ConcurrencyLimitLayer` waits for a permit in `poll_ready`. In an
        // Axum server that can leave excess connections queued while they hold
        // request state and buffers. Load shedding keeps overload bounded and
        // lets clients retry with an explicit 503 instead. The error handler
        // must be part of the same layer stack so Axum sees the final
        // `Infallible` service error rather than Tower's `BoxError`.
        .layer(
            tower::ServiceBuilder::new()
                .layer(axum::error_handling::HandleErrorLayer::new(
                    handle_web_layer_error,
                ))
                .layer(LoadShedLayer::new())
                .layer(ConcurrencyLimitLayer::new(WEB_MAX_IN_FLIGHT_REQUESTS)),
        )
        .with_state(state)
}

async fn handle_web_layer_error(error: BoxError) -> Response {
    if error
        .downcast_ref::<tower::load_shed::error::Overloaded>()
        .is_some()
    {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "ServiceUnavailable",
                "message": "the control panel is busy; retry shortly",
            })),
        )
            .into_response();
    }

    // Do not expose middleware internals to a browser. This path is not
    // expected for the current router, but returning a response keeps an
    // unexpected middleware failure from terminating the server task.
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(serde_json::json!({
            "error": "InternalServerError",
            "message": "the control panel could not process the request",
        })),
    )
        .into_response()
}

fn try_web_network_slot() -> Option<tokio::sync::OwnedSemaphorePermit> {
    WEB_NETWORK_POOL
        .get_or_init(|| Arc::new(Semaphore::new(WEB_NETWORK_SLOTS)))
        .clone()
        .try_acquire_owned()
        .ok()
}

/// Serve the SPA for client routes, but answer unknown API paths with a JSON 404.
async fn api_aware_fallback(uri: axum::http::Uri) -> Response {
    if uri.path().starts_with("/api/") {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "NotFound",
                "message": format!("no such API endpoint: {}", uri.path()),
            })),
        )
            .into_response();
    }
    static_handler(uri).await.into_response()
}

/// Whether an `Origin` header value may talk to the control API.
///
/// Only the local machine counts. The panel served by this process is
/// same-origin and needs no CORS at all; an Electron-hosted panel loads over
/// `file://` and therefore presents an opaque `null` origin. A permissive policy
/// would only widen the attack surface.
fn origin_is_loopback_or_null(origin: &str) -> bool {
    if origin == "null" {
        return true;
    }
    match url::Url::parse(origin) {
        Ok(url) => matches!(
            url.host_str(),
            Some("localhost") | Some("127.0.0.1") | Some("[::1]") | Some("::1")
        ),
        Err(_) => false,
    }
}

/// Whether a `Host` header names this machine's loopback interface.
///
/// Parsed as a URL authority rather than string-matched, so `127.0.0.1:31828`,
/// `localhost:31828` and `[::1]:31828` all pass while `evil.example`,
/// `127.0.0.1.evil.example` and `user:pass@127.0.0.1` do not. Mirrors the
/// router's own `is_loopback_authority`.
///
/// The host is matched on the typed [`url::Host`] rather than on `host_str()`,
/// because `host_str()` returns an IPv6 literal **with its brackets** (`"[::1]"`)
/// and `str::parse::<IpAddr>()` rejects that. A user who opened the panel at
/// `http://[::1]:31828` — which is what a resolver that prefers IPv6 hands the
/// browser for `localhost` — was therefore locked out of their own loopback-only
/// panel with a message telling them to use a loopback address.
fn host_is_loopback_authority(value: &str) -> bool {
    let Ok(parsed) = url::Url::parse(&format!("http://{value}")) else {
        return false;
    };
    let bare_authority = parsed.path().is_empty() || parsed.path() == "/";
    parsed.username().is_empty()
        && parsed.password().is_none()
        && parsed.query().is_none()
        && parsed.fragment().is_none()
        && bare_authority
        && match parsed.host() {
            Some(url::Host::Domain(domain)) => domain.eq_ignore_ascii_case("localhost"),
            Some(url::Host::Ipv4(address)) => address.is_loopback(),
            Some(url::Host::Ipv6(address)) => address.is_loopback(),
            None => false,
        }
}

/// Reject a request whose `Host` does not name the loopback interface.
///
/// A missing `Host` is rejected too: HTTP/1.1 requires it, so its absence means
/// a hand-rolled client, and guessing an authority for it would defeat the point.
/// The message names the header the caller got wrong, because "400 Bad Request"
/// alone sends a user who aliased the panel in `/etc/hosts` looking everywhere
/// except there.
fn reject_non_loopback_host(headers: &HeaderMap) -> Option<Response> {
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.trim().is_empty());
    if host.is_some_and(host_is_loopback_authority) {
        return None;
    }
    Some(
        (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "InvalidHost",
                "message": match host {
                    Some(value) => format!(
                        "拒绝访问：Host 头 `{value}` 不是本机回环地址。面板当前仅限本机访问，\
                         请通过 http://127.0.0.1:<端口> 或 http://localhost:<端口> 打开；\
                         如需从其他设备访问，请先在设置中开启「允许外网/局域网访问」并重启面板。"
                    ),
                    None => "拒绝访问：请求缺少 Host 头。".to_owned(),
                },
            })),
        )
            .into_response(),
    )
}

fn cors_layer() -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(
            |origin: &HeaderValue, _parts: &http::request::Parts| {
                origin
                    .to_str()
                    .map(origin_is_loopback_or_null)
                    .unwrap_or(false)
            },
        ))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::CONTENT_TYPE,
            header::AUTHORIZATION,
            header::ACCEPT,
            header::HeaderName::from_static("x-local-token"),
        ])
}

// -------------------------------- Auth 中间件 -------------------------------- //

async fn auth_middleware(
    State(state): State<WebState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    request: Request<Body>,
    next: Next,
) -> Response {
    let registry = match load_registry_for_web(&state).await {
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

    // DNS-rebinding guard.
    //
    // A malicious page can make `attacker.example` resolve to 127.0.0.1, after
    // which the browser considers the panel same-origin with the attacker's
    // script: no `Origin` header is sent, so the CORS allowlist below never gets
    // a vote, and the request arrives from a loopback peer — which is also what
    // the desktop `local_token` bypass keys on. The `Host` header is the one part
    // of that request the attacker cannot forge, because the browser sets it from
    // the URL bar.
    //
    // Only enforced while remote access is off. With `allow_remote` the user has
    // deliberately exposed the panel on the LAN, where the legitimate `Host` is
    // the machine's LAN address or hostname and cannot be enumerated here; that
    // mode already requires a password and still gets the `Origin` allowlist.
    if !sec.allow_remote
        && let Some(response) = reject_non_loopback_host(&headers)
    {
        return response;
    }

    // 检查是否有 Authorization 标头
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let custom_token_header = headers.get("x-local-token").and_then(|v| v.to_str().ok());

    let token = if let Some(auth) = auth_header {
        auth.strip_prefix("Bearer ")
            .map(|token| token.trim().to_string())
    } else {
        custom_token_header.map(|custom| custom.trim().to_string())
    };

    // 1. 桌面特权 Local Token：仅接受来自 loopback 的调用。
    //    缺少来源校验时，任何拿到该 token 的局域网客户端都能完全绕过密码。
    if let Some(ref tok) = token
        && is_loopback_addr(peer)
        && !state.local_token.is_empty()
        && constant_time_equal(tok.as_bytes(), state.local_token.as_bytes())
    {
        return next.run(request).await;
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

    // 4. 验证 Web 会话 Token（带绝对有效期、空闲超时，并绑定当前密码）
    if let Some(ref tok) = token {
        let now = Instant::now();
        // The hash in force for this request. The middleware already loaded the
        // registry above, so this is not an extra read; a password change made by
        // *another* process (`codex-mp web password`) must revoke the session, and
        // time alone cannot detect that.
        let current_hash = registry.web_security().password_hash.clone();
        let mut sessions = state.sessions.write().await;
        sessions.retain(|_, session| session.is_valid(now, current_hash.as_deref()));
        if let Some(session) = sessions.get_mut(tok) {
            session.last_seen = now;
            drop(sessions);
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

    // Defence in depth for the self-hosted panel: the same policy is also
    // declared as a <meta> tag in index.html for the Electron file:// case.
    // The two must stay textually identical.
    //
    // Fonts and icons are self-hosted under apps/panel/fonts, so no third-party
    // origin is allowed any more. `http://[::1]:*` used to be listed for
    // loopback IPv6, but Chromium rejects IPv6 literals in a CSP host-source
    // outright ("contains an invalid source") and logs an error on every load;
    // 127.0.0.1 and localhost cover the loopback cases the backend binds to.
    let security_headers = [
        (
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'none'; script-src 'self'; \
                 style-src 'self' 'unsafe-inline'; \
                 font-src 'self'; img-src 'self' data:; \
                 connect-src 'self' http://127.0.0.1:* http://localhost:*; \
                 form-action 'none'; base-uri 'none'; frame-ancestors 'none'; object-src 'none'",
            ),
        ),
        (
            header::X_CONTENT_TYPE_OPTIONS,
            HeaderValue::from_static("nosniff"),
        ),
        (
            header::REFERRER_POLICY,
            HeaderValue::from_static("no-referrer"),
        ),
    ];

    let (content_type, body) = match PanelAssets::get(req_path) {
        Some(content) => {
            let mime = mime_guess::from_path(req_path).first_or_octet_stream();
            let value = HeaderValue::from_str(mime.as_ref())
                .unwrap_or_else(|_| HeaderValue::from_static("application/octet-stream"));
            (value, Some(content.data))
        }
        None => {
            // Fall back to index.html for client-side routes, but only for
            // requests that are not asking for a static asset. Serving HTML
            // with a 200 for a mistyped .woff2/.css/.js turns "wrong path" into
            // a decode failure in the browser, which is far harder to diagnose
            // than a plain 404.
            let wants_asset = std::path::Path::new(req_path)
                .extension()
                .and_then(|ext| ext.to_str())
                .map(|ext| {
                    matches!(
                        ext.to_ascii_lowercase().as_str(),
                        "js" | "css"
                            | "woff"
                            | "woff2"
                            | "ttf"
                            | "otf"
                            | "svg"
                            | "png"
                            | "jpg"
                            | "jpeg"
                            | "gif"
                            | "webp"
                            | "ico"
                            | "json"
                            | "map"
                    )
                })
                .unwrap_or(false);

            if wants_asset {
                return (StatusCode::NOT_FOUND, "404 Not Found").into_response();
            }

            match PanelAssets::get("index.html") {
                Some(content) => (
                    HeaderValue::from_static("text/html; charset=utf-8"),
                    Some(content.data),
                ),
                None => (HeaderValue::from_static("text/plain"), None),
            }
        }
    };

    let Some(body) = body else {
        return (StatusCode::NOT_FOUND, "404 Not Found").into_response();
    };

    let mut response = Response::new(Body::from(body));
    let headers = response.headers_mut();
    for (name, value) in security_headers {
        headers.insert(name, value);
    }
    headers.insert(header::CONTENT_TYPE, content_type);
    response
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

async fn api_security_status(
    State(state): State<WebState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
) -> Response {
    let registry = match load_registry_for_web(&state).await {
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

    // Check if the caller is authenticated (either loopback local_token or valid session token)
    let auth_header = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    let custom_token_header = headers.get("x-local-token").and_then(|v| v.to_str().ok());
    let token = if let Some(auth) = auth_header {
        auth.strip_prefix("Bearer ")
            .map(|token| token.trim().to_string())
    } else {
        custom_token_header.map(|custom| custom.trim().to_string())
    };

    let is_authenticated = if let Some(ref tok) = token {
        if is_loopback_addr(peer)
            && !state.local_token.is_empty()
            && constant_time_equal(tok.as_bytes(), state.local_token.as_bytes())
        {
            true
        } else {
            let now = Instant::now();
            let current_hash = sec.password_hash.clone();
            let mut sessions = state.sessions.write().await;
            sessions.retain(|_, session| session.is_valid(now, current_hash.as_deref()));
            sessions.contains_key(tok)
        }
    } else {
        false
    };

    if !is_authenticated {
        // Unauthenticated callers only receive minimal reconnaissance-safe info:
        // whether web access is enabled and whether a password is required.
        return Json(serde_json::json!({
            "web_enabled": sec.web_enabled,
            "password_set": sec.password_hash.is_some(),
            "allow_remote": null,
            "bind_addr": null,
            "port": null,
        }))
        .into_response();
    }

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
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> Response {
    let ip = peer.ip();
    let now = Instant::now();

    // Reject before touching the password at all: an unlimited online guessing
    // loop against a LAN-exposed panel is the cheapest attack available.
    {
        let mut limiter = state.login_limiter.lock().await;
        if let Some(retry_after) = limiter.check(ip, now) {
            let seconds = retry_after.as_secs().max(1);
            return (
                StatusCode::TOO_MANY_REQUESTS,
                [(header::RETRY_AFTER, seconds.to_string())],
                Json(serde_json::json!({
                    "error": "TooManyAttempts",
                    "message": format!("登录尝试过于频繁，请在 {seconds} 秒后重试。")
                })),
            )
                .into_response();
        }
    }

    // Read the registry **without** taking its cross-process lock. Everything the
    // next few checks need is a pure read, and acquiring the lock here is exactly
    // what let the migration's `save()` deadlock against its own guard — see
    // [`mutate_registry_locked_blocking`]. That is not a lost safety property: the
    // migration below re-reads under the lock and re-verifies the password
    // against the value now on disk, so a concurrent change cannot be smuggled
    // past.
    let registry = match load_registry_for_web(&state).await {
        Ok(loaded) => loaded,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };
    // Same DNS-rebinding guard as `auth_middleware`. Login sits outside that
    // layer, so it needs its own: it is the one unauthenticated endpoint that
    // hands out a session token, and a rebound page reaching it from a loopback
    // peer is indistinguishable from the real user by address alone.
    if !registry.web_security().allow_remote
        && let Some(response) = reject_non_loopback_host(&headers)
    {
        return response;
    }
    if !registry.web_security().web_enabled {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "网页访问功能已禁用，请在应用设置中启用"})),
        )
            .into_response();
    }

    let stored_hash = registry
        .web_security()
        .password_hash
        .clone()
        .unwrap_or_default();
    let authenticated = !stored_hash.is_empty() && verify_password(&payload.password, &stored_hash);

    if !authenticated {
        state.login_limiter.lock().await.record_failure(ip, now);
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "密码错误，请重试"})),
        )
            .into_response();
    }

    state.login_limiter.lock().await.record_success(ip);

    let active_hash = if password_hash_needs_upgrade(&stored_hash) {
        match migrate_legacy_password(&state, payload.password.clone()).await {
            Ok(Some(hash)) => hash,
            Ok(None) => {
                // Another process changed or cleared the password between the
                // unlocked verification above and the locked re-read below. The
                // plaintext the caller supplied no longer matches what is stored,
                // so issuing a session from it would hand out a token that dies
                // on its first request. Ask them to log in again instead.
                return (
                    StatusCode::UNAUTHORIZED,
                    Json(serde_json::json!({"error": "访问密码已被修改，请重新登录"})),
                )
                    .into_response();
            }
            Err(RegistryCommitError::Save(detail)) => {
                eprintln!("codex-mp: could not persist upgraded password hash: {detail}");
                // The upgrade did not land, so the next request reloads the legacy
                // hash from disk and the session must be bound to that instead.
                stored_hash
            }
            Err(error) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    Json(serde_json::json!({"error": error.detail()})),
                )
                    .into_response();
            }
        }
    } else {
        stored_hash
    };

    let token = Uuid::new_v4().to_string();
    {
        let mut sessions = state.sessions.write().await;
        insert_session_bounded(
            &mut sessions,
            token.clone(),
            Session {
                created: now,
                last_seen: now,
                // Bind the session to the hash that is actually stored, so a later
                // password change (from this process or another) revokes it.
                //
                // Binding it to the *pre*-upgrade value was a live defect: the
                // migration had already replaced the stored hash, so the very next
                // request compared the session's legacy hash against the new
                // Argon2id one and rejected it. Every installation still on a
                // legacy hash therefore "logged in successfully" and was then
                // refused on its first API call, with nothing in the response to
                // explain why.
                password_hash: Some(active_hash.clone()),
            },
            now,
            Some(active_hash.as_str()),
        );
    }
    Json(LoginResponse { token }).into_response()
}

/// Migrate a pre-Argon2id password hash to the current format under the
/// registry's cross-process lock.
///
/// Yields the hash now stored, or `None` when what is on disk is no longer the
/// legacy hash the caller authenticated against — another process changed or
/// cleared the password in the window between the unlocked verification and this
/// locked re-read, so no session may be issued from the stale plaintext.
async fn migrate_legacy_password(
    state: &WebState,
    password: String,
) -> Result<Option<String>, RegistryCommitError> {
    mutate_registry_locked_blocking(state, move |registry| {
        let Some(current) = registry.web_security().password_hash.clone() else {
            return Err(());
        };
        if !password_hash_needs_upgrade(&current) {
            // A concurrent login already migrated it. Writing the same value back
            // is a no-op, and the caller must bind to *this* hash either way.
            return Ok(current);
        }
        if !verify_password(&password, &current) {
            return Err(());
        }
        let upgraded = hash_password(&password);
        registry.web_security_mut().password_hash = Some(upgraded.clone());
        Ok(upgraded)
    })
    .await
    .map(|committed| committed.ok())
}

async fn api_security_logout(State(state): State<WebState>, headers: HeaderMap) -> Response {
    // Removing the presented token is idempotent and only affects the caller, so
    // this is deliberately available without a valid session.
    let token = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if let Some(token) = token {
        state.sessions.write().await.remove(token);
    }
    Json(serde_json::json!({"status": "ok"})).into_response()
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
    // What the running listener is *actually* bound to, not what the registry
    // says. `serve_web` picks the bind address once at startup, so this is the
    // only truthful basis for deciding whether a restart is needed.
    let bound_addr = *state.listen_addr.read().await;
    let currently_remote = !bound_addr.ip().is_loopback();

    // The read, the decision and the write all happen on one blocking thread
    // under the registry lock. See [`mutate_registry_locked_blocking`]: doing the
    // `save()` from the async worker while the guard sat in this handler used to
    // burn the full 120 s stale window and then steal back the lock from
    // under itself.
    let outcome = mutate_registry_locked_blocking(&state, move |registry| {
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

        // 校验：启用网页访问或允许外网，必须有密码。
        // 远程访问走的是明文 HTTP，密码是唯一的访问屏障，因此这里必须失败关闭。
        // `Err` here means "decline": the registry is left byte-for-byte as it was.
        if (target_web_enabled || target_allow_remote) && !has_existing_pwd && !is_setting_new_pwd {
            return Err(());
        }

        let sec = registry.web_security_mut();
        let new_password_hash = if is_setting_new_pwd {
            let pwd = payload.password.as_deref().unwrap_or_default().trim();
            // `hash_password` owns the trim, so the stored hash and the one
            // `verify_password` computes at login cannot drift apart.
            let new_hash = hash_password(pwd);
            sec.password_hash = Some(new_hash.clone());
            Some(new_hash)
        } else {
            None
        };

        sec.web_enabled = target_web_enabled;
        sec.allow_remote = target_allow_remote;
        // 如果关闭了网页访问，同时重置 allow_remote
        if !sec.web_enabled {
            sec.allow_remote = false;
        }

        Ok(SecurityUpdate {
            password_changed: is_setting_new_pwd,
            new_password_hash,
            web_enabled: sec.web_enabled,
            allow_remote: sec.allow_remote,
        })
    })
    .await;

    let applied = match outcome {
        Ok(Ok(applied)) => applied,
        Ok(Err(())) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "PasswordRequired",
                    "message": "开启网页访问或外网访问必须先设置访问密码"
                })),
            )
                .into_response();
        }
        Err(error) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": error.detail()})),
            )
                .into_response();
        }
    };
    let SecurityUpdate {
        password_changed,
        new_password_hash,
        web_enabled: final_web_enabled,
        allow_remote: final_allow_remote,
    } = applied;

    // A session for the new password is issued only now that the write has
    // landed. Inserting it before the save handed out a token bound to a hash
    // that a failed write would never have persisted — dead on arrival, with
    // nothing on the wire to say why.
    let mut new_token = None;
    if let Some(stored_hash) = new_password_hash {
        let token = Uuid::new_v4().to_string();
        let now = Instant::now();
        let mut sessions = state.sessions.write().await;
        insert_session_bounded(
            &mut sessions,
            token.clone(),
            Session {
                created: now,
                last_seen: now,
                password_hash: Some(stored_hash.clone()),
            },
            now,
            Some(stored_hash.as_str()),
        );
        drop(sessions);
        new_token = Some(token);
    }

    // A password change must revoke every pre-existing session, otherwise the old
    // password keeps granting access until the sessions expire on their own.
    if password_changed || !final_web_enabled {
        let mut sessions = state.sessions.write().await;
        match new_token.as_ref() {
            Some(fresh) => sessions.retain(|token, _| token == fresh),
            None => sessions.clear(),
        }
    }

    // `allow_remote` decides the bind address, and `serve_web` binds once at
    // startup. Saving the flag therefore does *not* change what this process is
    // listening on, in either direction: turning remote access on appears to
    // succeed while the panel stays loopback-only, and — worse — turning it off
    // reports success while the socket is still open to the whole LAN. The
    // response has to say so explicitly rather than let the UI imply the change
    // is live.
    let restart_required = final_allow_remote != currently_remote;
    let restart_message = if restart_required {
        Some(format!(
            "「{}」已保存，但当前进程仍监听在 {}，需要重启 Web 面板后才会生效。",
            if final_allow_remote {
                "允许外网/局域网访问"
            } else {
                "仅限本机访问"
            },
            bound_addr
        ))
    } else {
        None
    };

    Json(serde_json::json!({
        "status": "ok",
        "message": "安全配置已保存",
        "token": new_token,
        "web_enabled": final_web_enabled,
        "allow_remote": final_allow_remote,
        "restart_required": restart_required,
        "restart_message": restart_message,
        "remote_warning": if final_allow_remote {
            Some("远程访问使用明文 HTTP，请仅在可信内网中启用，并确保已设置强密码。")
        } else {
            None
        },
    }))
    .into_response()
}

/// What [`api_security_update`] decided, returned from under the registry lock so
/// the session bookkeeping can happen after the write has actually landed.
struct SecurityUpdate {
    password_changed: bool,
    /// The newly stored hash, or `None` when the password was left alone.
    new_password_hash: Option<String>,
    web_enabled: bool,
    allow_remote: bool,
}

async fn api_router_status(State(state): State<WebState>) -> Response {
    if let Ok(mut guard) = state.supervisor.try_lock() {
        match guard.status().await {
            Ok(status) => Json(status).into_response(),
            Err(e) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response(),
        }
    } else {
        // Supervisor lock is held (e.g. start/restart is in progress).
        // Return starting=true immediately rather than blocking the web client for up to 40 seconds.
        Json(serde_json::json!({
            "running": true,
            "healthy": false,
            "starting": true,
            "last_error": serde_json::Value::Null,
        }))
        .into_response()
    }
}

async fn api_web_healthz() -> Response {
    Json(serde_json::json!({
        "status": "ok",
        "service": "web",
    }))
    .into_response()
}

/// Version shown by the panel. Release builds may provide the product version
/// independently of the Rust workspace version (for example, a prerelease
/// tag passed by the release workflow), while local builds fall back to the
/// Cargo package version.
const fn app_version() -> &'static str {
    match option_env!("CODEX_MP_APP_VERSION") {
        Some(version) if !version.is_empty() => version,
        _ => env!("CARGO_PKG_VERSION"),
    }
}

async fn api_app_version() -> Response {
    Json(serde_json::json!({
        "version": app_version(),
        "core_version": env!("CARGO_PKG_VERSION"),
    }))
    .into_response()
}

async fn api_router_restart(State(state): State<WebState>) -> Response {
    state.router_desired.store(true, Ordering::Release);
    let mut supervisor = state.supervisor.lock().await;
    match supervisor.restart().await {
        Ok(endpoint) => Json(serde_json::json!({
            "status": "ok",
            "base_url": endpoint.base_url,
        }))
        .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({
                "error": e.to_string(),
            })),
        )
            .into_response(),
    }
}

async fn api_router_stop(State(state): State<WebState>) -> Response {
    state.router_desired.store(false, Ordering::Release);
    let mut supervisor = state.supervisor.lock().await;
    match supervisor.stop().await {
        Ok(()) => Json(serde_json::json!({ "status": "stopped" })).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": error.to_string() })),
        )
            .into_response(),
    }
}

async fn api_desktop_status(State(state): State<WebState>) -> Response {
    let manifest = DesktopPaths::manifest_path_for_registry(state.registry_path());
    let result = run_web_blocking(move || {
        let paths = DesktopPaths::discover().map_err(|error| error.to_string())?;
        status_for(&paths, manifest).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(status) => Json(status).into_response(),
        Err(e) => (StatusCode::NOT_FOUND, Json(serde_json::json!({"error": e}))).into_response(),
    }
}

#[derive(Deserialize, Default)]
struct DesktopInstallReq {
    app_server_binary: Option<String>,
    build_metadata: Option<String>,
}

async fn api_desktop_install(
    State(state): State<WebState>,
    // `Option<Json<T>>` rather than `Json<Option<T>>`: the Json extractor rejects
    // an empty body before `Option` is ever considered, and the panel posts this
    // endpoint without a body.
    payload: Option<axum::extract::Json<DesktopInstallReq>>,
) -> Response {
    let registry_path = state.registry_path().to_path_buf();
    let manifest_path = DesktopPaths::manifest_path_for_registry(&registry_path);

    let req = payload.map(|Json(inner)| inner).unwrap_or_default();
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
    let integration_paths = state.integration_paths.clone();
    let result = run_web_blocking(move || {
        let desktop_paths = DesktopPaths::discover().map_err(|error| error.to_string())?;
        let result = codex_mp_desktop::install(&DesktopInstallOptions {
            manifest_path: manifest_path.clone(),
            app_server_binary,
            build_metadata,
            codex_mp_binary,
            endpoint_file,
        })
        .map_err(|error| error.to_string())?;
        let registry = ProviderRegistry::load(&registry_path).map_err(|error| error.to_string())?;
        if let Err(error) = build_and_install(&integration_paths, &registry, &result.catalog_binary)
        {
            let _ = codex_mp_desktop::restore(&desktop_paths, &manifest_path);
            return Err(error.to_string());
        }
        status_for(&desktop_paths, manifest_path).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(status) => Json(status).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

async fn api_desktop_restore(State(state): State<WebState>) -> Response {
    let manifest = DesktopPaths::manifest_path_for_registry(state.registry_path());
    let result = run_web_blocking(move || {
        let paths = DesktopPaths::discover().map_err(|error| error.to_string())?;
        codex_mp_desktop::restore(&paths, manifest).map_err(|error| error.to_string())
    })
    .await;
    match result {
        Ok(restored) => Json(serde_json::json!({"restored": restored})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e})),
        )
            .into_response(),
    }
}

async fn api_catalog_sync(State(state): State<WebState>) -> Response {
    // Must follow the configured registry, otherwise `--registry <path>` would
    // make the panel write the catalog/manifest to the default location and
    // silently diverge from the CLI.
    let paths = state.integration_paths.clone();
    let registry_path = state.registry_path().to_path_buf();
    let catalog_binary = match state.catalog_binary() {
        Ok(b) => b,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response();
        }
    };
    let operation_paths = paths.clone();
    let operation_catalog_binary = catalog_binary.clone();
    let result = run_web_blocking(move || {
        let registry = ProviderRegistry::load(&registry_path).map_err(|error| error.to_string())?;
        build_and_install(&operation_paths, &registry, &operation_catalog_binary)
            .map_err(|error| error.to_string())
    })
    .await;
    if let Err(e) = result {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
        )
            .into_response();
    }
    // `build_and_install` refreshes the official model allow-list in the
    // registry as part of the catalog transaction. A running Router keeps an
    // immutable in-memory snapshot, however, so it must reload *after* the
    // transaction has committed. Reloading only after provider/model edits
    // leaves the custom route working while newly discovered official models
    // still return `model ... was not found` until the next restart.
    let reload_warning = reload_router_if_running(&state).await;
    mutation_response(
        serde_json::json!({
            "catalog_path": clean_verbatim_path(&paths.catalog).display().to_string()
        }),
        reload_warning,
    )
}

async fn api_list_providers(State(state): State<WebState>) -> Response {
    match run_blocking_providers(&state, |providers| {
        providers
            .list_providers()
            .map_err(|error| error.to_string())
    })
    .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

/// Render a mutation result, attaching the reload warning when the running Router
/// did not pick the change up.
///
/// The panel shows this to the user, so a change that was saved but not yet
/// serving is no longer indistinguishable from one that took effect.
fn mutation_response(payload: serde_json::Value, reload_warning: Option<String>) -> Response {
    match reload_warning {
        None => Json(payload).into_response(),
        Some(warning) => {
            let mut body = match payload {
                serde_json::Value::Object(map) => map,
                other => {
                    let mut map = serde_json::Map::new();
                    map.insert("result".into(), other);
                    map
                }
            };
            body.insert(
                "router_reload_warning".into(),
                serde_json::Value::String(warning),
            );
            Json(serde_json::Value::Object(body)).into_response()
        }
    }
}

/// Ask a running Router to reload the registry after a panel mutation.
///
/// The registry file is already saved, so a later Router start picks the change
/// up regardless. But a *failed* reload means a currently-running Router keeps
/// serving the previous revision: the panel reports success while the model the
/// user just changed returns 404. That used to be discarded with `let _ =`, so
/// nothing anywhere recorded it. The failure is now logged and surfaced to the
/// caller's response so the operator knows the Router must be restarted.
async fn reload_router_if_running(state: &WebState) -> Option<String> {
    let supervisor = state.supervisor.lock().await;
    match supervisor.reload().await {
        Ok(()) => None,
        Err(error) => {
            eprintln!(
                "codex-mp: the registry change was saved, but the running router did not \
                 reload it ({error}); restart the router to apply it"
            );
            Some(error.to_string())
        }
    }
}

#[derive(Deserialize)]
struct AddProviderReq {
    name: String,
    base_url: String,
    /// Defaults to `chat_completions`: an omitted protocol must not silently
    /// select the Responses API, which almost no third-party gateway implements.
    #[serde(default)]
    protocol: ProviderProtocol,
    api_key: Option<String>,
    #[serde(default)]
    auth_strategy: Option<AuthStrategy>,
}

fn normalize_optional_auth_strategy(
    strategy: Option<AuthStrategy>,
) -> Result<Option<AuthStrategy>, String> {
    match strategy {
        None => Ok(None),
        Some(AuthStrategy::Header { name }) => {
            let name = name.trim().to_owned();
            if name.is_empty() {
                return Err("自定义认证方式需要填写请求头名称".to_owned());
            }
            header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| format!("无效的请求头名称：{name}"))?;
            Ok(Some(AuthStrategy::Header { name }))
        }
        Some(other) => Ok(Some(other)),
    }
}

fn normalize_auth_strategy(strategy: Option<AuthStrategy>) -> Result<AuthStrategy, String> {
    Ok(normalize_optional_auth_strategy(strategy)?.unwrap_or_default())
}

async fn api_add_provider(
    State(state): State<WebState>,
    Json(payload): Json<AddProviderReq>,
) -> Response {
    let name = payload.name;
    let base_url = payload.base_url;
    let protocol = payload.protocol;
    let api_key = payload.api_key.map(SecretString::from);
    let auth_strategy = match normalize_auth_strategy(payload.auth_strategy) {
        Ok(strategy) => strategy,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };
    // Offloaded: this writes the API key to the OS keyring, which panics if run
    // on an async worker thread (see `run_blocking_providers`).
    match run_blocking_providers(&state, move |providers| {
        providers
            .add_provider_with_auth(&name, &base_url, protocol, auth_strategy, api_key)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(summary) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!(summary), reload_warning)
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
    #[serde(default)]
    auth_strategy: Option<AuthStrategy>,
}

async fn api_edit_provider(
    State(state): State<WebState>,
    Json(payload): Json<EditProviderReq>,
) -> Response {
    let id = payload.id;
    let name = payload.name;
    let base_url = payload.base_url;
    let protocol = payload.protocol;
    let enabled = payload.enabled;
    let api_key = payload.api_key.map(SecretString::from);
    let auth_strategy = match normalize_optional_auth_strategy(payload.auth_strategy) {
        Ok(strategy) => strategy,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": error})),
            )
                .into_response();
        }
    };
    // Offloaded: may write the API key to the OS keyring.
    match run_blocking_providers(&state, move |providers| {
        providers
            .edit_provider_with_auth(
                &id,
                name,
                base_url,
                protocol,
                enabled,
                auth_strategy,
                api_key,
            )
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(summary) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!(summary), reload_warning)
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
    let id = payload.id;
    let purge_credential = payload.purge_credential;
    // Offloaded: purging reads and deletes keyring entries.
    match run_blocking_providers(&state, move |providers| {
        providers
            .remove_provider(&id, purge_credential)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(()) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!({"status": "ok"}), reload_warning)
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
    let Some(_network_slot) = try_web_network_slot() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "model discovery is at its bounded network capacity; retry shortly"
            })),
        )
            .into_response();
    };
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
    #[serde(default)]
    capabilities: Option<ModelCapabilities>,
    #[serde(default)]
    text: Option<bool>,
    #[serde(default, alias = "image")]
    images: Option<bool>,
    #[serde(default, alias = "file")]
    files: Option<bool>,
    #[serde(default)]
    audio: Option<bool>,
    #[serde(default)]
    video: Option<bool>,
    #[serde(default)]
    tools: Option<bool>,
    #[serde(default)]
    streaming: Option<bool>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    reasoning_levels: Option<Vec<String>>,
}

async fn api_add_model(
    State(state): State<WebState>,
    Json(payload): Json<AddModelReq>,
) -> Response {
    let mut capabilities = payload.capabilities.unwrap_or_default();
    apply_capability_fields(
        &mut capabilities,
        payload.text,
        payload.images,
        payload.files,
        payload.audio,
        payload.video,
        payload.tools,
        payload.streaming,
        payload.reasoning,
    );
    let provider_id = payload.provider_id;
    let upstream_model_id = payload.upstream_model_id;
    let display_name = payload.display_name;
    let context_window = payload.context_window;
    let reasoning_levels = payload.reasoning_levels.unwrap_or_default();
    match run_blocking_providers(&state, move |providers| {
        providers
            .add_model_with_metadata(
                &provider_id,
                &upstream_model_id,
                &display_name,
                context_window,
                capabilities,
                reasoning_levels,
            )
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(summary) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!(summary), reload_warning)
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
    let provider_id = payload.provider_id;
    let discovered = payload.discovered;
    let selected_ids = payload.selected_ids;
    match run_blocking_providers(&state, move |providers| {
        providers
            .import_models(&provider_id, &discovered, &selected_ids)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(summaries) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!(summaries), reload_warning)
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
    #[serde(default)]
    capabilities: Option<ModelCapabilities>,
    #[serde(default)]
    text: Option<bool>,
    #[serde(default, alias = "image")]
    images: Option<bool>,
    #[serde(default, alias = "file")]
    files: Option<bool>,
    #[serde(default)]
    audio: Option<bool>,
    #[serde(default)]
    video: Option<bool>,
    #[serde(default)]
    tools: Option<bool>,
    #[serde(default)]
    streaming: Option<bool>,
    #[serde(default)]
    reasoning: Option<bool>,
    #[serde(default)]
    reasoning_levels: Option<Vec<String>>,
    /// Replaces the system prompt Codex sends for this model.
    ///
    /// A JSON `null` and an omitted field both deserialize to `None` here, which
    /// is why clearing an override needs its own flag rather than
    /// `Option<Option<_>>` — the same reason `clear_context_window` exists.
    #[serde(default)]
    system_prompt_override: Option<String>,
    #[serde(default)]
    clear_system_prompt: bool,
}

async fn api_edit_model(
    State(state): State<WebState>,
    Json(payload): Json<EditModelReq>,
) -> Response {
    let capability_fields_present = payload.text.is_some()
        || payload.images.is_some()
        || payload.files.is_some()
        || payload.audio.is_some()
        || payload.video.is_some()
        || payload.tools.is_some()
        || payload.streaming.is_some()
        || payload.reasoning.is_some();
    let mut capabilities = payload
        .capabilities
        .or_else(|| capability_fields_present.then(ModelCapabilities::default));
    if let Some(capabilities_value) = capabilities.as_mut() {
        apply_capability_fields(
            capabilities_value,
            payload.text,
            payload.images,
            payload.files,
            payload.audio,
            payload.video,
            payload.tools,
            payload.streaming,
            payload.reasoning,
        );
    }
    let edit = ModelEdit {
        display_name: payload.display_name,
        context_window: if payload.clear_context_window {
            Some(None)
        } else {
            payload.context_window.map(Some)
        },
        capabilities,
        reasoning_levels: payload.reasoning_levels,
        system_prompt_override: if payload.clear_system_prompt {
            Some(None)
        } else {
            payload.system_prompt_override.map(Some)
        },
    };
    let logical_model_id = payload.logical_model_id;
    match run_blocking_providers(&state, move |providers| {
        providers
            .edit_model(&logical_model_id, edit)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(summary) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!(summary), reload_warning)
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// These fields are kept as individual optional JSON properties for backwards
// compatibility with the panel API. Grouping them into a transport struct
// would change the wire shape, so keep the small, explicit adapter here.
#[allow(clippy::too_many_arguments)]
fn apply_capability_fields(
    capabilities: &mut ModelCapabilities,
    text: Option<bool>,
    images: Option<bool>,
    files: Option<bool>,
    audio: Option<bool>,
    video: Option<bool>,
    tools: Option<bool>,
    streaming: Option<bool>,
    reasoning: Option<bool>,
) {
    if let Some(value) = text {
        capabilities.text = value;
    }
    if let Some(value) = images {
        capabilities.images = value;
    }
    if let Some(value) = files {
        capabilities.files = value;
    }
    if let Some(value) = audio {
        capabilities.audio = value;
    }
    if let Some(value) = video {
        capabilities.video = value;
    }
    if let Some(value) = tools {
        capabilities.tools = value;
    }
    if let Some(value) = streaming {
        capabilities.streaming = value;
    }
    if let Some(value) = reasoning {
        capabilities.reasoning = value;
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
    let logical_model_id = payload.logical_model_id;
    let enabled = payload.enabled;
    match run_blocking_providers(&state, move |providers| {
        providers
            .set_model_enabled(&logical_model_id, enabled)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(summary) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!(summary), reload_warning)
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct ShowModelReq {
    logical_model_id: String,
}

/// Read one model back in full, including its system-prompt override.
///
/// A `POST` with the id in the body rather than a `GET` with it in the path: a
/// logical model id is `provider/model` and contains a `/`, so it cannot be a
/// single path segment — the same reason every other model mutation here takes it
/// in a body. Nothing is written.
///
/// `ModelSummary` only carries a `has_system_prompt_override` flag, so without
/// this the panel could set and clear an override but never show the user what is
/// currently in force — editing it meant guessing at what was already there.
async fn api_show_model(
    State(state): State<WebState>,
    Json(payload): Json<ShowModelReq>,
) -> Response {
    let logical_model_id = payload.logical_model_id;
    // `run_blocking_providers` fixes its error channel to `String`, so the
    // not-found classification travels inside the success channel instead.
    // Matching on the error *variant* rather than sniffing the rendered message
    // matters: a substring test would silently start answering 400s the moment the
    // wording changes, and the panel would tell the user the model is gone when it
    // only typed the id wrong.
    let outcome = run_blocking_providers(&state, move |providers| {
        Ok(match providers.show_model(&logical_model_id) {
            Ok(detail) => Ok(detail),
            Err(error) => {
                let not_found = matches!(
                    error,
                    codex_mp_manager::ProviderManagerError::Core(
                        codex_mp_core::CoreError::ModelNotFound(_)
                            | codex_mp_core::CoreError::InvalidModelId(_)
                    )
                );
                Err((not_found, error.to_string()))
            }
        })
    })
    .await;
    match outcome {
        Ok(Ok(detail)) => Json(detail).into_response(),
        Ok(Err((not_found, message))) => {
            let status = if not_found {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, Json(serde_json::json!({"error": message}))).into_response()
        }
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": error})),
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
    let logical_model_id = payload.logical_model_id;
    match run_blocking_providers(&state, move |providers| {
        providers
            .remove_model(&logical_model_id)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(()) => {
            let reload_warning = reload_router_if_running(&state).await;
            mutation_response(serde_json::json!({"status": "ok"}), reload_warning)
        }
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// -------------------------------- 官方账号与额度 API -------------------------------- //

/// Run a synchronous account operation off the async runtime.
///
/// The account manager is synchronous by design, but it reads and writes the OS
/// keyring. On Linux that reaches the Secret Service through `zbus`, which
/// bridges to a synchronous API by calling `tokio::runtime::Runtime::block_on`,
/// so invoking it directly from an axum handler panics the worker with "Cannot
/// start a runtime from within a runtime" — exactly the failure that made
/// `codex-mp provider add` and every custom-provider router request abort. It
/// also keeps a slow keyring daemon from stalling unrelated requests.
///
/// Why a [`mutate_registry_locked_blocking`] call reached no commit.
enum RegistryCommitError {
    /// The registry could not be read under its own lock; nothing was written.
    Load(String),
    /// The mutation ran but the result could not be persisted.
    Save(String),
    /// The blocking worker itself failed or timed out.
    Overload(String),
}

impl RegistryCommitError {
    fn detail(&self) -> &str {
        match self {
            Self::Load(detail) | Self::Save(detail) | Self::Overload(detail) => detail,
        }
    }
}

/// What one [`mutate_registry_locked_blocking`] worker actually did.
///
/// A flat enum rather than a nested `Result` because [`run_web_blocking`] fixes
/// its error type to `String`: the three failure modes have to travel as values,
/// or `Load` and `Save` would be indistinguishable from an overload by the time
/// the handler sees them.
enum RegistryCommit<T, E> {
    /// The operation ran and its result is now on disk.
    Written(T),
    /// The operation declined (`Err`) — the registry was left untouched.
    Declined(E),
    LoadFailed(String),
    SaveFailed(String),
}

/// Read the registry under its cross-process lock, mutate it, and save it — all
/// on one blocking thread.
///
/// The single-thread requirement is the entire point of this helper, and the
/// shape it replaces was a live defect. `FileLock` reentrancy is keyed by
/// `(path, thread id)` and `ProviderRegistry::save` locks the very path it was
/// loaded from. The old handlers called `load_locked()` on a `spawn_blocking`
/// thread, parked the returned guard in the async handler, and then called
/// `save()` **from the async worker** — which presented one logical critical
/// section as two different holders. `save()` then saw the lock file already
/// present, polled every 25 ms for the full `FILE_LOCK_STALE` window (120 s),
/// and finally reclaimed it by renaming a lock file the same request still owned.
/// Two consequences, the first of them measured:
///
/// * every `POST /api/v1/security/update` hung for two minutes before answering,
///   so the panel's security form looked frozen;
/// * the reclaim voided the mutual exclusion the guard existed to provide. A
///   concurrent CLI `provider add` could be inside the same critical section,
///   which is precisely the lost update the lock was added to prevent.
///
/// Keeping acquisition, mutation and save on one thread makes the reentrancy
/// check hit, so `save()` recognises its own held lock and returns immediately.
/// The blocking thread is still required for a different reason: `FileLock`
/// waits by *sleeping the current thread*, and doing that on an async worker
/// stalls the whole runtime (measured: a contended acquisition froze a
/// single-threaded executor for 507 ms).
///
/// The operation returns `Err` to decline the write, in which case the registry is
/// left byte-for-byte as it was — a rejected request must not rewrite the file.
async fn mutate_registry_locked_blocking<T, E, F>(
    state: &WebState,
    operation: F,
) -> Result<Result<T, E>, RegistryCommitError>
where
    F: FnOnce(&mut ProviderRegistry) -> Result<T, E> + Send + 'static,
    T: Send + 'static,
    E: Send + 'static,
{
    let path = state.registry_path().to_path_buf();
    run_web_blocking(move || {
        let (mut registry, _lock) = match ProviderRegistry::load_locked(&path) {
            Ok(loaded) => loaded,
            Err(error) => return Ok(RegistryCommit::LoadFailed(error.to_string())),
        };
        let outcome = match operation(&mut registry) {
            Ok(outcome) => outcome,
            Err(declined) => return Ok(RegistryCommit::Declined(declined)),
        };
        match registry.save() {
            Ok(()) => Ok(RegistryCommit::Written(outcome)),
            Err(error) => Ok(RegistryCommit::SaveFailed(error.to_string())),
        }
    })
    .await
    .map_err(RegistryCommitError::Overload)
    .and_then(|commit| match commit {
        RegistryCommit::Written(outcome) => Ok(Ok(outcome)),
        RegistryCommit::Declined(declined) => Ok(Err(declined)),
        RegistryCommit::LoadFailed(detail) => Err(RegistryCommitError::Load(detail)),
        RegistryCommit::SaveFailed(detail) => Err(RegistryCommitError::Save(detail)),
    })
}

/// Run a synchronous, keyring-touching closure off the async runtime.
///
/// Same reasoning as [`run_blocking_account`]: the `ProviderManager` mutation
/// methods persist credentials to the OS keyring, and on Linux that path
/// (`zbus`) bridges to a synchronous API with
/// `tokio::runtime::Runtime::block_on`. Calling one directly from an axum
/// handler panicked the worker with "Cannot start a runtime from within a
/// runtime" — verified live: `POST /api/v1/providers/add` returned a dropped
/// connection (HTTP 000) and killed a tokio worker.
///
/// `state.providers` is an `Arc`, so the operation can own a clone.
async fn run_blocking_providers<T, F>(state: &WebState, operation: F) -> Result<T, String>
where
    F: FnOnce(Arc<codex_mp_manager::ProviderManager>) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let providers = state.providers.clone();
    run_web_blocking(move || operation(providers)).await
}

async fn run_blocking_account<T, F>(state: &WebState, operation: F) -> Result<T, String>
where
    F: FnOnce(&codex_mp_manager::AccountManager) -> Result<T, String> + Send + 'static,
    T: Send + 'static,
{
    let accounts = state.accounts.clone();
    run_web_blocking(move || operation(&accounts)).await
}

async fn api_list_accounts(State(state): State<WebState>) -> Response {
    match run_blocking_account(&state, |accounts| {
        accounts.list_accounts().map_err(|e| e.to_string())
    })
    .await
    {
        Ok(list) => Json(list).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_active_account(State(state): State<WebState>) -> Response {
    match run_blocking_account(&state, |accounts| {
        accounts.check_active_status().map_err(|e| e.to_string())
    })
    .await
    {
        Ok(status) => Json(status).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

// ---------------- auth.json switch backups ---------------- //

/// List the reversible `auth.bak-switch-*` copies of `auth.json`.
///
/// Every account switch writes one, and until now nothing could read them back:
/// a user who switched to the wrong account, or whose switch landed a token Codex
/// had already rotated, had to edit `auth.json` by hand to recover. Descriptions
/// come from `AuthBackupInfo`, which carries an identity and a "is this
/// restorable" flag but never a token.
async fn api_list_auth_backups(State(state): State<WebState>) -> Response {
    match run_blocking_account(&state, |accounts| {
        accounts.list_auth_backups().map_err(|e| e.to_string())
    })
    .await
    {
        Ok(backups) => Json(backups).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

#[derive(Deserialize)]
struct RestoreBackupReq {
    /// The `name` field of an entry from `/accounts/backups`. A bare file name,
    /// never a path: the manager re-validates its shape and its location.
    name: String,
    /// Restart the running Codex app-server afterwards. It holds the previous
    /// session in memory, so a restore without a restart is not visible to the
    /// editor until the user quits it themselves.
    restart_codex: Option<bool>,
}

async fn api_restore_auth_backup(
    State(state): State<WebState>,
    Json(payload): Json<RestoreBackupReq>,
) -> Response {
    let name = payload.name;
    let restart_codex = payload.restart_codex.unwrap_or(false);
    // The failure classification has to survive the blocking hop, so the outcome
    // travels as a JSON value and the HTTP status is derived from its `code`.
    let outcome = run_blocking_account(&state, move |accounts| {
        Ok(match accounts.restore_auth_backup(&name) {
            Ok(backup) => {
                let restart_report = if restart_codex {
                    Some(
                        accounts
                            .restart_codex_processes()
                            .map_err(|error| error.to_string())?,
                    )
                } else {
                    None
                };
                serde_json::json!({ "backup": backup, "restart_report": restart_report })
            }
            Err(error) => serde_json::json!({
                "code": error.public_code(),
                "error": error.public_message(),
            }),
        })
    })
    .await;
    let body = match outcome {
        Ok(body) => body,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e})),
            )
                .into_response();
        }
    };
    let status = match body.get("code").and_then(|code| code.as_str()) {
        Some("backup_not_found") => StatusCode::NOT_FOUND,
        Some("backup_invalid") => StatusCode::BAD_REQUEST,
        Some(_) => StatusCode::INTERNAL_SERVER_ERROR,
        None => StatusCode::OK,
    };
    (status, Json(body)).into_response()
}

/// Delete every switch backup. These are complete copies of the user's OAuth
/// token set sitting in the config directory, so "I am done recovering" has to be
/// an action they can take. `auth.json` itself is never touched.
async fn api_clean_auth_backups(State(state): State<WebState>) -> Response {
    match run_blocking_account(&state, |accounts| Ok(accounts.remove_auth_backups())).await {
        Ok(removed) => Json(serde_json::json!({ "removed": removed })).into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": e})),
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
    let name = payload.name;
    match run_blocking_account(&state, move |accounts| {
        let account = accounts
            .capture_current_auth(name)
            .map_err(|e| e.to_string())?;
        // Summarise inside the blocking task too, so the returned value never
        // carries the OAuth refresh token out of this scope.
        Ok(accounts.summarise_account(&account))
    })
    .await
    {
        // Never return the ManagedAccount itself: it embeds the OAuth refresh
        // token, which must not reach a browser.
        Ok(summary) => Json(summary).into_response(),
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
        if aj.get("auth_mode").and_then(|value| value.as_str()) != Some("chatgpt") {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "auth_json must use the official `chatgpt` auth_mode"
                })),
            )
                .into_response();
        }
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

    let name = payload.name;
    match run_blocking_account(&state, move |accounts| {
        let account = accounts
            .import_or_update_account(tokens, last_refresh, name)
            .map_err(|e| e.to_string())?;
        Ok(accounts.summarise_account(&account))
    })
    .await
    {
        Ok(summary) => Json(summary).into_response(),
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
    let account_id = payload.account_id;
    let restart_codex = payload.restart_codex;
    match run_blocking_account(&state, move |accounts| {
        let account = accounts
            .switch_to_account(&account_id)
            .map_err(|e| e.to_string())?;
        let restart_report = if restart_codex {
            Some(
                accounts
                    .restart_codex_processes()
                    .map_err(|e| e.to_string())?,
            )
        } else {
            None
        };
        Ok((accounts.summarise_account(&account), restart_report))
    })
    .await
    {
        Ok((account, restart_report)) => Json(serde_json::json!({
            "account": account,
            "restart_report": restart_report,
        }))
        .into_response(),
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
    let account_id = payload.account_id;
    let name = payload.name;
    match run_blocking_account(&state, move |accounts| {
        accounts
            .rename_account(&account_id, &name)
            .map_err(|e| e.to_string())
    })
    .await
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
    let account_id = payload.account_id;
    match run_blocking_account(&state, move |accounts| {
        accounts
            .delete_account(&account_id)
            .map_err(|e| e.to_string())
    })
    .await
    {
        Ok(deleted) => Json(serde_json::json!({"deleted": deleted})).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": e.to_string()})),
        )
            .into_response(),
    }
}

async fn api_restart_codex(State(state): State<WebState>) -> Response {
    match run_blocking_account(&state, |accounts| {
        accounts
            .restart_codex_processes()
            .map_err(|e| e.to_string())
    })
    .await
    {
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
    let Some(_network_slot) = try_web_network_slot() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({
                "error": "account usage checks are at their bounded network capacity; retry shortly"
            })),
        )
            .into_response();
    };
    let result = state.accounts.fetch_usage(&account_id).await;
    match result {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => {
            let code = error.public_code();
            let status = match code {
                "reauth_required" => StatusCode::CONFLICT,
                "usage_rate_limited" => StatusCode::TOO_MANY_REQUESTS,
                // This is the managed account's OAuth status, not the panel's
                // browser session. A 401 would make the SPA open its admin
                // login dialog and hide the real account repair action.
                "usage_unauthorized" => StatusCode::CONFLICT,
                _ => StatusCode::BAD_GATEWAY,
            };
            (
                status,
                Json(serde_json::json!({
                    "error": code,
                    "message": error.public_message(),
                    "account_id": account_id,
                })),
            )
                .into_response()
        }
    }
}

// -------------------------------- Web Server 运行入口 -------------------------------- //

fn spawn_router_recovery_monitor(state: &WebState) {
    let supervisor = state.supervisor.clone();
    let desired = state.router_desired.clone();
    tokio::spawn(async move {
        let mut recovery_backoff = RouterRecoveryBackoff::default();
        let mut unhealthy_rounds = 0_u8;
        loop {
            if !desired.load(Ordering::Acquire) {
                unhealthy_rounds = 0;
                recovery_backoff.reset();
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }

            let now = Instant::now();
            let result = {
                let mut guard = supervisor.lock().await;
                match guard.status().await {
                    Ok(status) if status.running && status.healthy => {
                        unhealthy_rounds = 0;
                        recovery_backoff.reset();
                        Ok(false)
                    }
                    Ok(status) if status.running => {
                        unhealthy_rounds = unhealthy_rounds.saturating_add(1);
                        if unhealthy_rounds < 3 || !recovery_backoff.due(now) {
                            Ok(false)
                        } else {
                            unhealthy_rounds = 0;
                            guard
                                .restart()
                                .await
                                .map(|_| true)
                                .map_err(|error| error.to_string())
                        }
                    }
                    Ok(_) if recovery_backoff.due(now) => guard
                        .start()
                        .await
                        .map(|_| true)
                        .map_err(|error| error.to_string()),
                    Ok(_) => Ok(false),
                    Err(error) => Err(error.to_string()),
                }
            };

            match result {
                Ok(true) => {
                    recovery_backoff.attempted(Instant::now());
                    eprintln!("codex-mp: Router supervisor recovered the Router process");
                }
                Ok(false) => {}
                Err(error) => {
                    let retry_delay = recovery_backoff.retry_after();
                    recovery_backoff.attempted(Instant::now());
                    eprintln!(
                        "codex-mp: Router health/recovery attempt failed ({error}); retrying in {}s",
                        retry_delay.as_secs(),
                    );
                    tokio::time::sleep(retry_delay).await;
                    continue;
                }
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    });
}

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

    // Remote access serves plain HTTP, so the password is the only thing standing
    // between the LAN and the whole control plane. Refuse to bind a non-loopback
    // address without one.
    if allow_remote && sec.password_hash.is_none() {
        return Err(WebError::RemoteNotAllowedWithoutPassword);
    }
    if !sec.web_enabled && sec.password_hash.is_none() {
        eprintln!(
            "codex-mp: web access is disabled and no password is set; \
             only the loopback desktop token can reach the panel."
        );
    }

    let bind_ip: IpAddr = if allow_remote {
        IpAddr::V4(Ipv4Addr::UNSPECIFIED)
    } else {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    };

    let bind_addr = SocketAddr::new(bind_ip, port);
    // Name the address in the failure. A bare `Address already in use (os error
    // 98)` left the user unable to tell which port conflicted, or that the panel
    // was even the thing that failed to bind.
    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .map_err(|error| WebError::Bind {
            addr: bind_addr.to_string(),
            source: error,
        })?;
    let local_addr = listener.local_addr()?;

    // An empty token would make every request carrying an empty Bearer value
    // authenticate successfully, so fall back to a generated one.
    let token = match local_token {
        Some(t) if !t.trim().is_empty() => t,
        _ => Uuid::new_v4().to_string(),
    };
    let state = WebState::with_local_token(
        registry_path,
        endpoint_file,
        router_bin,
        codex_bin,
        token.clone(),
    );
    *state.listen_addr.write().await = local_addr;

    // Keep the panel usable while supervising the Router. Startup failures and
    // later child/health failures are retried with backoff; an explicit Stop
    // request flips `router_desired` so the monitor does not immediately undo a
    // deliberate operator action.
    spawn_router_recovery_monitor(&state);

    println!("============================================================");
    println!(" Codex MultiProvider Web 控制面板已启动！");
    println!(" 监听地址: http://{local_addr}");
    if allow_remote {
        println!(" 外网/局域网访问: [已开启]");
        println!(" ! 安全警告: 远程访问使用明文 HTTP，密码与请求内容会以明文经过网络。");
        println!(" ! 请仅在可信内网中启用，并确保访问密码足够强壮。");
        println!(" ! 需要跨公网访问时，请改用 SSH 端口转发或 VPN 隧道。");
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
    // ConnectInfo carries the peer address into the auth middleware and the login
    // rate limiter; without it the loopback-only token rule cannot be enforced.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_credentials::{CredentialStore, CredentialStoreError, MemoryCredentialStore};
    use std::fs;
    use tempfile::tempdir;

    fn memory_backed_accounts(
        store_path: std::path::PathBuf,
        codex_home: impl Into<PathBuf>,
    ) -> AccountManager {
        // The production store writes tokens to the OS keyring, which a test
        // runner has no business touching.
        AccountManager::with_credential_store(
            store_path,
            codex_home,
            Arc::new(MemoryCredentialStore::default()),
        )
    }

    #[test]
    fn test_password_hash_and_verify() {
        let raw = "super-secret-123";
        let hash = hash_password(raw);
        assert!(hash.starts_with(PASSWORD_HASH_PREFIX));
        assert!(verify_password(raw, &hash));
        assert!(!verify_password("wrong-password", &hash));
        assert!(!password_hash_needs_upgrade(&hash));
    }

    #[test]
    fn router_recovery_backoff_grows_after_successful_attempts_and_resets_when_healthy() {
        let start = Instant::now();
        let mut backoff = RouterRecoveryBackoff::default();

        assert!(backoff.due(start));
        assert_eq!(backoff.retry_after(), ROUTER_RECOVERY_INITIAL_BACKOFF);

        backoff.attempted(start);
        assert!(!backoff.due(start + Duration::from_secs(1)));
        assert!(backoff.due(start + ROUTER_RECOVERY_INITIAL_BACKOFF));
        assert_eq!(backoff.retry_after(), Duration::from_secs(4));

        backoff.attempted(start + ROUTER_RECOVERY_INITIAL_BACKOFF);
        assert_eq!(backoff.retry_after(), Duration::from_secs(8));

        for attempt in 0..8 {
            backoff.attempted(start + Duration::from_secs(10 + attempt));
        }
        assert_eq!(backoff.retry_after(), ROUTER_RECOVERY_MAX_BACKOFF);

        backoff.reset();
        assert!(backoff.due(start));
        assert_eq!(backoff.retry_after(), ROUTER_RECOVERY_INITIAL_BACKOFF);
    }

    #[tokio::test]
    async fn web_concurrency_overload_is_shed_with_503() {
        use axum::extract::State as ExtractState;
        use tokio::sync::{Barrier, Semaphore};

        #[derive(Clone)]
        struct ProbeState {
            started: Arc<Barrier>,
            release: Arc<Semaphore>,
        }

        async fn hold_request(ExtractState(state): ExtractState<ProbeState>) -> &'static str {
            state.started.wait().await;
            let _permit = state
                .release
                .acquire()
                .await
                .expect("probe release semaphore must stay open");
            "ok"
        }

        let state = ProbeState {
            // The test task is the extra barrier participant. This lets it
            // wait until every one of the 128 handler calls has acquired a
            // permit before issuing the overload probe.
            started: Arc::new(Barrier::new(WEB_MAX_IN_FLIGHT_REQUESTS + 1)),
            release: Arc::new(Semaphore::new(0)),
        };
        let started = state.started.clone();
        let release = state.release.clone();
        let app = Router::new()
            .route("/probe", get(hold_request))
            .with_state(state.clone())
            .layer(
                tower::ServiceBuilder::new()
                    .layer(axum::error_handling::HandleErrorLayer::new(
                        handle_web_layer_error,
                    ))
                    .layer(LoadShedLayer::new())
                    .layer(ConcurrencyLimitLayer::new(WEB_MAX_IN_FLIGHT_REQUESTS)),
            );

        let mut held_requests = Vec::with_capacity(WEB_MAX_IN_FLIGHT_REQUESTS);
        for _ in 0..WEB_MAX_IN_FLIGHT_REQUESTS {
            let service = app.clone();
            held_requests.push(tokio::spawn(async move {
                tower::ServiceExt::oneshot(
                    service,
                    Request::builder()
                        .uri("/probe")
                        .body(Body::empty())
                        .expect("probe request must build"),
                )
                .await
                .expect("held probe request must return a response")
            }));
        }

        tokio::time::timeout(Duration::from_secs(1), started.wait())
            .await
            .expect("all in-flight probe handlers must start before overload is tested");

        // All permits are held inside `hold_request`, so this request must not
        // wait for one. It should be rejected by LoadShedLayer promptly.
        let overloaded = tokio::time::timeout(
            Duration::from_secs(1),
            tower::ServiceExt::oneshot(
                app,
                Request::builder()
                    .uri("/probe")
                    .body(Body::empty())
                    .expect("overload probe request must build"),
            ),
        )
        .await;

        release.add_permits(WEB_MAX_IN_FLIGHT_REQUESTS);
        for request in held_requests {
            let response = request.await.expect("held probe task must not panic");
            assert_eq!(response.status(), StatusCode::OK);
        }

        let response = overloaded
            .expect("an overloaded request must return without waiting")
            .expect("overloaded request must be converted to a response");
        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("overload response body must be bounded");
        let payload: serde_json::Value =
            serde_json::from_slice(&body).expect("overload response must be JSON");
        assert_eq!(payload["error"], "ServiceUnavailable");
    }

    #[test]
    fn test_password_hash_is_salted() {
        // Two hashes of the same password must differ, otherwise identical
        // passwords are trivially detectable in the registry.
        let a = hash_password("same-password");
        let b = hash_password("same-password");
        assert_ne!(a, b);
        assert!(verify_password("same-password", &a));
        assert!(verify_password("same-password", &b));
    }

    #[test]
    fn test_legacy_hash_still_verifies_and_is_flagged_for_upgrade() {
        let salt = "abcd1234";
        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(b":");
        hasher.update(b"legacy-password");
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let legacy = format!("{salt}${digest}");

        assert!(verify_password("legacy-password", &legacy));
        assert!(!verify_password("nope", &legacy));
        assert!(password_hash_needs_upgrade(&legacy));
    }

    #[test]
    fn test_verify_password_rejects_malformed_input() {
        assert!(!verify_password("x", ""));
        assert!(!verify_password("x", "no-separator"));
        assert!(!verify_password(
            "x",
            &format!("{PASSWORD_HASH_PREFIX}not-a-phc-string")
        ));
        assert!(!verify_password(
            "x",
            &format!("{PASSWORD_HASH_PREFIX}!hashing-failed")
        ));
    }

    #[test]
    fn test_login_rate_limiter_blocks_then_recovers() {
        let mut limiter = LoginRateLimiter::default();
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let start = Instant::now();

        for attempt in 0..LOGIN_MAX_ATTEMPTS_PER_IP {
            assert!(
                limiter
                    .check(ip, start + Duration::from_millis(attempt as u64))
                    .is_none(),
                "attempt {attempt} should still be allowed"
            );
            limiter.record_failure(ip, start + Duration::from_millis(attempt as u64));
        }

        // The budget is now exhausted.
        assert!(limiter.check(ip, start + Duration::from_secs(1)).is_some());

        // ...and a different source is unaffected.
        let other: IpAddr = "10.0.0.5".parse().unwrap();
        assert!(
            limiter
                .check(other, start + Duration::from_secs(1))
                .is_none()
        );

        // A successful login clears the per-IP window.
        limiter.record_success(ip);
        assert!(limiter.check(ip, start + Duration::from_secs(1)).is_none());
    }

    #[test]
    fn test_session_expires() {
        let now = Instant::now();
        let hash = Some("hash-1");
        assert!(
            Session {
                created: now,
                last_seen: now,
                password_hash: hash.map(str::to_owned),
            }
            .is_valid(now, hash)
        );

        // Regression: the session map was unbounded, and expired entries were
        // only swept when some other request happened to arrive. Repeated logins
        // could therefore grow it for ever. The cap must evict (so a legitimate
        // login still succeeds) rather than reject.
        let mut sessions: HashMap<String, Session> = HashMap::new();
        let session_hash = "hash-a";
        for n in 0..(MAX_SESSIONS + 50) {
            let session = Session {
                created: now,
                last_seen: now + Duration::from_millis(n as u64),
                password_hash: Some(session_hash.to_owned()),
            };
            insert_session_bounded(
                &mut sessions,
                format!("token-{n}"),
                session,
                now,
                Some(session_hash),
            );
        }
        assert_eq!(
            sessions.len(),
            MAX_SESSIONS,
            "the session map must not exceed its cap"
        );
        assert!(
            sessions.contains_key(&format!("token-{}", MAX_SESSIONS + 49)),
            "the most recent session must survive eviction"
        );
        assert!(
            !sessions.contains_key("token-0"),
            "the oldest session must have been evicted"
        );

        // Expired sessions are swept before eviction, so a stale map does not
        // force out a live session.
        let mut stale: HashMap<String, Session> = HashMap::new();
        let expired = now - (SESSION_TTL + Duration::from_secs(60));
        for n in 0..MAX_SESSIONS {
            stale.insert(
                format!("old-{n}"),
                Session {
                    created: expired,
                    last_seen: expired,
                    password_hash: Some(session_hash.to_owned()),
                },
            );
        }
        insert_session_bounded(
            &mut stale,
            "fresh".into(),
            Session {
                created: now,
                last_seen: now,
                password_hash: Some(session_hash.to_owned()),
            },
            now,
            Some(session_hash),
        );
        assert_eq!(
            stale.len(),
            1,
            "all expired sessions must be swept, leaving only the new one"
        );
        assert!(stale.contains_key("fresh"));

        // Regression: a session used to be valid for up to 12 hours by time
        // alone, so changing the password from another process left every
        // outstanding token working. A session must die with its password.
        assert!(
            !Session {
                created: now,
                last_seen: now,
                password_hash: Some("hash-1".to_owned()),
            }
            .is_valid(now, Some("hash-2")),
            "a session must not survive a password change"
        );

        let stale = Session {
            created: now,
            last_seen: now,
            password_hash: hash.map(str::to_owned),
        };
        let beyond_absolute = now + SESSION_TTL + Duration::from_secs(1);
        assert!(!stale.is_valid(beyond_absolute, hash));

        let idle = Session {
            created: now,
            last_seen: now,
            password_hash: hash.map(str::to_owned),
        };
        let beyond_idle = now + SESSION_IDLE_TIMEOUT + Duration::from_secs(1);
        assert!(!idle.is_valid(beyond_idle, hash));
    }

    #[test]
    fn test_cors_origin_predicate_rejects_foreign_origins() {
        for allowed in ["null", "http://localhost:31828", "http://127.0.0.1:8080"] {
            assert!(
                origin_is_loopback_or_null(allowed),
                "{allowed} should be allowed"
            );
        }
        for denied in [
            "https://evil.example.com",
            "http://192.168.1.50:31828",
            "not-a-url",
        ] {
            assert!(
                !origin_is_loopback_or_null(denied),
                "{denied} should be denied"
            );
        }
    }

    /// Regression: `reload_router_if_running` discarded its error with `let _ =`.
    /// The registry is saved either way, so a failed reload left a running Router
    /// serving the previous revision while the panel reported success — the model
    /// the user had just added simply 404'd, with nothing recorded anywhere.
    ///
    /// The helper must now return the warning, and the response must carry it
    /// without disturbing the payload shape on the success path.
    #[test]
    fn reload_warnings_are_surfaced_without_changing_the_payload() {
        // Success path: the payload is returned verbatim.
        let ok = mutation_response(serde_json::json!({"id": "p", "name": "P"}), None);
        assert_eq!(ok.status(), StatusCode::OK);

        // Failure path: the payload survives and the warning is attached.
        let warned = mutation_response(
            serde_json::json!({"status": "ok"}),
            Some("router did not reload".to_owned()),
        );
        assert_eq!(warned.status(), StatusCode::OK);
    }

    /// The warning key is what the panel keys off, so pin its exact name and
    /// assert on the *rendered* body rather than on a re-built expectation.
    #[tokio::test]
    async fn mutation_response_uses_a_stable_warning_key() {
        let response = mutation_response(
            serde_json::json!({"status": "ok"}),
            Some("router did not reload".to_owned()),
        );
        let bytes = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["router_reload_warning"], "router did not reload");
        assert_eq!(body["status"], "ok", "the original payload must survive");
    }

    /// Regression: the login limiter's `per_ip` map was only ever shrunk by a
    /// *successful* login from the same address. A failed-login flood from
    /// rotating source addresses therefore grew it without bound — the global
    /// attempt cap throttles how many failures are recorded per window, not how
    /// many distinct addresses get an entry. With a 60s window that is up to 30
    /// new entries per minute (~43k/day), each holding an `IpAddr` and a
    /// `VecDeque`, and nothing ever removed them.
    ///
    /// Expanding every window must let the map shrink back.
    #[test]
    fn the_login_limiter_does_not_grow_without_bound() {
        let mut limiter = LoginRateLimiter::default();
        let start = Instant::now();
        let ip = |n: u32| IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + n));

        // A flood from many distinct addresses inside one window.
        for n in 0..(LOGIN_MAX_TRACKED_IPS as u32 * 2) {
            limiter.record_failure(ip(n), start);
        }
        assert!(
            limiter.per_ip.len() <= LOGIN_MAX_TRACKED_IPS + LOGIN_MAX_ATTEMPTS_GLOBAL,
            "the map grew past its bound: {} entries",
            limiter.per_ip.len()
        );

        // Once every window has expired, a single further failure sweeps them all.
        let later = start + LOGIN_WINDOW + Duration::from_secs(1);
        limiter.record_failure(ip(1), later);
        assert!(
            limiter.per_ip.len() <= LOGIN_MAX_ATTEMPTS_GLOBAL + 1,
            "expired entries were never reclaimed: {} entries remain",
            limiter.per_ip.len()
        );
    }

    /// Regression: an unknown `/api/...` path fell through to `static_handler`,
    /// which answered **200 text/html** with the SPA. The panel's `api()` only
    /// checks `response.ok`, so a typo or a version mismatch looked like success
    /// and the caller proceeded with an empty object. API paths must 404 as JSON
    /// while client routes still receive the SPA.
    #[tokio::test]
    async fn unknown_api_paths_are_404_but_client_routes_still_get_the_spa() {
        let dir = tempdir().unwrap();
        let mut state = WebState::with_local_token(
            dir.path().join("providers.json"),
            dir.path().join("router-endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            "test-local-token".to_owned(),
        );
        state.providers = Arc::new(codex_mp_manager::ProviderManager::with_credentials(
            state.registry_path(),
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        ));

        let get = |path: &str| {
            let mut request = axum::http::Request::builder()
                .method("GET")
                .uri(path)
                .header("host", "127.0.0.1")
                .header("x-local-token", "test-local-token")
                .body(axum::body::Body::empty())
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                std::net::SocketAddr::from(([127, 0, 0, 1], 51234)),
            ));
            request
        };

        let response =
            tower::ServiceExt::oneshot(create_web_router(state.clone()), get("/healthz"))
                .await
                .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let response = tower::ServiceExt::oneshot(
            create_web_router(state.clone()),
            get("/api/v1/app/version"),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let version: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            version["version"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            version["core_version"]
                .as_str()
                .is_some_and(|value| !value.is_empty())
        );

        // An unknown API path must be a JSON 404, not an HTML 200.
        let response =
            tower::ServiceExt::oneshot(create_web_router(state.clone()), get("/api/v1/nope"))
                .await
                .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "an unknown API path must not answer 200"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(
            serde_json::from_slice::<serde_json::Value>(&body).is_ok(),
            "the 404 body must be JSON so the panel can parse it: {}",
            String::from_utf8_lossy(&body)
        );

        // A client route still receives the SPA.
        let response =
            tower::ServiceExt::oneshot(create_web_router(state), get("/some/client/route"))
                .await
                .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// End-to-end regression for the same defect as
    /// `mutation_response_uses_a_stable_warning_key`, driven through the real
    /// HTTP handler: when the running Router cannot be told to reload, the
    /// response must carry `router_reload_warning` so the panel can tell the user
    /// that a saved change is not yet being served.
    ///
    /// Without a reachable Router the reload fails, which is exactly the case a
    /// unit test of `mutation_response` alone cannot prove is wired up.
    #[tokio::test]
    async fn a_failed_router_reload_is_reported_in_the_response() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        let mut state = WebState::with_local_token(
            registry_path.clone(),
            dir.path().join("router-endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            "test-local-token".to_owned(),
        );
        // A supervisor pointing at an endpoint nobody serves: `reload` fails.
        let dead_endpoint = dir.path().join("dead-endpoint.json");
        std::fs::write(
            &dead_endpoint,
            serde_json::json!({
                "schema_version": 1,
                "base_url": "http://127.0.0.1:1",
                "capability_token": "dead"
            })
            .to_string(),
        )
        .unwrap();
        state.supervisor = Arc::new(tokio::sync::Mutex::new(
            codex_mp_manager::RouterSupervisor::new(
                PathBuf::from("codex-mp"),
                registry_path,
                &dead_endpoint,
            ),
        ));

        // Create the provider first so the edit below has something to change.
        // An in-memory store keeps this test off the OS keyring: a real keyring
        // call is synchronous and panics ("Cannot start a runtime from within a
        // runtime") when reached from an async test thread.
        let providers = Arc::new(codex_mp_manager::ProviderManager::with_credentials(
            state.registry_path(),
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        ));
        providers
            .add_provider(
                "Probe",
                "https://example.test/v1",
                codex_mp_core::ProviderProtocol::Responses,
                Some(secrecy::SecretString::from("sk-probe".to_owned())),
            )
            .unwrap();
        state.providers = providers;

        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/providers/edit")
            .header("host", "127.0.0.1")
            .header("x-local-token", "test-local-token")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({"id": "probe", "name": "Renamed"}).to_string(),
            ))
            .unwrap();
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                51234,
            ))));

        let response = tower::ServiceExt::oneshot(create_web_router(state), request)
            .await
            .expect("the handler must not panic");
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            body.get("router_reload_warning").is_some(),
            "a failed reload must be reported to the panel: {body}"
        );
    }

    /// Regression: catalog sync updates the official model allow-list after a
    /// provider/model mutation has already reloaded the Router. Without a
    /// second reload here, the running Router keeps the old allow-list: the
    /// custom model works, but switching to a newly discovered official model
    /// returns `model ... was not found` until the next restart.
    #[tokio::test]
    async fn catalog_sync_reloads_after_committing_official_model_ids() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        ProviderRegistry::empty(&registry_path).save().unwrap();

        // `build_and_install` invokes the configured Codex binary for both
        // `--version` and `debug models`; this tiny executable supplies a
        // schema-valid official catalog for both calls.
        let official_catalog = r#"{"models":[{"slug":"gpt-5.6-luna","display_name":"GPT-5.6 Luna","shell_type":"unified_exec","model_messages":{"persistent_instructions":"safe","instructions_template":"safe"},"supported_reasoning_levels":[{"effort":"low","description":"low"}],"supports_search_tool":false,"experimental_supported_tools":[]}]}"#;
        let fake_codex = dir.path().join(if cfg!(windows) {
            "fake-codex.cmd"
        } else {
            "fake-codex"
        });
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::write(
                &fake_codex,
                format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", official_catalog),
            )
            .unwrap();
            fs::set_permissions(&fake_codex, fs::Permissions::from_mode(0o755)).unwrap();
        }
        #[cfg(windows)]
        {
            fs::write(
                &fake_codex,
                format!("@echo off\r\necho {}\r\n", official_catalog),
            )
            .unwrap();
        }

        // A dead but well-formed endpoint lets the test observe that the
        // handler attempted the post-commit reload. The catalog transaction
        // itself must still succeed and return HTTP 200 with a warning.
        let endpoint_file = dir.path().join("router-endpoint.json");
        fs::write(
            &endpoint_file,
            serde_json::json!({
                "schema_version": 1,
                "base_url": "http://127.0.0.1:1",
                "capability_token": "test-capability"
            })
            .to_string(),
        )
        .unwrap();
        let state = WebState::with_local_token(
            registry_path.clone(),
            endpoint_file,
            PathBuf::from("codex-mp"),
            fake_codex,
            "test-local-token".to_owned(),
        );

        let response = api_catalog_sync(State(state)).await;
        assert_eq!(response.status(), StatusCode::OK);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            body.get("router_reload_warning").is_some(),
            "catalog sync must report a failed post-commit reload: {body}"
        );

        let registry = ProviderRegistry::load(registry_path).unwrap();
        assert_eq!(registry.official_model_ids(), &["gpt-5.6-luna"]);
    }

    /// Regression: the panel calls `ProviderManager`'s mutating methods directly
    /// from async handlers, and those methods read/write the OS keyring
    /// synchronously. That was verified live to **panic a tokio worker and drop
    /// the connection** (`POST /api/v1/providers/add` returned HTTP 000):
    /// on Linux the keyring backend (`zbus`) bridges to a synchronous API with
    /// `Runtime::block_on`, which cannot be called from inside a runtime.
    ///
    /// This drives the real handler through the real router, with a credential
    /// store that fails loudly if it is ever touched from a thread where a nested
    /// `block_on` would panic — i.e. an async worker.
    /// Regression: a port conflict surfaced only as
    /// `I/O error: Address already in use (os error 98)` — the port number never
    /// appeared, so the user could not tell which service conflicted or that the
    /// panel was what failed to bind.
    #[tokio::test]
    async fn a_port_conflict_names_the_address_and_the_fix() {
        // Occupy a port, then try to bind the same one.
        let occupied = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let addr = occupied.local_addr().unwrap();

        let error = tokio::net::TcpListener::bind(addr)
            .await
            .map_err(|source| WebError::Bind {
                addr: addr.to_string(),
                source,
            })
            .expect_err("binding an occupied port must fail");
        let message = error.to_string();

        assert!(
            message.contains(&addr.to_string()),
            "the error must name the address, got: {message}"
        );
        assert!(
            message.contains("--port"),
            "the error must state how to choose another port, got: {message}"
        );
    }

    #[tokio::test]
    async fn provider_mutations_run_off_the_async_runtime() {
        /// Fails the moment it is used on a thread that cannot block.
        ///
        /// `Handle::try_current()` is `Ok` on a `spawn_blocking` thread too, so the
        /// real discriminator is whether a nested `block_on` succeeds — which is
        /// precisely what `zbus::utils::block_on` performs for the keyring.
        struct BlockingOnlyStore;

        fn can_block_on_here() -> bool {
            std::panic::catch_unwind(|| {
                if let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    runtime.block_on(async {});
                }
            })
            .is_ok()
        }

        impl CredentialStore for BlockingOnlyStore {
            fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
                assert!(
                    can_block_on_here(),
                    "credential read for `{reference}` ran on an async worker thread"
                );
                Err(CredentialStoreError::NotFound(reference.to_owned()))
            }
            fn set(
                &self,
                reference: &str,
                _value: &SecretString,
            ) -> Result<(), CredentialStoreError> {
                assert!(
                    can_block_on_here(),
                    "credential write for `{reference}` ran on an async worker thread"
                );
                Ok(())
            }
            fn delete(&self, _reference: &str) -> Result<(), CredentialStoreError> {
                Ok(())
            }
        }

        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        const LOCAL_TOKEN: &str = "test-local-token";
        let mut state = WebState::with_local_token(
            registry_path.clone(),
            dir.path().join("endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            LOCAL_TOKEN.to_owned(),
        );
        state.providers = Arc::new(codex_mp_manager::ProviderManager::with_credentials(
            &registry_path,
            Arc::new(BlockingOnlyStore),
        ));

        let mut request = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/providers/add")
            .header("host", "127.0.0.1")
            .header("x-local-token", "test-local-token")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({
                    "name": "Probe",
                    "base_url": "https://example.test/v1",
                    "api_key": "sk-probe",
                    "protocol": "responses",
                    "auth_strategy": {"header": {"name": "X-Tenant-Key"}}
                })
                .to_string(),
            ))
            .unwrap();
        // The auth middleware extracts the peer address, so `oneshot` has to be
        // given one as a loopback `ConnectInfo`.
        request
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                51234,
            ))));

        let response = tower::ServiceExt::oneshot(create_web_router(state), request)
            .await
            .expect("the handler must not panic the worker");
        let status = response.status();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(
            status,
            StatusCode::OK,
            "providers/add must succeed when the keyring work is offloaded; body: {}",
            String::from_utf8_lossy(&body)
        );
        let summary: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            summary["auth_strategy"],
            serde_json::json!({"header": {"name": "X-Tenant-Key"}})
        );
        let registry = ProviderRegistry::load(&registry_path).unwrap();
        assert_eq!(
            registry.provider("probe").unwrap().auth_strategy,
            codex_mp_core::AuthStrategy::Header {
                name: "X-Tenant-Key".into()
            }
        );
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
        state.accounts = Arc::new(memory_backed_accounts(
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

        // 4. Regression guard for S-01: every account projection the panel can
        //    reach is serialized from `AccountSummary`, so a ChatGPT refresh token
        //    can never reach a browser. A future handler that returned the
        //    `ManagedAccount` itself would put these strings on the wire.
        let listed = serde_json::to_string(&list).unwrap();
        let summary = serde_json::to_string(&state.accounts.summarise_account(&captured)).unwrap();
        let active_json = serde_json::to_string(&active).unwrap();
        for (label, payload) in [
            ("list_accounts", &listed),
            ("account summary", &summary),
            ("active status", &active_json),
        ] {
            for secret in ["ref-1", "token-1"] {
                assert!(
                    !payload.contains(secret),
                    "{label} leaked credential material `{secret}`: {payload}"
                );
            }
            for field in ["refresh_token", "access_token", "id_token"] {
                assert!(
                    !payload.contains(field),
                    "{label} exposed a `{field}` field: {payload}"
                );
            }
        }
    }

    #[tokio::test]
    async fn test_account_secrets_are_kept_out_of_the_store_file() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            codex_home.join("auth.json"),
            serde_json::to_string(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "account_id": "account-secret-test"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let store_path = dir.path().join("accounts.json");
        let manager = memory_backed_accounts(store_path.clone(), &codex_home);
        let account = manager.capture_current_auth(Some("A".into())).unwrap();

        // The persisted document must not contain any secret material.
        let raw = fs::read_to_string(&store_path).unwrap();
        assert!(
            !raw.contains("refresh-secret"),
            "store leaked a refresh token"
        );
        assert!(
            !raw.contains("access-secret"),
            "store leaked an access token"
        );
        assert!(
            !raw.contains("tokens"),
            "store should carry no token section"
        );

        // ...while the manager can still produce them when actually switching.
        let reloaded = manager.load_file().unwrap();
        assert_eq!(
            reloaded.accounts[0].tokens.refresh_token.as_deref(),
            Some("refresh-secret")
        );

        // The API-facing projection must be secret-free as well.
        let summary = manager.summarise_account(&account);
        let json = serde_json::to_string(&summary).unwrap();
        assert!(!json.contains("refresh-secret"));
        assert!(!json.contains("access-secret"));
    }

    /// Builds a hash in the format releases before Argon2id wrote:
    /// `salt$sha256(salt:password)`, over the password **exactly as supplied**.
    ///
    /// Tests need it to reproduce an installation that upgraded in place, which
    /// is the only way to reach the migration branch in [`api_security_login`].
    fn legacy_password_hash(password: &str) -> String {
        let salt = "abcd1234";
        let mut hasher = Sha256::new();
        hasher.update(salt.as_bytes());
        hasher.update(b":");
        hasher.update(password.as_bytes());
        let digest: String = hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        format!("{salt}${digest}")
    }

    /// Regression: the trim used to be decided at each call site, so
    /// `codex-mp web password --stdin` (which receives the shell's trailing
    /// newline) stored a hash of `"hunter2"` while the panel verified the pasted
    /// string `"hunter2\n"`. The user was locked out by a password they had just
    /// set correctly, and nothing in the UI could explain why.
    #[test]
    fn a_password_set_through_a_pipe_verifies_from_the_panel() {
        let stored = hash_password("hunter2\n");
        assert!(stored.starts_with(PASSWORD_HASH_PREFIX));
        for candidate in ["hunter2", "hunter2\n", " hunter2 ", "\thunter2\r\n"] {
            assert!(
                verify_password(candidate, &stored),
                "`{candidate:?}` must verify against a hash of `hunter2`"
            );
        }
        assert!(!verify_password("hunter3", &stored));
        // Trimming must not turn a prefix into a match.
        assert!(!verify_password("hunter2x", &stored));
        assert!(!verify_password("hunter", &stored));

        // The direction that matters for an installation that upgrades in place:
        // every legacy *set* path trimmed (the panel's submit handler and the
        // CLI's `--stdin` both called `.trim()`), so the stored hash is over the
        // trimmed password, while the login field can still carry a pasted
        // newline or a stray trailing space. That user has to get in, because
        // this login is what migrates the hash to Argon2id.
        let legacy = legacy_password_hash("legacy-password");
        assert!(verify_password("legacy-password", &legacy));
        assert!(verify_password("legacy-password\n", &legacy));
        assert!(verify_password("  legacy-password\t", &legacy));
        assert!(!verify_password("legacy-passwords", &legacy));

        // The opposite direction is *not* covered, and deliberately so: a legacy
        // hash written over an untrimmed password cannot be matched from a
        // trimmed candidate without guessing what the whitespace was. Widening
        // the accepted set to try `"…\n"`, `"… "`, etc. would weaken
        // verification for every installation to rescue a case no shipped set
        // path could produce.
        let untrimmed = legacy_password_hash("legacy-password\n");
        assert!(verify_password("legacy-password\n", &untrimmed));
        assert!(!verify_password("legacy-password", &untrimmed));
    }

    /// Regression: a legacy-hash installation "logged in successfully" and was
    /// then refused on its **first** API call. The login handler upgraded the
    /// stored hash to Argon2id but bound the new session to the pre-upgrade
    /// value, so the very next request compared the session against the hash now
    /// on disk and rejected it. Driven end-to-end through the real router,
    /// because the defect only exists in the interaction between two handlers.
    #[tokio::test]
    async fn a_session_issued_after_a_legacy_hash_upgrade_still_works() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        let mut registry = ProviderRegistry::empty(&registry_path);
        registry.web_security_mut().password_hash = Some(legacy_password_hash("legacy-password"));
        registry.web_security_mut().web_enabled = true;
        registry.save().unwrap();

        let state = WebState::with_local_token(
            registry_path.clone(),
            dir.path().join("endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            "test-local-token".to_owned(),
        );

        let started = Instant::now();
        // Deliberately no `x-local-token`: this has to go through the password
        // path, not the desktop bypass.
        let mut login = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/security/login")
            .header("host", "127.0.0.1")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({"password": "legacy-password"}).to_string(),
            ))
            .unwrap();
        login
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                51234,
            ))));
        let response = tower::ServiceExt::oneshot(create_web_router(state.clone()), login)
            .await
            .unwrap();
        let login_elapsed = started.elapsed();
        assert_eq!(response.status(), StatusCode::OK);
        // The migration is a read-modify-write on the locked registry, so it is
        // also a check that the save no longer waits out its own lock.
        assert!(
            login_elapsed < codex_mp_core::FILE_LOCK_STALE,
            "a legacy login took {login_elapsed:?}, i.e. it stalled on the \
             registry lock it already held",
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let token = serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"]
            .as_str()
            .expect("login must hand back a token")
            .to_owned();

        // The upgrade landed on disk.
        let upgraded = ProviderRegistry::load(&registry_path)
            .unwrap()
            .web_security()
            .password_hash
            .clone()
            .unwrap();
        assert!(
            !password_hash_needs_upgrade(&upgraded),
            "the legacy hash should have been migrated: {upgraded}"
        );

        // ...and the token issued *by that login* is accepted on the next
        // request. `/api/v1/security/status` answers both authenticated and
        // anonymous callers with 200, so the discriminator is `allow_remote`:
        // `null` means the caller was not recognised.
        let mut status = axum::http::Request::builder()
            .method("GET")
            .uri("/api/v1/security/status")
            .header("host", "127.0.0.1")
            .header("authorization", format!("Bearer {token}"))
            .body(axum::body::Body::empty())
            .unwrap();
        status
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                51234,
            ))));
        let response = tower::ServiceExt::oneshot(create_web_router(state), status)
            .await
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            !body["allow_remote"].is_null(),
            "the session issued by a legacy-hash login must be recognised: {body}"
        );
        assert_eq!(body["password_set"], serde_json::json!(true));
    }

    /// The global attempt ceiling exists to stop a *distributed* remote source
    /// set from guessing without bound. It must not become a lockout the machine
    /// owner cannot escape: anything on the LAN that can reach an `allow_remote`
    /// panel could otherwise flood it and lock the owner out of the very setting
    /// they would turn off to stop the flood, since the panel is the only way to
    /// change it.
    #[test]
    fn the_global_login_ceiling_cannot_lock_out_the_machine_owner() {
        let mut limiter = LoginRateLimiter::default();
        let start = Instant::now();

        for index in 0..LOGIN_MAX_ATTEMPTS_GLOBAL {
            let attacker: IpAddr = format!("203.0.113.{}", index + 1).parse().unwrap();
            limiter.record_failure(attacker, start);
        }

        // A remote caller is now blocked by the global ceiling.
        let remote: IpAddr = "198.51.100.7".parse().unwrap();
        assert!(
            limiter.check(remote, start).is_some(),
            "the flood must have exhausted the global window"
        );

        // The owner on the same machine is not: loopback never contributed to the
        // global window, and its own per-address window is still empty.
        let owner: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            limiter.check(owner, start),
            None,
            "loopback must stay reachable while remote callers are throttled"
        );

        // Local failures must not consume the global budget either, or a typo at
        // the keyboard would lock a remote administrator out.
        for _ in 0..3 {
            limiter.record_failure(owner, start);
        }
        let fresh_remote: IpAddr = "198.51.100.8".parse().unwrap();
        assert!(
            limiter.check(fresh_remote, start).is_some(),
            "loopback failures must not widen the global window"
        );

        // ...but a local guessing loop is still throttled per address.
        for _ in 0..LOGIN_MAX_ATTEMPTS_PER_IP {
            limiter.record_failure(owner, start);
        }
        assert!(
            limiter.check(owner, start).is_some(),
            "loopback must remain subject to its own per-address limit"
        );
    }

    /// The `Host` header is the one part of a DNS-rebinding request the attacker
    /// cannot forge: after `attacker.example` is made to resolve to 127.0.0.1 the
    /// browser considers the panel same-origin, sends no `Origin` (so the CORS
    /// allowlist never votes) and connects from a loopback peer (so the address
    /// check passes too). Parsing as a URL authority rather than string-matching
    /// is what stops `127.0.0.1.evil.example`.
    #[test]
    fn the_host_guard_accepts_only_loopback_authorities() {
        for accepted in [
            "127.0.0.1",
            "127.0.0.1:31828",
            "127.5.6.7:31828",
            "localhost",
            "localhost:31828",
            "LOCALHOST:31828",
            "[::1]:31828",
        ] {
            assert!(
                host_is_loopback_authority(accepted),
                "`{accepted}` names the loopback interface"
            );
        }
        for rejected in [
            "evil.example",
            "evil.example:31828",
            "127.0.0.1.evil.example",
            "user:pass@127.0.0.1",
            "192.168.1.20:31828",
            "127.0.0.1/extra",
            "127.0.0.1?a=b",
            "127.0.0.1#frag",
            "",
        ] {
            assert!(
                !host_is_loopback_authority(rejected),
                "`{rejected}` must not be mistaken for loopback"
            );
        }

        // A missing Host is rejected rather than assumed to be local: HTTP/1.1
        // requires the header, so its absence means a hand-rolled client.
        let response = reject_non_loopback_host(&HeaderMap::new())
            .expect("a request without a Host must be refused");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let mut headers = HeaderMap::new();
        headers.insert(header::HOST, HeaderValue::from_static("evil.example"));
        let response = reject_non_loopback_host(&headers).expect("a foreign Host must be refused");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        headers.insert(header::HOST, HeaderValue::from_static("localhost:31828"));
        assert!(reject_non_loopback_host(&headers).is_none());
    }

    /// End-to-end counterpart of `the_host_guard_accepts_only_loopback_authorities`:
    /// the guard has to actually be wired into both the authenticated layer and
    /// the login endpoint (which sits outside it), and its refusal has to name
    /// the header the caller got wrong.
    #[tokio::test]
    async fn a_foreign_host_is_refused_while_the_panel_is_loopback_only() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        ProviderRegistry::empty(&registry_path).save().unwrap();
        let state = WebState::with_local_token(
            registry_path,
            dir.path().join("endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            "test-local-token".to_owned(),
        );

        let build = |uri: &str, host: &str| {
            let mut request = axum::http::Request::builder()
                .method("GET")
                .uri(uri)
                .header("host", host)
                .header("x-local-token", "test-local-token")
                .body(axum::body::Body::empty())
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                std::net::SocketAddr::from(([127, 0, 0, 1], 51234)),
            ));
            request
        };

        // Through the authenticated layer. `/api/v1/router/status` sits inside
        // the guarded group and touches nothing but the endpoint file, so a
        // refusal here can only have come from the middleware.
        let response = tower::ServiceExt::oneshot(
            create_web_router(state.clone()),
            build("/api/v1/router/status", "evil.example"),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"], serde_json::json!("InvalidHost"));
        let message = body["message"].as_str().unwrap();
        assert!(
            message.contains("evil.example"),
            "must name the Host: {message}"
        );
        assert!(
            message.contains("127.0.0.1"),
            "must state the fix: {message}"
        );

        // Same guard on the unauthenticated route a rebound page would target.
        let mut login = axum::http::Request::builder()
            .method("POST")
            .uri("/api/v1/security/login")
            .header("host", "evil.example")
            .header("content-type", "application/json")
            .body(axum::body::Body::from(
                serde_json::json!({"password": "anything"}).to_string(),
            ))
            .unwrap();
        login
            .extensions_mut()
            .insert(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                [127, 0, 0, 1],
                51234,
            ))));
        let response = tower::ServiceExt::oneshot(create_web_router(state.clone()), login)
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::BAD_REQUEST,
            "login must not hand out a token to a rebound Host"
        );

        // Once the user has deliberately exposed the panel, the legitimate Host
        // is a LAN address that cannot be enumerated here, so the guard stands
        // down and the password is the barrier instead.
        let mut registry = ProviderRegistry::load(state.registry_path()).unwrap();
        registry.web_security_mut().allow_remote = true;
        registry.save().unwrap();
        let response = tower::ServiceExt::oneshot(
            create_web_router(state),
            build("/api/v1/router/status", "192.168.1.20:31828"),
        )
        .await
        .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    /// `allow_remote` chooses the bind address, and `serve_web` binds once at
    /// startup. Saving the flag therefore changes nothing about what this process
    /// is listening on — in either direction. Turning remote access *off* and
    /// being told it succeeded while the socket is still open to the whole LAN is
    /// the dangerous case, so the response has to say a restart is pending.
    #[tokio::test]
    async fn flipping_allow_remote_reports_that_a_restart_is_required() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        let mut registry = ProviderRegistry::empty(&registry_path);
        registry.web_security_mut().password_hash = Some(hash_password("admin123"));
        registry.web_security_mut().web_enabled = true;
        registry.save().unwrap();

        let state = WebState::with_local_token(
            registry_path,
            dir.path().join("endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            "test-local-token".to_owned(),
        );

        let update = |state: &WebState, payload: serde_json::Value| {
            let payload = payload.to_string();
            let state = state.clone();
            async move {
                let mut request = axum::http::Request::builder()
                    .method("POST")
                    .uri("/api/v1/security/update")
                    .header("host", "127.0.0.1")
                    .header("x-local-token", "test-local-token")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(payload))
                    .unwrap();
                request.extensions_mut().insert(axum::extract::ConnectInfo(
                    std::net::SocketAddr::from(([127, 0, 0, 1], 51234)),
                ));
                let response = tower::ServiceExt::oneshot(create_web_router(state), request)
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()
            }
        };

        // The address the running listener is actually bound to: loopback.
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 31828);
        *state.listen_addr.write().await = loopback;

        // Saving something that does not change the exposure is not a pending
        // restart: the socket already matches what the registry now says.
        let body = update(&state, serde_json::json!({"web_enabled": true})).await;
        assert_eq!(body["allow_remote"], serde_json::json!(false));
        assert_eq!(body["restart_required"], serde_json::json!(false));
        assert!(
            body["restart_message"].is_null(),
            "no restart message when nothing about the bind address changed: {body}"
        );

        let body = update(&state, serde_json::json!({"allow_remote": true})).await;
        assert_eq!(body["allow_remote"], serde_json::json!(true));
        assert_eq!(body["restart_required"], serde_json::json!(true));
        let message = body["restart_message"]
            .as_str()
            .expect("a pending restart must be described");
        assert!(
            message.contains(&loopback.to_string()),
            "must name the address still in use: {message}"
        );

        // Re-saving the same flag does not clear it: the process still has not
        // restarted, so the change is still not live.
        let body = update(&state, serde_json::json!({"allow_remote": true})).await;
        assert_eq!(body["restart_required"], serde_json::json!(true));

        // Simulate the restart: the process is now bound to the LAN address.
        let lan = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 20)), 31828);
        *state.listen_addr.write().await = lan;
        let body = update(&state, serde_json::json!({"allow_remote": true})).await;
        assert_eq!(body["restart_required"], serde_json::json!(false));
        assert!(body["restart_message"].is_null());

        // The dangerous direction: the setting now says loopback-only while the
        // socket is still reachable from the LAN.
        let body = update(&state, serde_json::json!({"allow_remote": false})).await;
        assert_eq!(body["allow_remote"], serde_json::json!(false));
        assert_eq!(body["restart_required"], serde_json::json!(true));
        let message = body["restart_message"].as_str().unwrap();
        assert!(message.contains("仅限本机访问"), "{message}");
        assert!(message.contains(&lan.to_string()), "{message}");
    }

    /// Regression for the two-minute self-deadlock that the tests above only
    /// revealed through their runtime.
    ///
    /// `load_registry_locked_blocking` took the registry's `FileLock` on a
    /// `spawn_blocking` thread and parked the guard in the handler; the handler
    /// then called `registry.save()`, which re-locks the same path — **from the
    /// async worker**. `FileLock` reentrancy is keyed by `(path, thread)`, so that
    /// second call looked like an unrelated holder: it polled every 25 ms for the
    /// entire `FILE_LOCK_STALE` window and then reclaimed by renaming a lock file
    /// the very same request still owned. Every `POST /api/v1/security/update`
    /// therefore hung for two minutes, and the reclaim voided the mutual exclusion
    /// the guard existed to provide.
    ///
    /// The bound is deliberately loose — it only has to separate normal service
    /// (milliseconds) from the stale window (120 s), so it cannot flake on a slow
    /// machine while still failing loudly on a regression.
    #[tokio::test]
    async fn a_security_update_never_waits_on_a_lock_it_already_holds() {
        let dir = tempdir().unwrap();
        let registry_path = dir.path().join("providers.json");
        let mut registry = ProviderRegistry::empty(&registry_path);
        registry.web_security_mut().password_hash = Some(hash_password("admin123"));
        registry.web_security_mut().web_enabled = true;
        registry.save().unwrap();

        let state = WebState::with_local_token(
            registry_path.clone(),
            dir.path().join("endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
            "test-local-token".to_owned(),
        );

        let started = Instant::now();
        // Six sequential read-modify-writes on one registry: the shape that used
        // to cost six full stale windows. `attempt % 2 == 1` leaves the last one
        // enabling web access, so the assertion below has a fixed expectation.
        for attempt in 0..6 {
            let mut request = axum::http::Request::builder()
                .method("POST")
                .uri("/api/v1/security/update")
                .header("host", "127.0.0.1")
                .header("x-local-token", "test-local-token")
                .header("content-type", "application/json")
                .body(axum::body::Body::from(
                    serde_json::json!({"web_enabled": attempt % 2 == 1}).to_string(),
                ))
                .unwrap();
            request.extensions_mut().insert(axum::extract::ConnectInfo(
                std::net::SocketAddr::from(([127, 0, 0, 1], 51234)),
            ));
            let response = tower::ServiceExt::oneshot(create_web_router(state.clone()), request)
                .await
                .unwrap();
            let status = response.status();
            let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                .await
                .unwrap();
            assert_eq!(
                status,
                StatusCode::OK,
                "update #{attempt} must succeed without waiting on the registry lock; \
                 body: {}",
                String::from_utf8_lossy(&body),
            );
        }
        let elapsed = started.elapsed();
        assert!(
            elapsed < codex_mp_core::FILE_LOCK_STALE,
            "six security updates took {elapsed:?}, which is within one \
             `FILE_LOCK_STALE` window of the two-minute self-deadlock \
             ({:?} per stale window)",
            codex_mp_core::FILE_LOCK_STALE,
        );

        // The writes all landed, so nothing silently dropped a change either.
        let final_registry = ProviderRegistry::load(&registry_path).unwrap();
        assert!(final_registry.web_security().web_enabled);
    }

    /// The auth.json switch-backup endpoints, driven through the real router.
    ///
    /// Two properties only the HTTP surface can establish:
    ///
    /// * `/accounts/backups` must not be captured by `/accounts/{id}/usage`. Both
    ///   are three segments with a static prefix, and a routing regression here
    ///   would answer the list request with a 404-shaped usage error.
    /// * the failure classification from the manager has to survive the blocking
    ///   hop and become a real HTTP status. The panel's `api()` builds its message
    ///   from the body and its behaviour from the status, so collapsing
    ///   "no such backup" into a generic 500 would tell the user the server is
    ///   broken when in fact they just typed a stale name.
    #[tokio::test]
    async fn auth_backup_endpoints_list_restore_and_report_failures_accurately() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex_home");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            codex_home.join("auth.json"),
            serde_json::to_string(&serde_json::json!({
                "auth_mode": "chatgpt",
                "tokens": {
                    "id_token": "dummy.eyJlbWFpbCI6Im93bmVyQGV4YW1wbGUuY29tIiwic3ViIjoiMSIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3BsYW5fdHlwZSI6InBsdXMiLCJjaGF0Z3B0X2FjY291bnRfaWQiOiJhLTEifX0.dummy",
                    "access_token": "access-secret",
                    "refresh_token": "refresh-secret",
                    "account_id": "a-1"
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let mut state = WebState::new(
            dir.path().join("providers.json"),
            dir.path().join("endpoint.json"),
            PathBuf::from("codex-mp"),
            PathBuf::from("codex"),
        );
        state.accounts = Arc::new(memory_backed_accounts(
            dir.path().join("accounts.json"),
            &codex_home,
        ));
        // One switch, one backup of the session being replaced.
        state
            .accounts
            .capture_current_auth(Some("Owner".into()))
            .unwrap();
        let other = state
            .accounts
            .import_or_update_account(
                codex_mp_manager::AccountTokens {
                    id_token: Some("dummy.eyJlbWFpbCI6Im90aGVyQGV4YW1wbGUuY29tIiwic3ViIjoiMiIsImh0dHBzOi8vYXBpLm9wZW5haS5jb20vYXV0aCI6eyJjaGF0Z3B0X3BsYW5fdHlwZSI6ImZyZWUiLCJjaGF0Z3B0X2FjY291bnRfaWQiOiJhLTIifX0.dummy".into()),
                    access_token: Some("access-secret-2".into()),
                    refresh_token: Some("refresh-secret-2".into()),
                    account_id: Some("a-2".into()),
                },
                None,
                Some("Other".into()),
            )
            .unwrap();
        state.accounts.switch_to_account(&other.id).unwrap();

        let router = create_web_router(state.clone());

        let token = state.local_token.as_ref().clone();
        // `&'static str` rather than `&str`: the closure returns an `async move`
        // block, so any borrow it captures has to outlive the future, not the call.
        let call = |router: axum::Router,
                    method: &'static str,
                    uri: &'static str,
                    body: Option<String>| {
            let token = token.clone();
            async move {
                let mut builder = axum::http::Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("host", "127.0.0.1")
                    .header("x-local-token", token);
                if body.is_some() {
                    builder = builder.header("content-type", "application/json");
                }
                let mut request = builder
                    .body(axum::body::Body::from(body.unwrap_or_default()))
                    .unwrap();
                request.extensions_mut().insert(axum::extract::ConnectInfo(
                    std::net::SocketAddr::from(([127, 0, 0, 1], 51234)),
                ));
                let response = tower::ServiceExt::oneshot(router, request).await.unwrap();
                let status = response.status();
                let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
                    .await
                    .unwrap();
                (
                    status,
                    serde_json::from_slice::<serde_json::Value>(&body)
                        .unwrap_or(serde_json::Value::Null),
                )
            }
        };

        // Listing must reach the backup handler, not the `{id}/usage` route.
        let (status, body) = call(router.clone(), "GET", "/api/v1/accounts/backups", None).await;
        assert_eq!(status, StatusCode::OK, "backups body: {body}");
        let backups = body.as_array().expect("the list must be a JSON array");
        assert_eq!(backups.len(), 1);
        assert_eq!(backups[0]["email"], serde_json::json!("owner@example.com"));
        assert_eq!(
            backups[0]["has_usable_credentials"],
            serde_json::json!(true)
        );
        // The panel renders this straight into the page.
        let listed = backups[0].to_string();
        for secret in ["refresh-secret", "access-secret"] {
            assert!(
                !listed.contains(secret),
                "the backup listing leaked `{secret}` to the browser: {listed}"
            );
        }
        let name = backups[0]["name"]
            .as_str()
            .expect("a backup name")
            .to_owned();

        // A name that is not a backup shape is a client error, not a 500.
        let (status, body) = call(
            router.clone(),
            "POST",
            "/api/v1/accounts/backups/restore",
            Some(serde_json::json!({"name": "auth.json"}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {body}");

        let (status, _) = call(
            router.clone(),
            "POST",
            "/api/v1/accounts/backups/restore",
            Some(
                serde_json::json!({
                    "name": "auth.bak-switch-1700000000-5b8f0f2e-4c1f-4b7a-9a97-1f0f7c9d2e3a"
                })
                .to_string(),
            ),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Restoring the real one puts the original session back.
        let (status, body) = call(
            router.clone(),
            "POST",
            "/api/v1/accounts/backups/restore",
            Some(serde_json::json!({"name": name}).to_string()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "restore body: {body}");
        let live: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(codex_home.join("auth.json")).unwrap())
                .unwrap();
        assert_eq!(
            live["tokens"]["refresh_token"],
            serde_json::json!("refresh-secret")
        );

        // Cleaning removes the copies and reports how many.
        let (status, body) = call(
            router,
            "POST",
            "/api/v1/accounts/backups/clean",
            Some("{}".to_owned()),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "clean body: {body}");
        assert!(body["removed"].as_u64().unwrap_or(0) >= 1);
        assert!(state.accounts.list_auth_backups().unwrap().is_empty());
        // And the live login is still there, untouched.
        assert!(codex_home.join("auth.json").exists());
    }
}
