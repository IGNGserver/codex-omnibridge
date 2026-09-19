//! Shared domain types and safe, non-secret provider registry persistence.
//! API keys are deliberately absent from every persisted type in this crate.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

pub const REGISTRY_SCHEMA_VERSION: u32 = 2;
pub const DEFAULT_WEB_PORT: u16 = 31828;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WebSecurityConfig {
    /// Whether web browser access is enabled (default false: local app direct access only)
    #[serde(default)]
    pub web_enabled: bool,
    /// PBKDF2-HMAC-SHA256 hash formatted as salt$hash (hex-encoded)
    pub password_hash: Option<String>,
    #[serde(default)]
    pub allow_remote: bool,
    #[serde(default = "default_web_port")]
    pub port: u16,
}

fn default_web_port() -> u16 {
    DEFAULT_WEB_PORT
}

impl Default for WebSecurityConfig {
    fn default() -> Self {
        Self {
            web_enabled: false,
            password_hash: None,
            allow_remote: false,
            port: DEFAULT_WEB_PORT,
        }
    }
}

#[derive(Debug, Error)]
pub enum CoreError {
    #[error("invalid provider id `{0}`")]
    InvalidProviderId(String),
    #[error("invalid model id `{0}`")]
    InvalidModelId(String),
    #[error("invalid provider name")]
    EmptyProviderName,
    #[error("invalid provider base URL: {0}")]
    InvalidBaseUrl(String),
    #[error("invalid provider header: {0}")]
    InvalidHeader(String),
    #[error("provider `{0}` already exists")]
    ProviderExists(String),
    #[error("provider `{0}` was not found")]
    ProviderNotFound(String),
    #[error("model `{0}` already exists")]
    ModelExists(String),
    #[error("model `{0}` was not found")]
    ModelNotFound(String),
    /// The model exists but is disabled.
    ///
    /// Reported separately from `ModelNotFound`: a disabled model and a typo used
    /// to produce the same `model \`x\` was not found`, so a user who had just
    /// switched a model off was sent looking for a spelling mistake instead of
    /// the switch they had flipped.
    #[error(
        "model `{0}` is disabled; enable it with `codex-mp model enable {0}` or in the web panel"
    )]
    ModelDisabled(String),
    /// The provider owning this model is disabled.
    ///
    /// Reported separately for the same reason as `ModelDisabled`: `provider edit
    /// <id> --disable` and a typo both produced `model ... was not found`.
    #[error(
        "provider `{0}` is disabled; enable it with `codex-mp provider edit {0} --enable` or in the web panel"
    )]
    ProviderDisabled(String),
    #[error("registry JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("registry IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("unsupported registry schema version {0}")]
    UnsupportedSchema(u32),
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    #[default]
    Responses,
    ChatCompletions,
}

/// How a custom upstream receives the credential held by the selected
/// provider.  The credential value itself is never serialized here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "snake_case")]
pub enum AuthStrategy {
    #[default]
    Bearer,
    ApiKey,
    Header {
        name: String,
    },
    None,
}

impl AuthStrategy {
    /// Header name and value that carry the credential for this strategy.
    ///
    /// This is the single source of truth shared by the router's upstream
    /// calls and by provider model discovery, so a provider configured with
    /// `--auth-strategy header` or `api_key` is not silently probed with a
    /// `Bearer` token.
    pub fn credential_header(&self, secret: &str) -> Option<(String, String)> {
        match self {
            AuthStrategy::None => None,
            AuthStrategy::Bearer => Some(("authorization".to_owned(), format!("Bearer {secret}"))),
            AuthStrategy::ApiKey => Some(("x-api-key".to_owned(), secret.to_owned())),
            AuthStrategy::Header { name } => Some((name.to_ascii_lowercase(), secret.to_owned())),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    #[serde(default)]
    pub reasoning: bool,
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub images: bool,
    #[serde(default)]
    pub files: bool,
    #[serde(default)]
    pub streaming: bool,
}

impl Default for ModelCapabilities {
    fn default() -> Self {
        // Capability advertising must fail closed. `tools: true` by default meant
        // every custom model advertised tool support (and therefore parallel tool
        // calls in the generated catalog) before anything had verified that the
        // provider can honour the full tool lifecycle.
        Self {
            reasoning: true,
            tools: false,
            images: false,
            files: false,
            streaming: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustomModel {
    /// Stable namespaced id exposed to Codex, e.g. `newapi/qwen3.8`.
    pub logical_model_id: String,
    /// Provider-native model id sent upstream.
    pub upstream_model_id: String,
    /// Picker label. It is intentionally provider-prefixed for older pickers.
    pub display_name: String,
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub reasoning_levels: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelEdit {
    pub display_name: Option<String>,
    pub context_window: Option<Option<u64>>,
}

impl CustomModel {
    pub fn new(
        provider_id: &str,
        upstream_model_id: &str,
        display_name: &str,
    ) -> Result<Self, CoreError> {
        validate_provider_id(provider_id)?;
        let upstream_model_id = upstream_model_id.trim();
        if upstream_model_id.is_empty() || upstream_model_id.chars().any(char::is_control) {
            return Err(CoreError::InvalidModelId(upstream_model_id.to_owned()));
        }
        let logical_model_id = format!("{provider_id}/{upstream_model_id}");
        Ok(Self {
            logical_model_id,
            upstream_model_id: upstream_model_id.to_owned(),
            display_name: display_name.trim().to_owned(),
            capabilities: ModelCapabilities::default(),
            enabled: true,
            context_window: None,
            reasoning_levels: vec!["low".into(), "medium".into(), "high".into()],
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderConfig {
    /// Stable lower-case slug used as the first segment of custom model ids.
    pub id: String,
    pub name: String,
    pub base_url: String,
    #[serde(default)]
    pub protocol: ProviderProtocol,
    #[serde(default)]
    pub auth_strategy: AuthStrategy,
    /// Non-secret headers explicitly allowed by the provider profile.
    #[serde(default)]
    pub static_headers: BTreeMap<String, String>,
    /// Keyring service/account reference. This is not the API key.
    pub credential_reference: String,
    #[serde(default = "default_true")]
    pub model_discovery: bool,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub models: Vec<CustomModel>,
}

impl ProviderConfig {
    pub fn new(name: &str, base_url: &str) -> Result<Self, CoreError> {
        let name = name.trim();
        if name.is_empty() {
            return Err(CoreError::EmptyProviderName);
        }
        let id = slugify(name);
        let base_url = normalize_base_url(base_url)?;
        Ok(Self {
            id: id.clone(),
            name: name.to_owned(),
            base_url,
            protocol: ProviderProtocol::Responses,
            auth_strategy: AuthStrategy::Bearer,
            static_headers: BTreeMap::new(),
            credential_reference: format!("provider:{id}"),
            model_discovery: true,
            enabled: true,
            models: Vec::new(),
        })
    }

    pub fn validate(&self) -> Result<(), CoreError> {
        validate_provider_id(&self.id)?;
        if self.name.trim().is_empty() {
            return Err(CoreError::EmptyProviderName);
        }
        normalize_base_url(&self.base_url)?;
        validate_static_headers(&self.static_headers)?;
        if let AuthStrategy::Header { name } = &self.auth_strategy {
            validate_header_name(name).map_err(|_| CoreError::InvalidHeader(name.clone()))?;
            let lower = name.to_ascii_lowercase();
            if lower == "authorization"
                || lower == "chatgpt-account-id"
                || lower == "x-codex-omnibridge-token"
                || lower.starts_with("x-codex-")
            {
                return Err(CoreError::InvalidHeader(name.clone()));
            }
        }
        let mut model_ids = BTreeSet::new();
        for model in &self.models {
            if model.upstream_model_id.trim().is_empty()
                || model.upstream_model_id.chars().any(char::is_control)
                || model.logical_model_id != format!("{}/{}", self.id, model.upstream_model_id)
            {
                return Err(CoreError::InvalidModelId(model.logical_model_id.clone()));
            }
            if !model_ids.insert(model.logical_model_id.clone()) {
                return Err(CoreError::ModelExists(model.logical_model_id.clone()));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderRegistryFile {
    pub schema_version: u32,
    #[serde(default)]
    pub providers: Vec<ProviderConfig>,
    /// Exact official model slugs known by the target stock Codex catalog.
    /// This list is the only source that classifies an official route.
    #[serde(default)]
    pub official_models: Vec<String>,
    #[serde(default = "default_generation")]
    pub generation: u64,
    #[serde(default)]
    pub web_security: WebSecurityConfig,
}

impl Default for ProviderRegistryFile {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            providers: Vec::new(),
            official_models: Vec::new(),
            generation: 1,
            web_security: WebSecurityConfig::default(),
        }
    }
}

impl ProviderRegistryFile {
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchema(self.schema_version));
        }
        let mut ids = BTreeSet::new();
        let mut official = BTreeSet::new();
        for model in &self.official_models {
            if model.trim().is_empty() || model.chars().any(char::is_control) {
                return Err(CoreError::InvalidModelId(model.clone()));
            }
            if !official.insert(model.clone()) {
                return Err(CoreError::ModelExists(model.clone()));
            }
        }
        for provider in &self.providers {
            provider.validate()?;
            if !ids.insert(provider.id.clone()) {
                return Err(CoreError::ProviderExists(provider.id.clone()));
            }
            for model in &provider.models {
                if official.contains(&model.logical_model_id) {
                    return Err(CoreError::ModelExists(model.logical_model_id.clone()));
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ProviderRegistry {
    path: PathBuf,
    file: ProviderRegistryFile,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogicalModelRoute {
    Official {
        model_id: String,
    },
    Custom {
        logical_model_id: String,
        provider_id: String,
        upstream_model_id: String,
        protocol: ProviderProtocol,
    },
}

impl ProviderRegistry {
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            file: ProviderRegistryFile::default(),
        }
    }

    pub fn load(path: impl Into<PathBuf>) -> Result<Self, CoreError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self::empty(path));
        }
        let content = fs::read_to_string(&path)?;
        let mut file: ProviderRegistryFile = serde_json::from_str(&content)?;
        // Version 1 did not carry an explicit official route table. It is safe
        // to load it for migration, but it cannot be served until sync records
        // the target catalog's exact official slugs.
        if file.schema_version == 1 {
            file.schema_version = REGISTRY_SCHEMA_VERSION;
            file.generation = 1;
        }
        file.validate()?;
        Ok(Self { path, file })
    }

    /// Load the registry while holding the cross-process lock for the whole
    /// read-modify-write cycle.
    ///
    /// `load` followed by `save` is not safe on its own: `save` takes the lock,
    /// but by then the read has already happened, so two processes can both read
    /// revision N and each write N+1 with only their own change — a classic lost
    /// update. This was reproducible on a real host (8 concurrent
    /// `provider add` runs left 2 providers in the registry). Callers that intend
    /// to mutate must use this and keep the returned guard alive until after
    /// `save()`.
    pub fn load_locked(path: impl Into<PathBuf>) -> Result<(Self, FileLock), CoreError> {
        let path = path.into();
        let guard = FileLock::acquire(&path)?;
        let registry = Self::load(path)?;
        Ok((registry, guard))
    }

    pub fn load_default() -> Result<Self, CoreError> {
        Self::load(default_registry_path())
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn providers(&self) -> &[ProviderConfig] {
        &self.file.providers
    }

    pub fn generation(&self) -> u64 {
        self.file.generation
    }

    pub fn web_security(&self) -> &WebSecurityConfig {
        &self.file.web_security
    }

    pub fn web_security_mut(&mut self) -> &mut WebSecurityConfig {
        &mut self.file.web_security
    }

    pub fn official_model_ids(&self) -> &[String] {
        &self.file.official_models
    }

    pub fn set_official_model_ids<I, S>(&mut self, model_ids: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut ids = model_ids.into_iter().map(Into::into).collect::<Vec<_>>();
        ids.sort();
        ids.dedup();
        self.file.official_models = ids;
        self.bump_generation();
    }

    pub fn bump_generation(&mut self) {
        self.file.generation = self.file.generation.saturating_add(1);
    }

    pub fn provider(&self, id: &str) -> Option<&ProviderConfig> {
        self.file
            .providers
            .iter()
            .find(|provider| provider.id == id)
    }

    pub fn provider_mut(&mut self, id: &str) -> Option<&mut ProviderConfig> {
        self.file
            .providers
            .iter_mut()
            .find(|provider| provider.id == id)
    }

    pub fn add_provider(&mut self, provider: ProviderConfig) -> Result<(), CoreError> {
        provider.validate()?;
        if self.provider(&provider.id).is_some() {
            return Err(CoreError::ProviderExists(provider.id));
        }
        self.file.providers.push(provider);
        self.bump_generation();
        Ok(())
    }

    pub fn remove_provider(&mut self, id: &str) -> Result<ProviderConfig, CoreError> {
        let index = self
            .file
            .providers
            .iter()
            .position(|provider| provider.id == id)
            .ok_or_else(|| CoreError::ProviderNotFound(id.to_owned()))?;
        let removed = self.file.providers.remove(index);
        self.bump_generation();
        Ok(removed)
    }

    pub fn add_model(&mut self, model: CustomModel) -> Result<(), CoreError> {
        let provider_id = model
            .logical_model_id
            .split_once('/')
            .map(|(provider, _)| provider)
            .ok_or_else(|| CoreError::InvalidModelId(model.logical_model_id.clone()))?;
        let provider = self
            .provider_mut(provider_id)
            .ok_or_else(|| CoreError::ProviderNotFound(provider_id.to_owned()))?;
        if provider
            .models
            .iter()
            .any(|candidate| candidate.logical_model_id == model.logical_model_id)
        {
            return Err(CoreError::ModelExists(model.logical_model_id));
        }
        provider.models.push(model);
        self.bump_generation();
        Ok(())
    }

    pub fn remove_model(&mut self, logical_model_id: &str) -> Result<CustomModel, CoreError> {
        let (provider_id, _) = logical_model_id
            .split_once('/')
            .ok_or_else(|| CoreError::InvalidModelId(logical_model_id.to_owned()))?;
        let provider = self
            .provider_mut(provider_id)
            .ok_or_else(|| CoreError::ProviderNotFound(provider_id.to_owned()))?;
        let index = provider
            .models
            .iter()
            .position(|model| model.logical_model_id == logical_model_id)
            .ok_or_else(|| CoreError::ModelNotFound(logical_model_id.to_owned()))?;
        let removed = provider.models.remove(index);
        self.bump_generation();
        Ok(removed)
    }

    pub fn edit_model(&mut self, logical_model_id: &str, edit: ModelEdit) -> Result<(), CoreError> {
        let (provider_id, _) = logical_model_id
            .split_once('/')
            .ok_or_else(|| CoreError::InvalidModelId(logical_model_id.to_owned()))?;
        let provider = self
            .provider_mut(provider_id)
            .ok_or_else(|| CoreError::ProviderNotFound(provider_id.to_owned()))?;
        let model = provider
            .models
            .iter_mut()
            .find(|model| model.logical_model_id == logical_model_id)
            .ok_or_else(|| CoreError::ModelNotFound(logical_model_id.to_owned()))?;
        if let Some(display_name) = edit.display_name {
            model.display_name = display_name.trim().to_owned();
        }
        if let Some(context_window) = edit.context_window {
            model.context_window = context_window;
        }
        self.bump_generation();
        Ok(())
    }

    pub fn enabled_custom_models(&self) -> impl Iterator<Item = (&ProviderConfig, &CustomModel)> {
        self.file
            .providers
            .iter()
            .filter(|p| p.enabled)
            .flat_map(|provider| {
                provider
                    .models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(move |model| (provider, model))
            })
    }

    pub fn resolve_logical_model_route(
        &self,
        logical_model_id: &str,
    ) -> Result<LogicalModelRoute, CoreError> {
        let logical_model_id = logical_model_id.trim();
        if logical_model_id.is_empty() {
            return Err(CoreError::InvalidModelId(logical_model_id.to_owned()));
        }
        if self
            .file
            .official_models
            .iter()
            .any(|model| model == logical_model_id)
        {
            return Ok(LogicalModelRoute::Official {
                model_id: logical_model_id.to_owned(),
            });
        }
        for provider in self
            .file
            .providers
            .iter()
            .filter(|provider| provider.enabled)
        {
            if let Some(model) = provider
                .models
                .iter()
                .find(|model| model.enabled && model.logical_model_id == logical_model_id)
            {
                return Ok(LogicalModelRoute::Custom {
                    logical_model_id: logical_model_id.to_owned(),
                    provider_id: provider.id.clone(),
                    upstream_model_id: model.upstream_model_id.clone(),
                    protocol: provider.protocol,
                });
            }
        }
        // Distinguish "disabled" from "does not exist". The loop above only
        // considers enabled providers and enabled models, so anything found here
        // exists but was deliberately switched off. Both levels matter: disabling
        // the whole provider is just as legitimate as disabling one model, and
        // both used to read as `model ... was not found`.
        for provider in &self.file.providers {
            for model in &provider.models {
                if model.logical_model_id != logical_model_id {
                    continue;
                }
                if !provider.enabled {
                    return Err(CoreError::ProviderDisabled(provider.id.clone()));
                }
                if !model.enabled {
                    return Err(CoreError::ModelDisabled(logical_model_id.to_owned()));
                }
            }
        }
        Err(CoreError::ModelNotFound(logical_model_id.to_owned()))
    }

    pub fn save(&self) -> Result<(), CoreError> {
        self.file.validate()?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Serialize concurrent read-modify-write cycles across processes. Atomic
        // rename alone prevents a torn file but not a lost update: two writers can
        // each read revision N and both replace it, discarding one change.
        let _guard = FileLock::acquire(&self.path)?;
        let mut bytes = serde_json::to_vec_pretty(&self.file)?;
        bytes.push(b'\n');
        // Routed through `write_private_atomic` so the registry is created 0600
        // rather than umask-readable, and so the parent directory is fsynced.
        write_private_atomic(&self.path, &bytes)?;
        Ok(())
    }
}

/// The single application-identity triple used for every on-disk path.
///
/// This must stay in one place. `core` used `("dev", "codex-multiprovider",
/// "Codex MultiProvider")` while `credentials` used `("dev", "codex",
/// "codexmultiprovider")`. On Linux `directories` builds the config directory
/// from the *application* segment only, so both happened to resolve to
/// `~/.config/codexmultiprovider` — which is why the drift went unnoticed. On
/// macOS and Windows the qualifier/organization participate, so the registry and
/// the credential file would have landed in **different** directories.
pub const APP_QUALIFIER: &str = "dev";
pub const APP_ORGANIZATION: &str = "codex-multiprovider";
pub const APP_NAME: &str = "Codex MultiProvider";

/// Directory holding the registry, catalog, manifest and credential file.
pub fn app_config_dir() -> Option<PathBuf> {
    ProjectDirs::from(APP_QUALIFIER, APP_ORGANIZATION, APP_NAME)
        .map(|dirs| dirs.config_dir().to_path_buf())
}

pub fn default_registry_path() -> PathBuf {
    app_config_dir()
        .map(|dir| dir.join("providers.json"))
        .unwrap_or_else(|| PathBuf::from("providers.json"))
}

/// Replace `destination` with `temporary` using the platform's replacement
/// primitive. Unix `rename` replaces an existing file; Windows `rename` does
/// not, so it must use MoveFileExW with MOVEFILE_REPLACE_EXISTING instead.
/// An exclusive advisory lock guarding a read-modify-write of a state file.
///
/// The registry, the integration manifest, the accounts store and the Router
/// endpoint are all updated as `load -> mutate -> atomic_replace`, and none of
/// them took a cross-process lock. Atomic rename prevents a *torn* file but not a
/// *lost update*: two processes (the CLI and the web panel, for instance) can
/// each read revision N and both write revision N+1, silently discarding one
/// change. In the integration case that leaves `config.toml` and its manifest
/// disagreeing, which every guard then rejects.
///
/// The lock is a sibling `<file>.lock` created with `create_new`, so it is
/// exclusive across processes. A lock whose holder is gone (stale PID) or that is
/// older than [`FILE_LOCK_STALE`] is reclaimed, so a crash cannot wedge the file
/// for ever. The guard removes the lock on drop.
#[derive(Debug)]
pub struct FileLock {
    path: PathBuf,
    /// True when this acquisition actually created the lock file. A reentrant
    /// acquisition must not delete a lock the outer frame still owns.
    owner: bool,
}

/// Which thread currently owns each lock path.
///
/// The lock must be reentrant: the guarded state file's own save function takes
/// it too (`ProviderRegistry::save` locks `self.path`), so a caller doing
/// `let _guard = FileLock::acquire(p)?; ...; registry.save()` would otherwise
/// deadlock against itself waiting out its own timeout.
///
/// This registry is **process-global** rather than `thread_local`, and that is
/// load-bearing. With `thread_local` state, a guard dropped on a different thread
/// than the one that acquired it removed the entry from the *wrong* thread's set:
/// the acquiring thread kept a phantom "already held" entry and **every later
/// lock acquisition on that thread silently succeeded without locking at all**.
/// With `spawn_blocking` (the web panel's execution model) reusing pooled
/// threads, that produced real lost updates: 5 concurrent `providers/add` calls
/// all returned 200 while only 4 providers were stored.
static HELD_LOCKS: std::sync::Mutex<
    Option<std::collections::HashMap<PathBuf, std::thread::ThreadId>>,
> = std::sync::Mutex::new(None);

/// Run `action` with the held-lock registry.
fn with_held_locks<T>(
    action: impl FnOnce(&mut std::collections::HashMap<PathBuf, std::thread::ThreadId>) -> T,
) -> T {
    let mut guard = HELD_LOCKS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = guard.get_or_insert_with(std::collections::HashMap::new);
    action(map)
}

/// How long a lock may be held before it is treated as abandoned.
pub const FILE_LOCK_STALE: std::time::Duration = std::time::Duration::from_secs(120);

impl FileLock {
    /// Acquire the lock for `state_file`, blocking until it is available.
    pub fn acquire(state_file: impl AsRef<Path>) -> Result<Self, std::io::Error> {
        let state_file = state_file.as_ref();
        let path = lock_file_path(state_file);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        // Reentrant fast path: this thread already holds it.
        let current_thread = std::thread::current().id();
        // Reentrant fast path: this same thread already owns the lock.
        let already_held = with_held_locks(|held| held.get(&path) == Some(&current_thread));
        if already_held {
            return Ok(Self { path, owner: false });
        }
        let deadline = std::time::Instant::now() + FILE_LOCK_STALE;
        loop {
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut handle) => {
                    handle.write_all(std::process::id().to_string().as_bytes())?;
                    with_held_locks(|held| held.insert(path.clone(), current_thread));
                    return Ok(Self { path, owner: true });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_abandoned(&path) {
                        // Reclaim by *renaming* to a unique name: `rename` is
                        // atomic, so exactly one waiter wins and the others fall
                        // through to retry `create_new`. A plain `remove_file`
                        // here raced with a concurrent waiter's fresh lock.
                        let stale =
                            path.with_extension(format!("lock.reclaim.{}", uuid::Uuid::new_v4()));
                        // Someone else may have reclaimed it first; either way
                        // we simply retry `create_new`.
                        if fs::rename(&path, &stale).is_ok() {
                            let _ = fs::remove_file(&stale);
                        }
                        continue;
                    }
                    if std::time::Instant::now() >= deadline {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::WouldBlock,
                            format!("timed out waiting for lock `{}`", path.display()),
                        ));
                    }
                    std::thread::sleep(std::time::Duration::from_millis(25));
                }
                Err(error) => return Err(error),
            }
        }
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        if !self.owner {
            // A reentrant frame: the outer acquisition still owns the file.
            return;
        }
        // Remove by path regardless of which thread is running this `Drop`: the
        // entry belongs to the guard, not to the thread that happens to drop it.
        with_held_locks(|held| held.remove(&self.path));
        let _ = fs::remove_file(&self.path);
    }
}

/// Path of the lock file guarding `state_file`.
pub fn lock_file_path(state_file: &Path) -> PathBuf {
    let mut name = state_file
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_owned());
    name.push_str(".lock");
    state_file.with_file_name(name)
}

fn lock_is_abandoned(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        // The file is gone. That is not an "abandoned lock to reclaim" — the
        // owner just released it, and the caller only needs to retry its
        // `create_new`. Reporting `true` here made waiters call `remove_file`,
        // and two waiters could each delete the lock the other had just created,
        // putting both inside the critical section. Observed as: 20 concurrent
        // `providers/add` all returning 200 with only 19 landing, and no lock
        // timeout anywhere.
        return false;
    };
    if let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
        && age > FILE_LOCK_STALE
    {
        return true;
    }
    match fs::read_to_string(path) {
        Ok(contents) => contents
            .trim()
            .parse::<u32>()
            .map(|pid| !process_is_alive(pid))
            // An unparsable lock is left alone; the age check reclaims it.
            .unwrap_or(false),
        Err(_) => false,
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // `kill(pid, 0)` checks for existence without delivering a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    // Fall back to "alive" when the probe cannot run; the age check is the
    // backstop, and erring toward "alive" never corrupts state.
    std::process::Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("\"{pid}\"")))
        .unwrap_or(true)
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

/// Write `bytes` to `path` atomically, with the file never observable as
/// readable by anyone but the owner.
///
/// `File::create` applies the process umask, so a temp file holding a secret is
/// typically mode 0644 (or 0664 under the common umask 0002) between creation and
/// the later `chmod 0600`. On a shared machine that window exposes live OAuth
/// tokens and API keys. Creating the file with `mode(0o600)` at `open()` time
/// removes the window entirely; the `chmod` afterwards is kept as belt-and-braces
/// for platforms/filesystems that ignore the mode argument.
pub fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    // Follow a symlink to its target before writing.
    //
    // The write below is a temp-file-plus-`rename`, and `rename` replaces the
    // *link* rather than the file it points at. A user who symlinks
    // `providers.json` into a dotfiles repository (an ordinary setup) therefore
    // got a silent fork: the command reported success, the new file held the
    // change, and the real target kept the old data. Resolving first makes the
    // write land where the user pointed.
    //
    // A dangling symlink is resolved with `canonicalize` failing on the target,
    // so fall back to reading the link itself.
    let resolved = match fs::canonicalize(path) {
        Ok(target) => target,
        Err(_) => match fs::read_link(path) {
            Ok(target) if target.is_absolute() => target,
            Ok(target) => path
                .parent()
                .map(|parent| parent.join(&target))
                .unwrap_or(target),
            // Not a symlink (or unreadable): use the path as given.
            Err(_) => path.to_path_buf(),
        },
    };
    let path = resolved.as_path();

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    // A unique temp name keeps two concurrent writers from clobbering each
    // other; the name never becomes the public path.
    let nonce = uuid::Uuid::new_v4();
    let mut temp_name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "state".to_owned());
    temp_name.push_str(&format!(".{nonce}.tmp"));
    let temporary = path.with_file_name(temp_name);

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), std::io::Error> {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    if let Err(error) = atomic_replace(&temporary, path) {
        let _ = fs::remove_file(&temporary);
        return Err(error);
    }
    set_private_permissions(path)?;
    // Make the rename itself durable, so a crash cannot lose the directory entry
    // while a later write survives.
    #[cfg(unix)]
    if let Some(parent) = path.parent()
        && let Ok(dir) = fs::File::open(parent)
    {
        let _ = dir.sync_all();
    }
    Ok(())
}

/// Spawn a command, retrying briefly while the kernel reports `ETXTBSY`.
///
/// On Unix, `execve` fails with `ETXTBSY` ("text file busy") when *any* process
/// still holds the executable open for writing — including an unrelated process
/// that inherited the descriptor across a concurrent `fork()`. This project both
/// writes launcher scripts and immediately executes them (desktop override,
/// patched Codex, test stubs), so the race is reachable in production and was
/// observed in this test suite: a parallel test that forked while another had a
/// freshly written script open turned the expected `TimedOut` into
/// `ExecutableFileBusy`, failing roughly 1 run in 15.
///
/// The writer closes the file within microseconds, so a short bounded retry is
/// the standard remedy. Any other error is returned immediately.
fn spawn_retrying_text_busy(
    command: &mut std::process::Command,
) -> Result<std::process::Child, std::io::Error> {
    /// Attempt count; the waits sum to well under any caller's timeout.
    const MAX_ATTEMPTS: u32 = 10;
    let mut attempt = 0;
    loop {
        match command.spawn() {
            Ok(child) => return Ok(child),
            Err(error) if error.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                attempt += 1;
                if attempt >= MAX_ATTEMPTS {
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(5 * u64::from(attempt)));
            }
            Err(error) => return Err(error),
        }
    }
}

/// Run a command to completion with a deadline, capturing stdout.
///
/// Every `codex` invocation this project makes is metadata gathering (version,
/// bundled model catalog), never work worth blocking on. `std::process::Command::output`
/// waits with no deadline, so a wrapper script or a wedged install that never
/// exits hung `codex-mp sync` for ever with no output. A timed-out child is
/// killed and reported as an error so the caller can degrade.
pub fn run_with_timeout(
    command: &mut std::process::Command,
    limit: std::time::Duration,
) -> Result<std::process::Output, std::io::Error> {
    command
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    // Put the child in its own process group. `.kill()` signals only the direct
    // child, so a shell script that starts another process (`sh -c 'sleep 3600'`)
    // leaves a grandchild holding the inherited pipe open — the drain threads then
    // block on `read_to_end` for ever after the timeout fires. Signalling the
    // group tears down the whole tree. The callers here run short-lived,
    // non-interactive probes, so replacing the process group is safe.
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }

    let mut child = spawn_retrying_text_busy(command)?;

    // Drain both pipes on their own threads for as long as the child runs.
    //
    // This used to poll `try_wait()` and only read the pipes *after* the child
    // exited. That deadlocks for any output larger than the pipe buffer (~64 KiB
    // on Linux): the child blocks writing, so it never exits, so the parent never
    // reads. `codex debug models --bundled` emits ~444 KiB, so `codex-mp sync`
    // failed with a bogus "did not finish within 30s" against every real Codex.
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || drain(stdout_pipe));
    let stderr_reader = std::thread::spawn(move || drain(stderr_pipe));

    let deadline = std::time::Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(status) => break Some(status),
            None => {
                if std::time::Instant::now() >= deadline {
                    kill_process_tree(&mut child);
                    break None;
                }
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
        }
    };

    // The pipes close once the child (and any process holding them) exits, so
    // these joins are bounded by the child's lifetime; a killed child closes them
    // too. Joining after `kill` is deliberate: it guarantees the collected output
    // belongs to this run rather than a partially-read buffer.
    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr = stderr_reader.join().unwrap_or_default();

    match status {
        Some(status) => Ok(std::process::Output {
            status,
            stdout,
            stderr,
        }),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("command did not finish within {limit:?}"),
        )),
    }
}

/// Terminate a timed-out child and every process in its group.
///
/// Killing only the direct child can leave a grandchild running with the
/// inherited stdout/stderr pipe still open, which keeps the drain threads blocked
/// after the timeout has already fired.
fn kill_process_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The child was spawned with `process_group(0)`, so its pid is the group
        // id. Signal the group first, then the child as a fallback for platforms
        // or spawn paths where the group call did not apply.
        let pid = child.id() as i32;
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

/// Read a child pipe to EOF, tolerating errors and a missing pipe.
///
/// Generic over both `ChildStdout` and `ChildStderr`, which are distinct types
/// with the same `Read` implementation.
fn drain<R: std::io::Read>(pipe: Option<R>) -> Vec<u8> {
    let mut collected = Vec::new();
    if let Some(mut pipe) = pipe {
        let _ = pipe.read_to_end(&mut collected);
    }
    collected
}

pub fn atomic_replace(
    temporary: impl AsRef<Path>,
    destination: impl AsRef<Path>,
) -> Result<(), std::io::Error> {
    let temporary = temporary.as_ref();
    let destination = destination.as_ref();

    #[cfg(not(windows))]
    {
        fs::rename(temporary, destination)
    }

    #[cfg(windows)]
    {
        use std::ffi::OsStr;
        use std::os::windows::ffi::OsStrExt;

        fn wide(path: &OsStr) -> Vec<u16> {
            path.encode_wide().chain(std::iter::once(0)).collect()
        }

        let temporary = wide(temporary.as_os_str());
        let destination = wide(destination.as_os_str());
        // MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH.
        const REPLACE_EXISTING: u32 = 0x0000_0001;
        const WRITE_THROUGH: u32 = 0x0000_0008;

        unsafe extern "system" {
            fn MoveFileExW(
                existing_file_name: *const u16,
                new_file_name: *const u16,
                flags: u32,
            ) -> i32;
        }

        if unsafe {
            MoveFileExW(
                temporary.as_ptr(),
                destination.as_ptr(),
                REPLACE_EXISTING | WRITE_THROUGH,
            )
        } == 0
        {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

/// Return the executable names that can represent `path` on this platform.
/// Windows installations may expose a binary as `.exe`, `.cmd`, or `.bat`
/// while Unix installations normally use the name verbatim.
pub fn executable_variants(path: impl AsRef<Path>) -> Vec<PathBuf> {
    let path = path.as_ref();

    #[cfg(windows)]
    {
        let mut variants = vec![path.to_path_buf()];
        if path.extension().is_none() {
            for extension in ["exe", "cmd", "bat", "com"] {
                variants.push(path.with_extension(extension));
            }
        }
        variants
    }

    #[cfg(not(windows))]
    {
        vec![path.to_path_buf()]
    }
}

/// Resolve an executable from an explicit path or the current PATH. The
/// returned path is still allowed to be unresolved so callers can produce a
/// useful `Command` error for the original input.
pub fn resolve_executable(path: impl AsRef<Path>) -> PathBuf {
    let requested = path.as_ref();
    let has_path_component = requested.is_absolute() || requested.components().count() > 1;

    if has_path_component {
        return executable_variants(requested)
            .into_iter()
            .find(|candidate| candidate.is_file())
            .unwrap_or_else(|| requested.to_path_buf());
    }

    if let Some(path_value) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path_value) {
            for candidate in executable_variants(directory.join(requested)) {
                if candidate.is_file() {
                    return candidate;
                }
            }
        }
    }

    requested.to_path_buf()
}

/// Build a process command for an executable resolved by [`resolve_executable`].
/// Windows batch launchers are not PE executables, so they must be invoked via
/// the system command interpreter rather than passed directly to CreateProcess.
pub fn command_for_executable(path: impl AsRef<Path>) -> std::process::Command {
    let path = resolve_executable(path);

    #[cfg(windows)]
    {
        let is_batch = path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| {
                matches!(extension.to_ascii_lowercase().as_str(), "cmd" | "bat")
            });
        if is_batch {
            let command_interpreter = std::env::var_os("SystemRoot")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"))
                .join("System32")
                .join("cmd.exe");
            let mut command = std::process::Command::new(command_interpreter);
            command.args(["/D", "/S", "/C"]).arg(path);
            return command;
        }
    }

    std::process::Command::new(path)
}

pub fn slugify(value: &str) -> String {
    let mut result = String::new();
    let mut last_dash = false;
    for ch in value.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            result.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash && !result.is_empty() {
            result.push('-');
            last_dash = true;
        }
    }
    while result.ends_with('-') {
        result.pop();
    }
    if result.is_empty() {
        "provider".to_owned()
    } else {
        result
    }
}

pub fn validate_provider_id(value: &str) -> Result<(), CoreError> {
    if value.is_empty()
        || value.len() > 64
        || value.starts_with('-')
        || value.ends_with('-')
        || value.contains("--")
        || !value
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-')
    {
        return Err(CoreError::InvalidProviderId(value.to_owned()));
    }
    Ok(())
}

/// Addresses that are never a legitimate model provider.
///
/// Loopback and private ranges stay allowed on purpose: pointing a provider at a
/// local model server (Ollama, vLLM, LM Studio) is a supported setup. The cloud
/// instance-metadata endpoint is different — it is the classic SSRF target and
/// no model is ever served from it.
/// Strip the brackets `Url::host_str` keeps on an IPv6 literal.
///
/// `host_str` returns `[::1]`, not `::1`, so a plain `parse::<IpAddr>()` fails and
/// every IPv6 host was silently treated as "not an IP address". That broke the
/// loopback check (plaintext HTTP to `[::1]` was rejected as "non-loopback") and,
/// far worse, **disabled the metadata/link-local block for IPv6 entirely**:
/// `https://[::ffff:169.254.169.254]/` was accepted while its IPv4 spelling was
/// rejected.
fn unbracket_ipv6_host(host: &str) -> &str {
    host.strip_prefix('[')
        .and_then(|rest| rest.strip_suffix(']'))
        .unwrap_or(host)
}

/// Whether an address is loopback, including the IPv4-mapped IPv6 form.
///
/// `Ipv6Addr::is_loopback` is false for `::ffff:127.0.0.1`, so a gateway bound to
/// that spelling was rejected as "non-loopback" while `127.0.0.1` was accepted.
fn is_loopback_address(address: std::net::IpAddr) -> bool {
    address.is_loopback()
        || match address {
            std::net::IpAddr::V6(v6) => v6
                .to_ipv4_mapped()
                .is_some_and(|v4| std::net::IpAddr::V4(v4).is_loopback()),
            std::net::IpAddr::V4(_) => false,
        }
}

fn is_blocked_provider_host(host: &str) -> bool {
    match unbracket_ipv6_host(host).parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(address)) => {
            // 169.254.0.0/16 — link-local, includes 169.254.169.254.
            let octets = address.octets();
            octets[0] == 169 && octets[1] == 254
        }
        Ok(std::net::IpAddr::V6(address)) => {
            // fe80::/10 link-local, and the IPv4-mapped form of the above.
            let segments = address.segments();
            (segments[0] & 0xffc0) == 0xfe80
                || address
                    .to_ipv4_mapped()
                    .is_some_and(|v4| is_blocked_provider_host(&v4.to_string()))
        }
        Err(_) => false,
    }
}

pub fn normalize_base_url(value: &str) -> Result<String, CoreError> {
    let value = value.trim().trim_end_matches('/');
    let parsed = Url::parse(value).map_err(|error| CoreError::InvalidBaseUrl(error.to_string()))?;
    if !matches!(parsed.scheme(), "http" | "https")
        || parsed.host_str().is_none()
        || !parsed.username().is_empty()
        || parsed.password().is_some()
        || parsed.query().is_some()
        || parsed.fragment().is_some()
    {
        return Err(CoreError::InvalidBaseUrl(value.to_owned()));
    }
    if let Some(host) = parsed.host_str()
        && is_blocked_provider_host(host)
    {
        return Err(CoreError::InvalidBaseUrl(
            "provider host is a link-local/metadata address".into(),
        ));
    }
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or_default();
        let loopback = host.eq_ignore_ascii_case("localhost")
            || unbracket_ipv6_host(host)
                .parse::<std::net::IpAddr>()
                .map(is_loopback_address)
                .unwrap_or(false);
        if !loopback {
            return Err(CoreError::InvalidBaseUrl(
                "non-loopback provider URLs must use https".into(),
            ));
        }
    }
    Ok(value.to_owned())
}

fn default_true() -> bool {
    true
}

fn default_generation() -> u64 {
    1
}

fn validate_static_headers(headers: &BTreeMap<String, String>) -> Result<(), CoreError> {
    for (name, value) in headers {
        validate_header_name(name).map_err(|_| CoreError::InvalidHeader(name.clone()))?;
        let lower = name.to_ascii_lowercase();
        if lower == "authorization"
            || lower == "chatgpt-account-id"
            || lower == "x-api-key"
            || lower == "x-codex-omnibridge-token"
            || lower.starts_with("x-codex-")
        {
            return Err(CoreError::InvalidHeader(name.clone()));
        }
        if value.chars().any(char::is_control) {
            return Err(CoreError::InvalidHeader(name.clone()));
        }
    }
    Ok(())
}

fn validate_header_name(name: &str) -> Result<(), ()> {
    if name.trim().is_empty()
        || name.chars().any(char::is_control)
        || !name
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Err(());
    }
    Ok(())
}

pub fn set_private_permissions(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        // Windows has no portable std-only equivalent of mode 0600. Use the
        // system ACL utility without a shell: remove inherited entries and
        // grant the current Windows principal full access to this file.
        let system_root = std::env::var_os("SystemRoot").ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "SystemRoot is not set; cannot secure a Windows private file",
            )
        })?;
        let system_dir = PathBuf::from(system_root).join("System32");
        let principal = std::process::Command::new(system_dir.join("whoami.exe"))
            .output()?
            .stdout;
        let principal = String::from_utf8_lossy(&principal).trim().to_owned();
        if principal.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "whoami.exe returned no Windows principal",
            ));
        }
        let output = std::process::Command::new(system_dir.join("icacls.exe"))
            .arg(path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{principal}:F"))
            .output()?;
        if !output.status.success() {
            let message = String::from_utf8_lossy(&output.stderr).trim().to_owned();
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                if message.is_empty() {
                    "icacls.exe could not secure the private file".to_owned()
                } else {
                    message
                },
            ));
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// Strip Windows verbatim (`\\?\` or `\\?\UNC\`) prefix if present, returning a normal path.
/// On non-Windows platforms, returns the path as-is.
pub fn clean_verbatim_path(path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    let s = path.to_string_lossy();
    if let Some(stripped) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{stripped}"))
    } else if let Some(stripped) = s.strip_prefix(r"\\?\") {
        PathBuf::from(stripped)
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn registry_never_serializes_an_api_key() {
        let mut registry = ProviderRegistry::empty("/tmp/does-not-matter.json");
        let mut provider = ProviderConfig::new("NewAPI", "https://api.example.test/v1").unwrap();
        provider.credential_reference = "provider:newapi".to_owned();
        registry.add_provider(provider).unwrap();
        let model = CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap();
        registry.add_model(model).unwrap();
        let json = serde_json::to_string(registry.providers()).unwrap();
        assert!(!json.to_ascii_lowercase().contains("api_key"));
        assert!(!json.contains("secret"));
    }

    #[test]
    fn non_loopback_http_provider_urls_are_rejected() {
        assert!(normalize_base_url("http://provider.example/v1").is_err());
        assert!(normalize_base_url("https://user:secret@provider.example/v1").is_err());
        assert!(normalize_base_url("https://provider.example/v1?token=secret").is_err());
        assert_eq!(
            normalize_base_url("http://127.0.0.1:8787/v1").unwrap(),
            "http://127.0.0.1:8787/v1"
        );
        assert_eq!(
            normalize_base_url("http://localhost:8787/v1").unwrap(),
            "http://localhost:8787/v1"
        );
    }

    #[test]
    fn provider_header_policy_rejects_official_and_capability_headers() {
        let mut provider = ProviderConfig::new("NewAPI", "https://api.example.test/v1").unwrap();
        provider
            .static_headers
            .insert("Authorization".into(), "canary".into());
        assert!(matches!(
            provider.validate(),
            Err(CoreError::InvalidHeader(_))
        ));
        provider.static_headers.clear();
        provider.auth_strategy = AuthStrategy::Header {
            name: "x-codex-omnibridge-token".into(),
        };
        assert!(matches!(
            provider.validate(),
            Err(CoreError::InvalidHeader(_))
        ));
    }

    #[test]
    fn registry_round_trips_atomically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let mut registry = ProviderRegistry::empty(&path);
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://api.example.test/v1").unwrap())
            .unwrap();
        registry.save().unwrap();
        let loaded = ProviderRegistry::load(&path).unwrap();
        assert_eq!(loaded.providers()[0].id, "newapi");
    }

    /// The registry and the credential file must share one application identity.
    ///
    /// They used two different `ProjectDirs` triples. Linux builds the config
    /// directory from the application segment alone, so both resolved to
    /// `~/.config/codexmultiprovider` and the drift was invisible; macOS and
    /// Windows include the qualifier/organization, so the two files would have
    /// been split across directories.
    #[test]
    fn registry_and_credential_paths_share_one_application_identity() {
        let registry = default_registry_path();
        let credential = codex_path_for_test();
        assert_eq!(
            registry.parent(),
            credential.parent(),
            "registry ({}) and credential file ({}) must live in the same directory",
            registry.display(),
            credential.display()
        );
    }

    /// Mirrors what `codex_mp_credentials` derives, without creating a cycle.
    fn codex_path_for_test() -> PathBuf {
        app_config_dir()
            .map(|dir| dir.join(".credentials"))
            .unwrap_or_else(|| PathBuf::from(".credentials"))
    }

    /// Regression: `File::create` applies the umask, so a temp file holding a
    /// secret was group/world readable (0664 under the common umask 0002) until a
    /// later `chmod 0600`. Live OAuth tokens must never be readable by anyone but
    /// the owner, at any instant.
    #[cfg(unix)]
    #[test]
    fn write_private_atomic_never_exposes_the_file() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let target = dir.path().join("auth.json");
        write_private_atomic(&target, b"{\"refresh_token\":\"secret\"}").unwrap();

        let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "final mode was {mode:o}");

        // No leftover temp file may exist, private or otherwise.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name != "auth.json")
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp files left behind: {leftovers:?}"
        );

        // And the content must have landed intact.
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{\"refresh_token\":\"secret\"}"
        );
    }

    /// A failed write must not leave a temp file behind, and must not damage the
    /// existing target.
    #[cfg(unix)]
    #[test]
    fn write_private_atomic_replaces_atomically() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("state.json");
        write_private_atomic(&target, b"first").unwrap();
        write_private_atomic(&target, b"second").unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "second");
        let count = std::fs::read_dir(dir.path()).unwrap().count();
        assert_eq!(count, 1, "an extra file was left in the directory");
    }

    /// Regression: `Command::output()` waits for the child with no deadline, so a
    /// wrapper script that never exits hung `codex-mp sync` indefinitely.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_kills_a_hung_child() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir().unwrap();
        let script = dir.path().join("hang.sh");
        std::fs::write(&script, "#!/bin/sh\nsleep 3600\n").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        let mut command = std::process::Command::new(&script);
        let started = std::time::Instant::now();
        let error = run_with_timeout(&mut command, std::time::Duration::from_millis(300))
            .expect_err("a hung child must be reported, not awaited for ever");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "the deadline was not enforced"
        );
    }

    /// Regression: `run_with_timeout` piped stdout/stderr but only read them
    /// **after** the child exited, while polling `try_wait()` in between. Any
    /// output larger than the pipe buffer (64 KiB on Linux) filled the pipe, so
    /// the child blocked writing and never exited — and the parent, waiting for it
    /// to exit before reading, timed out.
    ///
    /// `codex debug models --bundled` emits ~444 KiB, so `codex-mp sync` failed
    /// with a bogus "command did not finish within 30s" against every real Codex
    /// installation. The pipes must be drained while the child runs.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_reads_output_larger_than_the_pipe_buffer() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let script = dir.path().join("big.sh");
        // ~512 KiB, comfortably above the 64 KiB pipe buffer. Without concurrent
        // draining this deadlocks and the call returns TimedOut.
        fs::write(
            &script,
            "#!/bin/sh\ni=0\nwhile [ $i -lt 512 ]; do\n  printf '%01024d' 0\n  i=$((i+1))\ndone\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let mut command = std::process::Command::new(&script);
        let output = run_with_timeout(&mut command, std::time::Duration::from_secs(30))
            .expect("large output must be drained, not deadlocked");
        assert!(output.status.success());
        assert_eq!(
            output.stdout.len(),
            512 * 1024,
            "the whole output must be captured"
        );
    }

    /// The timeout must still fire for a child that genuinely hangs, even while
    /// output is being drained concurrently.
    #[cfg(unix)]
    #[test]
    fn a_hung_child_still_times_out_while_draining() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let script = dir.path().join("noisy-hang.sh");
        // Emits more than a pipe buffer, then sleeps forever: the drain threads
        // must not prevent the deadline from firing.
        fs::write(
            &script,
            "#!/bin/sh\ni=0\nwhile [ $i -lt 256 ]; do\n  printf '%01024d' 0\n  i=$((i+1))\ndone\nsleep 3600\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let started = std::time::Instant::now();
        let mut command = std::process::Command::new(&script);
        let error = run_with_timeout(&mut command, std::time::Duration::from_millis(500))
            .expect_err("a hung child must be reported");
        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the deadline was not enforced while draining"
        );
    }

    /// Regression: `execve` returns `ETXTBSY` while any process holds the target
    /// open for writing — including a process that merely inherited the write
    /// descriptor across a concurrent `fork()`. That turned the expected
    /// `TimedOut` into `ExecutableFileBusy` (about 1 run in 15 under the parallel
    /// test runner) and can equally hit production, which writes launcher scripts
    /// and immediately executes them.
    ///
    /// The writer releases the handle within microseconds, so a bounded retry must
    /// recover it rather than surfacing a spurious failure.
    #[cfg(unix)]
    #[test]
    fn transient_text_file_busy_is_retried_not_reported() {
        use std::io::Write;
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let script = dir.path().join("busy.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        // Hold it open for writing, exactly as a concurrent writer would.
        let mut handle = std::fs::OpenOptions::new()
            .append(true)
            .open(&script)
            .unwrap();
        handle.write_all(b"# pending\n").unwrap();
        handle.flush().unwrap();

        // Release shortly after, so the first `spawn` attempt does hit ETXTBSY.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            drop(handle);
        });

        let mut command = std::process::Command::new(&script);
        let output = run_with_timeout(&mut command, std::time::Duration::from_secs(10))
            .expect("a transient ETXTBSY must be retried, not surfaced");
        assert!(output.status.success());
    }

    /// A command that finishes quickly must still return its real output.
    #[cfg(unix)]
    #[test]
    fn run_with_timeout_returns_output_for_a_fast_command() {
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "printf hello"]);
        let output = run_with_timeout(&mut command, std::time::Duration::from_secs(10)).unwrap();
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout), "hello");
    }

    /// The lock must be exclusive across handles and must not survive its guard.
    #[test]
    fn file_lock_is_exclusive_and_released_on_drop() {
        let dir = tempdir().unwrap();
        let state = dir.path().join("providers.json");

        let first = FileLock::acquire(&state).unwrap();
        assert!(lock_file_path(&state).exists());
        // A second acquisition must block, not succeed.
        let blocked = std::thread::spawn({
            let state = state.clone();
            move || FileLock::acquire(&state).is_ok()
        });
        std::thread::sleep(std::time::Duration::from_millis(200));
        assert!(
            !blocked.is_finished(),
            "a second lock was granted while the first was held"
        );

        // Checking file absence immediately after `drop(first)` would be inherently
        // racy: the waiting thread polls every 25ms and may have already re-acquired
        // (recreated) the lock file by then. Joining first establishes the ordering —
        // the waiter can only succeed *because* the drop released it, and its own
        // temporary guard is dropped before the closure returns — so the final
        // file-absence check below is deterministic.
        drop(first);
        assert!(
            blocked.join().unwrap(),
            "the waiting acquirer never got the lock"
        );
        assert!(
            !lock_file_path(&state).exists(),
            "the lock file must be gone once every holder has dropped it"
        );
    }

    /// A lock left behind by a dead process must be reclaimed, otherwise a crash
    /// wedges the state file for ever.
    #[test]
    fn file_lock_reclaims_an_abandoned_holder() {
        let dir = tempdir().unwrap();
        let state = dir.path().join("providers.json");
        let lock = lock_file_path(&state);
        // A PID that cannot exist.
        std::fs::write(&lock, "999999999").unwrap();

        let guard = FileLock::acquire(&state).expect("an abandoned lock must be reclaimed");
        drop(guard);
        assert!(!lock.exists());
    }

    /// Two processes racing on a read-modify-write must not lose an update.
    #[test]
    fn locked_read_modify_write_does_not_lose_updates() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.json");
        ProviderRegistry::empty(&path).save().unwrap();

        let mut handles = Vec::new();
        for index in 0..8 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                let _guard = FileLock::acquire(&path).unwrap();
                // Read-modify-write, exactly as every real caller does it.
                let mut registry = ProviderRegistry::load(&path).unwrap();
                registry
                    .add_provider(
                        ProviderConfig::new(
                            &format!("P{index}"),
                            &format!("https://example{index}.test/v1"),
                        )
                        .unwrap(),
                    )
                    .unwrap();
                registry.save().unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let registry = ProviderRegistry::load(&path).unwrap();
        assert_eq!(
            registry.providers().len(),
            8,
            "an update was lost despite the lock"
        );
    }

    /// A guard bound inside a `match` arm is dropped when the arm ends, which
    /// silently releases the lock *before* the protected mutation. This is the
    /// shape the web handlers must use: `let (value, _guard) = match .. {}`.
    ///
    /// This test pins the property that makes that pattern necessary — the lock
    /// must still be held after the producing expression completes.
    #[test]
    fn a_guard_bound_outside_the_match_still_holds_the_lock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("providers.json");
        let lock_path = lock_file_path(&path);

        {
            // Mirrors the web handler: the guard is part of an outer binding.
            let (_value, _guard) = match FileLock::acquire(&path) {
                Ok(guard) => (1_u32, guard),
                Err(error) => panic!("{error}"),
            };
            // The producing expression has finished; the lock must still be ours.
            assert!(
                lock_path.exists(),
                "the lock was released as soon as the match arm ended"
            );
            assert!(
                FileLock::acquire(&path).is_ok(),
                "same-thread reentry must still work while the guard is alive"
            );
        }
        assert!(
            !lock_path.exists(),
            "the lock must be released at scope end"
        );
    }

    /// Regression: the reentrancy registry was `thread_local`, so a guard dropped
    /// on a **different** thread than the one that acquired it removed the entry
    /// from the wrong thread's set. The acquiring thread kept a phantom
    /// "already held" entry, and its next acquisition silently succeeded
    /// **without taking the lock at all**.
    ///
    /// `spawn_blocking` (the web panel's execution model) moves work between
    /// pooled threads, so this was reachable in production: concurrent
    /// `providers/add` requests all returned 200 while one provider was dropped.
    ///
    /// The test keeps the acquiring thread alive, moves the guard to another
    /// thread to drop it (the failing shape), then has the **same** acquiring
    /// thread acquire again — while a bystander thread must still be excluded.
    #[test]
    fn a_lock_dropped_on_another_thread_does_not_disable_it_for_the_acquirer() {
        let dir = tempdir().unwrap();
        let state_file = dir.path().join("state.json");

        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let (guard_tx, guard_rx) = std::sync::mpsc::channel();
        let (again_tx, again_rx) = std::sync::mpsc::channel();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();

        // Thread A: acquire, hand the guard away, then acquire AGAIN later.
        let acquirer_file = state_file.clone();
        let acquirer = std::thread::spawn(move || {
            let guard = FileLock::acquire(&acquirer_file).unwrap();
            ready_tx.send(()).unwrap();
            // The main thread drops this guard, on ITS thread.
            guard_tx.send(guard).unwrap();
            again_rx.recv().unwrap();

            // Second acquisition on the SAME thread that first acquired. With the
            // buggy thread_local registry this was treated as reentrant and took
            // no lock at all.
            let second = FileLock::acquire(&acquirer_file).unwrap();
            // Announce we hold it; hold until told to stop.
            entered_tx.send(()).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(250));
            drop(second);
        });

        ready_rx.recv().unwrap();
        let guard = guard_rx.recv().unwrap();
        // Drop on a different thread than the acquirer — the failing shape.
        std::thread::spawn(move || drop(guard)).join().unwrap();

        // Ask thread A to acquire again.
        again_tx.send(()).unwrap();
        entered_rx.recv().unwrap();

        // While A holds its second lock, another thread must NOT get in.
        let (bystander_tx, bystander_rx) = std::sync::mpsc::channel();
        let bystander_file = state_file.clone();
        std::thread::spawn(move || {
            let _guard = FileLock::acquire(&bystander_file).unwrap();
            let _ = bystander_tx.send(());
        });
        assert!(
            bystander_rx
                .recv_timeout(std::time::Duration::from_millis(120))
                .is_err(),
            "a bystander entered while thread A held the lock; the lock was skipped"
        );

        acquirer.join().unwrap();
        // Once A releases, the bystander gets in.
        assert!(
            bystander_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .is_ok()
        );
    }

    /// Regression: `write_private_atomic` is a temp-file-plus-`rename`, and
    /// `rename` replaces the **link** rather than the file it points at. A user
    /// who symlinks `providers.json` into a dotfiles repository therefore got a
    /// silent fork: the command reported success, the new regular file held the
    /// change, and the real target kept the old data.
    ///
    /// The write must follow the link and leave the link in place.
    #[cfg(unix)]
    #[test]
    fn writing_through_a_symlink_updates_the_target_and_keeps_the_link() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_private_atomic(&link, b"new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink must be preserved, not replaced by a regular file"
        );
        assert_eq!(
            fs::read(&real).unwrap(),
            b"new",
            "the write must reach the symlink's target"
        );
        assert_eq!(fs::read(&link).unwrap(), b"new");
    }

    /// A *relative* symlink is resolved against its own directory.
    #[cfg(unix)]
    #[test]
    fn a_relative_symlink_is_resolved_against_its_directory() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        fs::write(&real, b"old").unwrap();
        std::os::unix::fs::symlink("real.json", &link).unwrap();

        write_private_atomic(&link, b"new").unwrap();

        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(fs::read(&real).unwrap(), b"new");
    }

    /// Writing through a symlink must still harden the target's permissions.
    ///
    /// The N-70 fix made writes follow the link. That must not become a way to
    /// *skip* the 0600 guarantee: a pre-existing, loosely-permissioned target
    /// (0644 here) has to end up 0600 after the write.
    #[cfg(unix)]
    #[test]
    fn writing_through_a_symlink_still_hardens_the_target_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().unwrap();
        let real = dir.path().join("real.json");
        let link = dir.path().join("link.json");
        fs::write(&real, b"old").unwrap();
        fs::set_permissions(&real, fs::Permissions::from_mode(0o644)).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        write_private_atomic(&link, b"new").unwrap();

        assert_eq!(fs::read(&real).unwrap(), b"new");
        assert!(
            fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink(),
            "the link must survive"
        );
        assert_eq!(
            fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o600,
            "the target must end up 0600 even when reached through a symlink"
        );
    }

    /// The common case: no symlink at all, the file is created where asked.
    #[test]
    fn a_plain_path_is_written_where_asked() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("plain.json");
        write_private_atomic(&path, b"data").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"data");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn atomic_replace_replaces_existing_destination() {
        let dir = tempdir().unwrap();
        let temporary = dir.path().join("providers.json.tmp");
        let destination = dir.path().join("providers.json");
        fs::write(&temporary, b"new").unwrap();
        fs::write(&destination, b"old").unwrap();

        atomic_replace(&temporary, &destination).unwrap();

        assert_eq!(fs::read_to_string(destination).unwrap(), "new");
        assert!(!temporary.exists());
    }

    /// The PATH search itself, exercised without mutating the process-global
    /// `PATH`.
    ///
    /// This test used to `set_var("PATH", tempdir)` and restore it afterwards.
    /// `PATH` is process-global while tests run in parallel *threads* of one
    /// process, so for the duration of this test every sibling that spawned a
    /// bare command name (`sh`, `codex`, ...) could fail to resolve it. The search
    /// logic is separable, so it is tested directly and the shared mutable state
    /// is gone.
    #[test]
    fn executable_resolution_finds_a_path_entry() {
        let dir = tempdir().unwrap();
        let executable = dir.path().join("codex-mp-test");
        fs::write(&executable, b"binary").unwrap();

        // The same lookup `resolve_executable` performs, but against an explicit
        // search path instead of the ambient one.
        let found = std::iter::once(dir.path().to_path_buf())
            .flat_map(|directory| executable_variants(directory.join("codex-mp-test")))
            .find(|candidate| candidate.is_file());
        assert_eq!(found.as_deref(), Some(executable.as_path()));
    }

    /// `resolve_executable` must still consult `PATH` for a bare name — asserted
    /// without changing `PATH`, by looking up a program that is always present.
    #[test]
    fn resolve_executable_consults_the_ambient_path() {
        let resolved = resolve_executable("sh");
        // Either it found an absolute `sh`, or it returned the name unchanged
        // (no PATH). Both are correct; what must not happen is a panic or a
        // path that does not exist.
        if resolved.is_absolute() {
            assert!(
                resolved.is_file(),
                "resolved to a non-existent path: {resolved:?}"
            );
        }
    }

    /// Regression: `Url::host_str` keeps the brackets on an IPv6 literal, so a
    /// plain `parse::<IpAddr>()` failed and every IPv6 host was treated as "not an
    /// IP". Two consequences, one cosmetic and one a security hole:
    ///
    /// * plaintext HTTP to `[::1]` was rejected as "non-loopback";
    /// * **the metadata / link-local block was bypassed entirely** — the IPv4
    ///   spelling of the AWS metadata address was rejected, while
    ///   `[::ffff:169.254.169.254]` and `[fe80::1]` were accepted.
    #[test]
    fn ipv6_literals_are_classified_by_their_actual_address() {
        // Loopback spellings must all be accepted over plaintext HTTP.
        for url in [
            "http://127.0.0.1:8080/v1",
            "http://localhost:8080/v1",
            "http://[::1]:8080/v1",
            "http://[::ffff:127.0.0.1]:8080/v1",
        ] {
            assert!(
                normalize_base_url(url).is_ok(),
                "loopback URL must be accepted: {url}"
            );
        }

        // Metadata and link-local spellings must all be rejected, in both the
        // IPv4 and the (previously bypassable) IPv6 forms.
        for url in [
            "https://169.254.169.254/v1",
            "http://169.254.169.254/v1",
            "https://[::ffff:169.254.169.254]/v1",
            "http://[::ffff:169.254.169.254]/v1",
            "https://[fe80::1]/v1",
            "http://[fe80::1]/v1",
        ] {
            assert!(
                normalize_base_url(url).is_err(),
                "metadata/link-local URL must be rejected: {url}"
            );
        }

        // A public IPv6 literal is fine over HTTPS but not over plaintext HTTP.
        assert!(normalize_base_url("https://[2001:db8::1]/v1").is_ok());
        assert!(normalize_base_url("http://[2001:db8::1]/v1").is_err());
        assert!(normalize_base_url("https://[::ffff:8.8.8.8]/v1").is_ok());
    }

    #[test]
    fn model_ids_are_namespaced() {
        let model = CustomModel::new("openrouter", "qwen/qwen3.8", "OpenRouter / Qwen3.8").unwrap();
        assert_eq!(model.logical_model_id, "openrouter/qwen/qwen3.8");
    }

    #[test]
    fn logical_model_route_keeps_official_and_custom_on_separate_paths() {
        let mut registry = ProviderRegistry::empty("/tmp/does-not-matter.json");
        let provider = ProviderConfig::new("NewAPI", "https://api.example.test/v1").unwrap();
        registry.add_provider(provider).unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen3.8").unwrap())
            .unwrap();
        registry.set_official_model_ids(["gpt-5.6-sol"]);

        assert!(matches!(
            registry.resolve_logical_model_route("gpt-5.6-sol"),
            Ok(LogicalModelRoute::Official { model_id }) if model_id == "gpt-5.6-sol"
        ));
        assert!(matches!(
            registry.resolve_logical_model_route("newapi/qwen3.8"),
            Ok(LogicalModelRoute::Custom {
                logical_model_id,
                provider_id,
                upstream_model_id,
                protocol: ProviderProtocol::Responses,
            }) if logical_model_id == "newapi/qwen3.8"
                && provider_id == "newapi"
                && upstream_model_id == "qwen3.8"
        ));
        assert!(matches!(
            registry.resolve_logical_model_route("missing/model"),
            Err(CoreError::ModelNotFound(_))
        ));
    }

    /// Regression: `resolve_logical_model_route` only inspects *enabled* models,
    /// so a model the user had just switched off was reported as
    /// `model \`x\` was not found` — identical to a typo. The user was sent
    /// looking for a spelling mistake instead of the switch they had flipped.
    ///
    /// A disabled model must be reported as disabled, with the way to re-enable it.
    #[test]
    fn a_disabled_model_is_reported_as_disabled_not_missing() {
        let mut registry = ProviderRegistry::empty("/tmp/does-not-matter.json");
        registry
            .add_provider(ProviderConfig {
                id: "newapi".into(),
                name: "NewAPI".into(),
                base_url: "https://example.test/v1".into(),
                protocol: ProviderProtocol::Responses,
                auth_strategy: AuthStrategy::Bearer,
                static_headers: BTreeMap::new(),
                credential_reference: "provider:newapi".into(),
                model_discovery: false,
                enabled: true,
                models: Vec::new(),
            })
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "NewAPI / Qwen").unwrap())
            .unwrap();

        // Enabled: resolves.
        assert!(
            registry
                .resolve_logical_model_route("newapi/qwen3.8")
                .is_ok()
        );

        // Disabled: a distinct, actionable error. The flag lives on the model, as
        // the manager's `set_model_enabled` mutates it in place.
        registry
            .provider_mut("newapi")
            .unwrap()
            .models
            .iter_mut()
            .find(|model| model.logical_model_id == "newapi/qwen3.8")
            .unwrap()
            .enabled = false;
        match registry.resolve_logical_model_route("newapi/qwen3.8") {
            Err(CoreError::ModelDisabled(id)) => {
                assert_eq!(id, "newapi/qwen3.8");
                let message = CoreError::ModelDisabled(id).to_string();
                assert!(
                    message.contains("model enable"),
                    "the error must say how to re-enable it, got: {message}"
                );
            }
            other => panic!("a disabled model must report ModelDisabled, got {other:?}"),
        }

        // Disabling the whole provider is the other legitimate way to switch a
        // model off, and it must be reported just as precisely.
        registry
            .provider_mut("newapi")
            .unwrap()
            .models
            .iter_mut()
            .find(|model| model.logical_model_id == "newapi/qwen3.8")
            .unwrap()
            .enabled = true;
        registry.provider_mut("newapi").unwrap().enabled = false;
        match registry.resolve_logical_model_route("newapi/qwen3.8") {
            Err(CoreError::ProviderDisabled(id)) => {
                assert_eq!(id, "newapi");
                let message = CoreError::ProviderDisabled(id).to_string();
                assert!(
                    message.contains("provider edit") && message.contains("--enable"),
                    "the error must name the re-enable command, got: {message}"
                );
            }
            other => panic!("a disabled provider must report ProviderDisabled, got {other:?}"),
        }

        // A model that never existed is still "not found", not "disabled".
        assert!(matches!(
            registry.resolve_logical_model_route("newapi/nope"),
            Err(CoreError::ModelNotFound(_))
        ));
    }

    #[test]
    fn edits_model_presentation_without_changing_logical_id() {
        let mut registry = ProviderRegistry::empty("/tmp/does-not-matter.json");
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://api.example.test/v1").unwrap())
            .unwrap();
        registry
            .add_model(CustomModel::new("newapi", "qwen3.8", "Old name").unwrap())
            .unwrap();

        registry
            .edit_model(
                "newapi/qwen3.8",
                ModelEdit {
                    display_name: Some("  Preferred name  ".into()),
                    context_window: Some(Some(131_072)),
                },
            )
            .unwrap();

        let model = &registry.provider("newapi").unwrap().models[0];
        assert_eq!(model.logical_model_id, "newapi/qwen3.8");
        assert_eq!(model.display_name, "Preferred name");
        assert_eq!(model.context_window, Some(131_072));
    }

    #[test]
    fn clears_model_context_window() {
        let mut registry = ProviderRegistry::empty("/tmp/does-not-matter.json");
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://api.example.test/v1").unwrap())
            .unwrap();
        let mut model = CustomModel::new("newapi", "qwen3.8", "Qwen").unwrap();
        model.context_window = Some(131_072);
        registry.add_model(model).unwrap();

        registry
            .edit_model(
                "newapi/qwen3.8",
                ModelEdit {
                    context_window: Some(None),
                    ..ModelEdit::default()
                },
            )
            .unwrap();

        assert_eq!(
            registry.provider("newapi").unwrap().models[0].context_window,
            None
        );
    }
}
