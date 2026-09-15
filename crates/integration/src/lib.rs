//! Stock Codex integration for the single OmniBridge provider.
//!
//! This module edits only the semantic Codex config, generated catalog,
//! capability file and its own manifest. It never reads or writes `auth.json`
//! or any OAuth material.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use codex_mp_catalog::{
    discover_official_catalog, merge_catalog, official_model_ids, schema_fingerprint,
    write_catalog_atomic,
};
use codex_mp_core::{
    ProviderRegistry, atomic_replace, command_for_executable, default_registry_path,
    set_private_permissions,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

const MANIFEST_FILE: &str = "integration.json";
const MANAGED_KEY: &str = "model_catalog_json";
const MANAGED_PROVIDER_KEY: &str = "model_provider";
const OMNIBRIDGE_PROVIDER_ID: &str = "omnibridge";

#[derive(Debug, Error)]
pub enum IntegrationError {
    #[error("provider registry error: {0}")]
    Core(#[from] codex_mp_core::CoreError),
    #[error("integration IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("integration JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("integration TOML value error: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("integration TOML serialization error: {0}")]
    TomlSerialize(#[from] toml::ser::Error),
    #[error(
        "managed config field `{MANAGED_KEY}` changed outside Codex MultiProvider; refusing to overwrite it"
    )]
    UserChangedManagedField,
    #[error("Codex catalog error: {0}")]
    Catalog(#[from] codex_mp_catalog::CatalogError),
    #[error("config path is not a regular file: {0}")]
    InvalidConfigPath(PathBuf),
    #[error("catalog path is already present without a MultiProvider manifest: {0}")]
    CatalogPathAlreadyExists(PathBuf),
    #[error("integration manifest is invalid: {0}")]
    InvalidManifest(String),
    #[error("managed Codex provider configuration changed outside Codex MultiProvider")]
    UserChangedProvider,
    #[error("router capability is not available: {0}")]
    Capability(String),
}

#[derive(Debug, Clone)]
pub struct IntegrationPaths {
    pub config_dir: PathBuf,
    pub codex_config: PathBuf,
    pub catalog: PathBuf,
    pub manifest: PathBuf,
}

impl Default for IntegrationPaths {
    fn default() -> Self {
        let config_dir = default_registry_path()
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        Self {
            config_dir: config_dir.clone(),
            codex_config: default_codex_config_path(),
            catalog: config_dir.join("models.json"),
            manifest: config_dir.join(MANIFEST_FILE),
        }
    }
}

impl IntegrationPaths {
    pub fn for_registry(registry_path: impl AsRef<Path>) -> Self {
        let registry_path = registry_path.as_ref();
        let config_dir = registry_path
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from("."));
        let default_registry = default_registry_path();
        let codex_config = if registry_path == default_registry {
            default_codex_config_path()
        } else {
            config_dir.join("config.toml")
        };
        Self {
            config_dir: config_dir.clone(),
            codex_config,
            catalog: config_dir.join("models.json"),
            manifest: config_dir.join(MANIFEST_FILE),
        }
    }

    pub fn with_codex_config(mut self, path: impl Into<PathBuf>) -> Self {
        self.codex_config = path.into();
        self
    }

    pub fn capability_path(&self) -> PathBuf {
        self.config_dir.join("router-capability")
    }

    pub fn router_base_url(&self) -> String {
        std::env::var("CODEX_MP_ROUTER_BASE_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:8787/v1".into())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IntegrationManifest {
    pub schema_version: u32,
    pub config_path: PathBuf,
    pub managed_field: String,
    pub applied_value: String,
    pub original_value: Option<String>,
    pub original_key_present: bool,
    pub catalog_path: PathBuf,
    pub codex_binary: PathBuf,
    pub codex_version: Option<String>,
    #[serde(default = "default_managed_provider")]
    pub managed_provider: String,
    #[serde(default = "default_omnibridge_provider")]
    pub applied_provider: String,
    #[serde(default)]
    pub original_provider: Option<String>,
    #[serde(default)]
    pub original_provider_present: bool,
    #[serde(default)]
    pub provider_table_present: bool,
    #[serde(default)]
    pub capability_path: Option<PathBuf>,
    #[serde(default)]
    pub catalog_schema_fingerprint: Option<String>,
    #[serde(default)]
    pub official_model_count: usize,
}

pub fn default_codex_config_path() -> PathBuf {
    if let Some(home) = std::env::var_os("CODEX_HOME") {
        return PathBuf::from(home).join("config.toml");
    }
    directories::BaseDirs::new()
        .map(|dirs| dirs.home_dir().join(".codex/config.toml"))
        .unwrap_or_else(|| PathBuf::from(".codex/config.toml"))
}

pub fn build_and_install(
    paths: &IntegrationPaths,
    registry: &ProviderRegistry,
    codex_binary: impl AsRef<Path>,
) -> Result<IntegrationManifest, IntegrationError> {
    let config_existed = paths.codex_config.exists();
    let config_content = read_config(&paths.codex_config)?;
    let mut config = parse_config(&config_content)?;
    let existing_manifest = if paths.manifest.exists() {
        let manifest = load_manifest(&paths.manifest)?;
        if manifest.config_path != paths.codex_config {
            return Err(IntegrationError::InvalidConfigPath(
                paths.codex_config.clone(),
            ));
        }
        if manifest.catalog_path != paths.catalog {
            return Err(IntegrationError::InvalidManifest(
                "catalog path changed outside the integration manifest".into(),
            ));
        }
        Some(manifest)
    } else {
        None
    };
    if existing_manifest.is_none() && paths.catalog.exists() {
        return Err(IntegrationError::CatalogPathAlreadyExists(
            paths.catalog.clone(),
        ));
    }
    let previous_catalog = match fs::read(&paths.catalog) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(error.into()),
    };
    let current_value = root_string(&config, MANAGED_KEY);
    let current_provider = root_string(&config, MANAGED_PROVIDER_KEY);
    let provider_table_present = has_omnibridge_provider(&config);

    let (original_value, original_key_present, original_provider, original_provider_present) =
        if let Some(previous) = &existing_manifest {
            // A second sync must not replace the user's original value with our own
            // catalog path. Refuse to overwrite a value changed outside the manifest
            // rather than silently taking ownership of it.
            if current_value.as_deref() != Some(previous.applied_value.as_str()) {
                return Err(IntegrationError::UserChangedManagedField);
            }
            if current_provider.as_deref() != Some(previous.applied_provider.as_str())
                || !provider_table_matches(&config, paths)
            {
                return Err(IntegrationError::UserChangedProvider);
            }
            (
                previous.original_value.clone(),
                previous.original_key_present,
                previous.original_provider.clone(),
                previous.original_provider_present,
            )
        } else {
            if current_provider.as_deref() == Some(OMNIBRIDGE_PROVIDER_ID) || provider_table_present
            {
                return Err(IntegrationError::UserChangedProvider);
            }
            (
                current_value.clone(),
                current_value.is_some(),
                current_provider.clone(),
                current_provider.is_some(),
            )
        };

    let official = discover_official_catalog(&codex_binary)?;
    let official_ids = official_model_ids(&official)?;
    let catalog_schema_fingerprint = Some(schema_fingerprint(&official)?);
    let merged = merge_catalog(&official, registry)?;
    let capability_path = paths.capability_path();
    let capability = ensure_capability(&capability_path)?;
    write_catalog_atomic(&merged, &paths.catalog)?;
    let applied_value = paths.catalog.to_string_lossy().to_string();
    let router_base_url = paths.router_base_url();
    apply_omnibridge_config(&mut config, &applied_value, &router_base_url, &capability)?;
    let updated_config = toml::to_string_pretty(&config)?;
    if let Err(error) = write_config(&paths.codex_config, &updated_config) {
        let _ = restore_catalog_snapshot(&paths.catalog, previous_catalog.as_deref());
        return Err(error);
    }

    let manifest = IntegrationManifest {
        schema_version: 2,
        config_path: paths.codex_config.clone(),
        managed_field: MANAGED_KEY.into(),
        applied_value,
        original_value,
        original_key_present,
        catalog_path: paths.catalog.clone(),
        codex_binary: codex_binary.as_ref().to_path_buf(),
        codex_version: codex_version(codex_binary.as_ref()),
        managed_provider: MANAGED_PROVIDER_KEY.into(),
        applied_provider: OMNIBRIDGE_PROVIDER_ID.into(),
        original_provider,
        original_provider_present,
        provider_table_present,
        capability_path: Some(capability_path),
        catalog_schema_fingerprint,
        official_model_count: official_ids.len(),
    };
    if let Err(error) = save_manifest(&paths.manifest, &manifest) {
        let config_rollback =
            restore_config_snapshot(&paths.codex_config, &config_content, config_existed);
        let catalog_rollback =
            restore_catalog_snapshot(&paths.catalog, previous_catalog.as_deref());
        if config_rollback.is_err() || catalog_rollback.is_err() {
            return Err(IntegrationError::InvalidManifest(
                "integration save failed and rollback was incomplete".into(),
            ));
        }
        return Err(error);
    }
    let mut effective_registry = registry.clone();
    effective_registry.set_official_model_ids(official_ids);
    if let Err(error) = effective_registry.save() {
        let _ = restore_config_snapshot(&paths.codex_config, &config_content, config_existed);
        let _ = restore_catalog_snapshot(&paths.catalog, previous_catalog.as_deref());
        return Err(error.into());
    }
    Ok(manifest)
}

fn default_managed_provider() -> String {
    MANAGED_PROVIDER_KEY.into()
}

fn default_omnibridge_provider() -> String {
    OMNIBRIDGE_PROVIDER_ID.into()
}

fn parse_config(content: &str) -> Result<toml::Value, IntegrationError> {
    if content.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    Ok(toml::from_str(content)?)
}

fn root_string(config: &toml::Value, key: &str) -> Option<String> {
    config
        .as_table()
        .and_then(|table| table.get(key))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
}

fn has_omnibridge_provider(config: &toml::Value) -> bool {
    config
        .get("model_providers")
        .and_then(toml::Value::as_table)
        .is_some_and(|providers| providers.contains_key(OMNIBRIDGE_PROVIDER_ID))
}

fn provider_table_matches(config: &toml::Value, paths: &IntegrationPaths) -> bool {
    let Some(provider) = config
        .get("model_providers")
        .and_then(toml::Value::as_table)
        .and_then(|providers| providers.get(OMNIBRIDGE_PROVIDER_ID))
        .and_then(toml::Value::as_table)
    else {
        return false;
    };
    let expected_base = paths.router_base_url();
    let capability = fs::read_to_string(paths.capability_path())
        .ok()
        .map(|value| value.trim().to_owned());
    let header_matches = provider
        .get("http_headers")
        .and_then(toml::Value::as_table)
        .and_then(|headers| headers.get("x-codex-omnibridge-token"))
        .and_then(toml::Value::as_str)
        .zip(capability.as_deref())
        .is_some_and(|(left, right)| left == right);
    provider.get("name").and_then(toml::Value::as_str) == Some("OpenAI")
        && provider.get("base_url").and_then(toml::Value::as_str) == Some(expected_base.as_str())
        && provider.get("wire_api").and_then(toml::Value::as_str) == Some("responses")
        && provider
            .get("requires_openai_auth")
            .and_then(toml::Value::as_bool)
            == Some(true)
        && provider
            .get("supports_websockets")
            .and_then(toml::Value::as_bool)
            == Some(false)
        && header_matches
}

fn apply_omnibridge_config(
    config: &mut toml::Value,
    catalog_path: &str,
    base_url: &str,
    capability: &str,
) -> Result<(), IntegrationError> {
    let root = config.as_table_mut().ok_or_else(|| {
        IntegrationError::InvalidManifest("root TOML value is not a table".into())
    })?;
    root.insert(
        MANAGED_PROVIDER_KEY.into(),
        toml::Value::String(OMNIBRIDGE_PROVIDER_ID.into()),
    );
    root.insert(MANAGED_KEY.into(), toml::Value::String(catalog_path.into()));
    let providers = root
        .entry("model_providers")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .ok_or_else(|| {
            IntegrationError::InvalidManifest("model_providers must be a table".into())
        })?;
    let mut bridge = toml::map::Map::new();
    bridge.insert("name".into(), toml::Value::String("OpenAI".into()));
    bridge.insert("base_url".into(), toml::Value::String(base_url.into()));
    bridge.insert("wire_api".into(), toml::Value::String("responses".into()));
    bridge.insert("requires_openai_auth".into(), toml::Value::Boolean(true));
    bridge.insert("supports_websockets".into(), toml::Value::Boolean(false));
    let mut headers = toml::map::Map::new();
    headers.insert(
        "x-codex-omnibridge-token".into(),
        toml::Value::String(capability.into()),
    );
    bridge.insert("http_headers".into(), toml::Value::Table(headers));
    providers.insert(OMNIBRIDGE_PROVIDER_ID.into(), toml::Value::Table(bridge));
    Ok(())
}

fn restore_root_string(
    root: &mut toml::map::Map<String, toml::Value>,
    key: &str,
    value: Option<&str>,
) {
    if let Some(value) = value {
        root.insert(key.into(), toml::Value::String(value.into()));
    } else {
        root.remove(key);
    }
}

pub fn ensure_capability(path: &Path) -> Result<String, IntegrationError> {
    if let Ok(metadata) = fs::symlink_metadata(path) {
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(IntegrationError::Capability(format!(
                "capability path is not a regular file: {}",
                path.display()
            )));
        }
        let value = fs::read_to_string(path)?.trim().to_owned();
        if value.is_empty() {
            return Err(IntegrationError::Capability(
                "capability file is empty".into(),
            ));
        }
        set_private_permissions(path)?;
        return Ok(value);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let value = Uuid::new_v4().to_string();
    let temp = path.with_extension(format!("{}.tmp", Uuid::new_v4()));
    fs::write(&temp, format!("{value}\n"))?;
    set_private_permissions(&temp)?;
    atomic_replace(temp, path)?;
    set_private_permissions(path)?;
    Ok(value)
}

pub fn repair(
    paths: &IntegrationPaths,
    registry: &ProviderRegistry,
    codex_binary: impl AsRef<Path>,
) -> Result<IntegrationManifest, IntegrationError> {
    if paths.manifest.exists() {
        let old = load_manifest(&paths.manifest)?;
        if old.config_path != paths.codex_config {
            return Err(IntegrationError::InvalidConfigPath(
                paths.codex_config.clone(),
            ));
        }
    }
    build_and_install(paths, registry, codex_binary)
}

pub fn restore(paths: &IntegrationPaths) -> Result<(), IntegrationError> {
    let manifest = load_manifest(&paths.manifest)?;
    if manifest.schema_version >= 2 {
        let content = read_config(&manifest.config_path)?;
        let mut config = parse_config(&content)?;
        if root_string(&config, &manifest.managed_field).as_deref()
            != Some(manifest.applied_value.as_str())
            || root_string(&config, &manifest.managed_provider).as_deref()
                != Some(manifest.applied_provider.as_str())
            || !has_omnibridge_provider(&config)
        {
            return Err(IntegrationError::UserChangedProvider);
        }
        let root = config.as_table_mut().ok_or_else(|| {
            IntegrationError::InvalidManifest("root TOML value is not a table".into())
        })?;
        restore_root_string(
            root,
            &manifest.managed_field,
            manifest.original_value.as_deref(),
        );
        restore_root_string(
            root,
            &manifest.managed_provider,
            manifest.original_provider.as_deref(),
        );
        if let Some(providers) = root
            .get_mut("model_providers")
            .and_then(toml::Value::as_table_mut)
        {
            providers.remove(OMNIBRIDGE_PROVIDER_ID);
            if providers.is_empty() {
                root.remove("model_providers");
            }
        }
        write_config(&manifest.config_path, &toml::to_string_pretty(&config)?)?;
        if manifest.catalog_path.exists() {
            fs::remove_file(&manifest.catalog_path)?;
        }
        if let Some(capability) = manifest.capability_path.as_ref()
            && capability.exists()
        {
            fs::remove_file(capability)?;
        }
        fs::remove_file(&paths.manifest)?;
        return Ok(());
    }

    // v1 manifests were catalog-only and are restored with the legacy guard.
    let current = read_root_string_field(&manifest.config_path, &manifest.managed_field)?;
    let already_restored = if manifest.original_key_present {
        current.as_deref() == manifest.original_value.as_deref()
    } else {
        current.is_none()
    };
    if current.as_deref() != Some(manifest.applied_value.as_str()) && !already_restored {
        return Err(IntegrationError::UserChangedManagedField);
    }
    if !already_restored {
        let mut content = read_config(&manifest.config_path)?;
        if manifest.original_key_present {
            let original_value = manifest.original_value.as_deref().ok_or_else(|| {
                IntegrationError::InvalidManifest(
                    "original_key_present requires original_value".into(),
                )
            })?;
            replace_root_string_line(&mut content, &manifest.managed_field, original_value);
        } else {
            remove_root_key_line(&mut content, &manifest.managed_field);
        }
        write_config(&manifest.config_path, &content)?;
    }
    if manifest.catalog_path.exists() {
        fs::remove_file(&manifest.catalog_path)?;
    }
    fs::remove_file(&paths.manifest)?;
    Ok(())
}

pub fn restore_if_present(paths: &IntegrationPaths) -> Result<bool, IntegrationError> {
    if !paths.manifest.exists() {
        return Ok(false);
    }
    restore(paths)?;
    Ok(true)
}

pub fn load_manifest(path: impl AsRef<Path>) -> Result<IntegrationManifest, IntegrationError> {
    let manifest: IntegrationManifest = serde_json::from_str(&fs::read_to_string(path)?)?;
    if !matches!(manifest.schema_version, 1 | 2) {
        return Err(IntegrationError::InvalidManifest(format!(
            "unsupported schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.managed_field != MANAGED_KEY {
        return Err(IntegrationError::InvalidManifest(format!(
            "managed field must be `{MANAGED_KEY}`"
        )));
    }
    if manifest.schema_version >= 2 && manifest.managed_provider != MANAGED_PROVIDER_KEY {
        return Err(IntegrationError::InvalidManifest(format!(
            "managed provider field must be `{MANAGED_PROVIDER_KEY}`"
        )));
    }
    if manifest.original_key_present && manifest.original_value.is_none() {
        return Err(IntegrationError::InvalidManifest(
            "original_key_present requires original_value".into(),
        ));
    }
    Ok(manifest)
}

pub fn save_manifest(
    path: impl AsRef<Path>,
    manifest: &IntegrationManifest,
) -> Result<(), IntegrationError> {
    let path = path.as_ref();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("{}.json.tmp", Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(manifest)?;
    {
        let mut file = fs::File::create(&temp)?;
        use std::io::Write;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
    }
    atomic_replace(temp, path)?;
    set_private_permissions(path)?;
    Ok(())
}

pub fn read_root_string_field(
    path: impl AsRef<Path>,
    key: &str,
) -> Result<Option<String>, IntegrationError> {
    let content = read_config(path.as_ref())?;
    read_root_string_field_from_content(&content, key)
}

fn read_root_string_field_from_content(
    content: &str,
    key: &str,
) -> Result<Option<String>, IntegrationError> {
    let Some(line) = root_key_line(content, key) else {
        return Ok(None);
    };
    let rhs = line
        .split_once('=')
        .map(|(_, rhs)| rhs.trim())
        .unwrap_or_default();
    let value: toml::Value = toml::from_str(&format!("value = {rhs}"))?;
    Ok(value
        .get("value")
        .and_then(toml::Value::as_str)
        .map(str::to_owned))
}

pub fn set_root_string_field(
    path: impl AsRef<Path>,
    key: &str,
    value: &str,
) -> Result<(), IntegrationError> {
    let path = path.as_ref();
    if path.exists() && !path.is_file() {
        return Err(IntegrationError::InvalidConfigPath(path.to_path_buf()));
    }
    let mut content = read_config(path)?;
    set_root_string_field_in_content(&mut content, key, value)?;
    write_config(path, &content)
}

fn restore_catalog_snapshot(path: &Path, previous: Option<&[u8]>) -> Result<(), IntegrationError> {
    match previous {
        Some(bytes) => write_bytes_atomic(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        },
    }
}

fn restore_config_snapshot(
    path: &Path,
    content: &str,
    existed: bool,
) -> Result<(), IntegrationError> {
    if existed {
        write_config(path, content)
    } else {
        match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.into()),
        }
    }
}

fn write_bytes_atomic(path: &Path, bytes: &[u8]) -> Result<(), IntegrationError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("{}.json.tmp", Uuid::new_v4()));
    {
        let mut file = fs::File::create(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    atomic_replace(temp, path)?;
    set_private_permissions(path)?;
    Ok(())
}

fn set_root_string_field_in_content(
    content: &mut String,
    key: &str,
    value: &str,
) -> Result<(), IntegrationError> {
    if root_key_line(content, key).is_some() {
        replace_root_string_line(content, key, value);
        return Ok(());
    }

    let line_ending = if content.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let line = format!(
        "{key} = {}{line_ending}",
        toml::Value::String(value.to_owned())
    );
    if let Some(insert_at) = first_table_offset(content) {
        content.insert_str(insert_at, &line);
    } else {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push_str(line_ending);
        }
        content.push_str(&line);
    }
    Ok(())
}

fn root_key_line<'a>(content: &'a str, key: &str) -> Option<&'a str> {
    let mut in_table = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        if !in_table && is_key_line(trimmed, key) {
            return Some(line);
        }
    }
    None
}

fn first_table_offset(content: &str) -> Option<usize> {
    let mut offset = 0;
    for segment in content.split_inclusive('\n') {
        let line = segment.strip_suffix('\n').unwrap_or(segment);
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim_start().starts_with('[') {
            return Some(offset);
        }
        offset += segment.len();
    }
    if content.trim_start().starts_with('[') {
        Some(0)
    } else {
        None
    }
}

fn is_key_line(line: &str, key: &str) -> bool {
    line.strip_prefix(key)
        .is_some_and(|rest| rest.trim_start().starts_with('='))
}

fn replace_root_string_line(content: &mut String, key: &str, value: &str) {
    let mut output = String::with_capacity(content.len() + value.len());
    let mut in_table = false;
    for segment in content.split_inclusive('\n') {
        let (line, ending) = split_line_ending(segment);
        let trimmed = line.trim_start();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        if !in_table && is_key_line(trimmed, key) {
            let indent = &line[..line.len() - trimmed.len()];
            output.push_str(indent);
            output.push_str(key);
            output.push_str(" = ");
            output.push_str(&toml::Value::String(value.to_owned()).to_string());
        } else {
            output.push_str(line);
        }
        output.push_str(ending);
    }
    *content = output;
}

fn remove_root_key_line(content: &mut String, key: &str) {
    let mut output = String::new();
    let mut in_table = false;
    for segment in content.split_inclusive('\n') {
        let (line, ending) = split_line_ending(segment);
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_table = true;
        }
        if !in_table && is_key_line(trimmed, key) {
            continue;
        }
        output.push_str(line);
        output.push_str(ending);
    }
    *content = output;
}

fn split_line_ending(segment: &str) -> (&str, &str) {
    if let Some(line) = segment.strip_suffix("\r\n") {
        (line, "\r\n")
    } else if let Some(line) = segment.strip_suffix('\n') {
        (line, "\n")
    } else {
        (segment, "")
    }
}

fn read_config(path: &Path) -> Result<String, IntegrationError> {
    match fs::read_to_string(path) {
        Ok(content) => Ok(content),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(error.into()),
    }
}

fn write_config(path: &Path, content: &str) -> Result<(), IntegrationError> {
    if let Ok(metadata) = fs::symlink_metadata(path)
        && (metadata.file_type().is_symlink() || !metadata.is_file())
    {
        return Err(IntegrationError::InvalidConfigPath(path.to_path_buf()));
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = path.with_extension(format!("{}.toml.tmp", Uuid::new_v4()));
    let mut file = fs::File::create(&temp)?;
    file.write_all(content.as_bytes())?;
    file.sync_all()?;
    atomic_replace(temp, path)?;
    set_private_permissions(path)?;
    Ok(())
}

fn codex_version(path: &Path) -> Option<String> {
    command_for_executable(path)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_core::{ProviderConfig, ProviderRegistry};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn test_codex_binary(dir: &Path) -> PathBuf {
        let catalog = r#"{"models":[{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","shell_type":"shell_command","model_messages":{"persistent_instructions":"safe","instructions_template":"safe"} }]}"#;

        #[cfg(unix)]
        {
            let path = dir.join("codex-stub");
            fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", catalog)).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
            path
        }

        #[cfg(windows)]
        {
            let path = dir.join("codex-stub.cmd");
            fs::write(&path, format!("@echo off\r\necho {}\r\n", catalog)).unwrap();
            path
        }
    }

    #[test]
    fn patch_only_touches_root_model_catalog_field() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "model_provider = \"openai\"\n\n[profiles.work]\nmodel = \"gpt-5.6-sol\"\n",
        )
        .unwrap();
        set_root_string_field(&path, MANAGED_KEY, "/tmp/models.json").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("model_provider = \"openai\""));
        assert!(text.contains("[profiles.work]"));
        let catalog_pos = text
            .find("model_catalog_json = \"/tmp/models.json\"")
            .unwrap();
        let table_pos = text.find("[profiles.work]").unwrap();
        assert!(catalog_pos < table_pos);
        assert_eq!(
            read_root_string_field(&path, MANAGED_KEY)
                .unwrap()
                .as_deref(),
            Some("/tmp/models.json")
        );
    }

    #[test]
    fn restore_refuses_to_overwrite_user_change() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let manifest_path = dir.path().join("integration.json");
        fs::write(&path, "model_catalog_json = \"/tmp/other.json\"\n").unwrap();
        let manifest = IntegrationManifest {
            schema_version: 1,
            config_path: path.clone(),
            managed_field: MANAGED_KEY.into(),
            applied_value: "/tmp/models.json".into(),
            original_value: None,
            original_key_present: false,
            catalog_path: dir.path().join("models.json"),
            codex_binary: PathBuf::from("codex"),
            codex_version: None,
            managed_provider: MANAGED_PROVIDER_KEY.into(),
            applied_provider: OMNIBRIDGE_PROVIDER_ID.into(),
            original_provider: None,
            original_provider_present: false,
            provider_table_present: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
        };
        save_manifest(&manifest_path, &manifest).unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: path,
            catalog: manifest.catalog_path.clone(),
            manifest: manifest_path,
        };
        assert!(matches!(
            restore(&paths),
            Err(IntegrationError::UserChangedManagedField)
        ));
    }

    #[test]
    fn current_catalog_install_keeps_registry_separate() {
        let dir = tempdir().unwrap();
        let mut registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: dir.path().join("config.toml"),
            catalog: dir.path().join("models.json"),
            manifest: dir.path().join("integration.json"),
        };
        let codex_binary = test_codex_binary(dir.path());
        let manifest = build_and_install(&paths, &registry, codex_binary).unwrap();
        assert_eq!(manifest.managed_field, MANAGED_KEY);
        assert!(paths.catalog.exists());
    }

    #[test]
    fn preserves_crlf_and_does_not_patch_nested_keys() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "model_provider = 'openai'\r\n\r\n[profiles.work]\r\nmodel_catalog_json = 'user-owned'\r\n",
        )
        .unwrap();
        assert_eq!(
            read_root_string_field(&path, MANAGED_KEY)
                .unwrap()
                .as_deref(),
            None
        );
        set_root_string_field(&path, MANAGED_KEY, "/tmp/models.json").unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("model_provider = 'openai'\r\n"));
        assert!(text.contains("model_catalog_json = \"/tmp/models.json\"\r\n"));
        assert!(text.contains("[profiles.work]\r\nmodel_catalog_json = 'user-owned'\r\n"));
        assert_eq!(text.matches("\r\n").count(), 5);
    }

    #[test]
    fn repeated_sync_keeps_the_original_manifest_value() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        fs::write(&config, "model_catalog_json = '/tmp/user-catalog.json'\n").unwrap();
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());
        let first = build_and_install(&paths, &registry, &codex_binary).unwrap();
        let second = build_and_install(&paths, &registry, &codex_binary).unwrap();
        assert_eq!(
            first.original_value.as_deref(),
            Some("/tmp/user-catalog.json")
        );
        assert_eq!(
            second.original_value.as_deref(),
            Some("/tmp/user-catalog.json")
        );
    }

    #[test]
    fn restore_removes_the_catalog_recorded_in_the_manifest() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let manifest_path = dir.path().join("integration.json");
        let manifest_catalog = dir.path().join("manifest-catalog.json");
        let unrelated_catalog = dir.path().join("unrelated-catalog.json");
        fs::write(&config, "model_catalog_json = \"/tmp/models.json\"\n").unwrap();
        fs::write(&manifest_catalog, "{}\n").unwrap();
        fs::write(&unrelated_catalog, "{}\n").unwrap();
        let manifest = IntegrationManifest {
            schema_version: 1,
            config_path: config.clone(),
            managed_field: MANAGED_KEY.into(),
            applied_value: "/tmp/models.json".into(),
            original_value: None,
            original_key_present: false,
            catalog_path: manifest_catalog.clone(),
            codex_binary: PathBuf::from("codex"),
            codex_version: None,
            managed_provider: MANAGED_PROVIDER_KEY.into(),
            applied_provider: OMNIBRIDGE_PROVIDER_ID.into(),
            original_provider: None,
            original_provider_present: false,
            provider_table_present: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
        };
        save_manifest(&manifest_path, &manifest).unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: unrelated_catalog.clone(),
            manifest: manifest_path,
        };
        restore(&paths).unwrap();
        assert!(!manifest_catalog.exists());
        assert!(unrelated_catalog.exists());
        assert!(!fs::read_to_string(config).unwrap().contains(MANAGED_KEY));
    }

    #[test]
    fn restore_retries_after_config_was_restored_but_cleanup_failed() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let manifest_path = dir.path().join("integration.json");
        let catalog = dir.path().join("models.json");
        fs::write(&config, "model_provider = \"openai\"\n").unwrap();
        fs::write(&catalog, "{}\n").unwrap();
        let manifest = IntegrationManifest {
            schema_version: 1,
            config_path: config.clone(),
            managed_field: MANAGED_KEY.into(),
            applied_value: catalog.to_string_lossy().into_owned(),
            original_value: None,
            original_key_present: false,
            catalog_path: catalog.clone(),
            codex_binary: PathBuf::from("codex"),
            codex_version: None,
            managed_provider: MANAGED_PROVIDER_KEY.into(),
            applied_provider: OMNIBRIDGE_PROVIDER_ID.into(),
            original_provider: None,
            original_provider_present: false,
            provider_table_present: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
        };
        save_manifest(&manifest_path, &manifest).unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path.clone(),
        };

        restore(&paths).unwrap();
        assert!(!catalog.exists());
        assert!(!manifest_path.exists());
        assert!(!fs::read_to_string(config).unwrap().contains(MANAGED_KEY));
    }

    #[test]
    fn build_refuses_unowned_catalog_without_manifest() {
        let dir = tempdir().unwrap();
        let catalog = dir.path().join("models.json");
        fs::write(&catalog, "user-owned\n").unwrap();
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: dir.path().join("config.toml"),
            catalog: catalog.clone(),
            manifest: dir.path().join("integration.json"),
        };

        assert!(matches!(
            build_and_install(&paths, &registry, "missing-codex"),
            Err(IntegrationError::CatalogPathAlreadyExists(path)) if path == catalog
        ));
    }
}
