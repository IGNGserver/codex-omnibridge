//! Shared domain types and safe, non-secret provider registry persistence.
//! API keys are deliberately absent from every persisted type in this crate.

use std::collections::BTreeSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;

pub const REGISTRY_SCHEMA_VERSION: u32 = 1;

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
}

impl Default for ProviderRegistryFile {
    fn default() -> Self {
        Self {
            schema_version: REGISTRY_SCHEMA_VERSION,
            providers: Vec::new(),
        }
    }
}

impl ProviderRegistryFile {
    pub fn validate(&self) -> Result<(), CoreError> {
        if self.schema_version != REGISTRY_SCHEMA_VERSION {
            return Err(CoreError::UnsupportedSchema(self.schema_version));
        }
        let mut ids = BTreeSet::new();
        for provider in &self.providers {
            provider.validate()?;
            if !ids.insert(provider.id.clone()) {
                return Err(CoreError::ProviderExists(provider.id.clone()));
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
        let file: ProviderRegistryFile = serde_json::from_str(&content)?;
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
        Ok(())
    }

    pub fn remove_provider(&mut self, id: &str) -> Result<ProviderConfig, CoreError> {
        let index = self
            .file
            .providers
            .iter()
            .position(|provider| provider.id == id)
            .ok_or_else(|| CoreError::ProviderNotFound(id.to_owned()))?;
        Ok(self.file.providers.remove(index))
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
        Ok(provider.models.remove(index))
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
        let Some((provider_id, upstream_model_id)) = logical_model_id.split_once('/') else {
            return Ok(LogicalModelRoute::Official {
                model_id: logical_model_id.to_owned(),
            });
        };
        let provider = self
            .provider(provider_id)
            .ok_or_else(|| CoreError::ProviderNotFound(provider_id.to_owned()))?;
        if !provider.enabled {
            return Err(CoreError::ProviderNotFound(provider_id.to_owned()));
        }
        let model = provider
            .models
            .iter()
            .find(|model| {
                model.enabled
                    && model.logical_model_id == logical_model_id
                    && model.upstream_model_id == upstream_model_id
            })
            .ok_or_else(|| CoreError::ModelNotFound(logical_model_id.to_owned()))?;
        Ok(LogicalModelRoute::Custom {
            logical_model_id: logical_model_id.to_owned(),
            provider_id: provider.id.clone(),
            upstream_model_id: model.upstream_model_id.clone(),
            protocol: provider.protocol,
        })
    }

    pub fn save(&self) -> Result<(), CoreError> {
        self.file.validate()?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let temp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec_pretty(&self.file)?;
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(&bytes)?;
            file.write_all(b"\n")?;
            file.sync_all()?;
        }
        fs::rename(temp, &self.path)?;
        set_private_permissions(&self.path)?;
        Ok(())
    }
}

pub fn default_registry_path() -> PathBuf {
    ProjectDirs::from("dev", "codex-multiprovider", "Codex MultiProvider")
        .map(|dirs| dirs.config_dir().join("providers.json"))
        .unwrap_or_else(|| PathBuf::from("providers.json"))
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
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(CoreError::InvalidBaseUrl(value.to_owned()));
    }
    Ok(value.to_owned())
}

fn default_true() -> bool {
    true
}

fn set_private_permissions(path: &Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
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
            Err(CoreError::ProviderNotFound(_))
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
