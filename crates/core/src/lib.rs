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
use uuid::Uuid;

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
        Self {
            reasoning: true,
            tools: true,
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
        Err(CoreError::ModelNotFound(logical_model_id.to_owned()))
    }

    pub fn save(&self) -> Result<(), CoreError> {
        self.file.validate()?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self
            .path
            .with_extension(format!("{}.json.tmp", Uuid::new_v4()));
        let bytes = serde_json::to_vec_pretty(&self.file)?;
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        atomic_replace(temp, &self.path)?;
        set_private_permissions(&self.path)?;
        Ok(())
    }
}

pub fn default_registry_path() -> PathBuf {
    ProjectDirs::from("dev", "codex-multiprovider", "Codex MultiProvider")
        .map(|dirs| dirs.config_dir().join("providers.json"))
        .unwrap_or_else(|| PathBuf::from("providers.json"))
}

/// Replace `destination` with `temporary` using the platform's replacement
/// primitive. Unix `rename` replaces an existing file; Windows `rename` does
/// not, so it must use MoveFileExW with MOVEFILE_REPLACE_EXISTING instead.
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
    if parsed.scheme() == "http" {
        let host = parsed.host_str().unwrap_or_default();
        let loopback = host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .map(|address| address.is_loopback())
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

    #[test]
    fn executable_resolution_finds_a_path_entry() {
        let dir = tempdir().unwrap();
        let executable = dir.path().join("codex-mp-test");
        fs::write(&executable, b"binary").unwrap();
        let previous = std::env::var_os("PATH");
        unsafe {
            std::env::set_var("PATH", dir.path());
        }

        assert_eq!(resolve_executable("codex-mp-test"), executable);

        if let Some(previous) = previous {
            unsafe {
                std::env::set_var("PATH", previous);
            }
        } else {
            unsafe {
                std::env::remove_var("PATH");
            }
        }
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
