//! Stock Codex integration for the single OmniBridge provider.
//!
//! This module edits only the semantic Codex config, generated catalog,
//! capability file and its own manifest. It never reads or writes `auth.json`
//! or any OAuth material.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use codex_mp_catalog::{
    discover_official_catalog, merge_catalog, official_model_ids, schema_fingerprint,
    write_catalog_atomic,
};
use codex_mp_core::{
    FileLock, ProviderRegistry, command_for_executable, default_registry_path,
    set_private_permissions,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use toml_edit::{DocumentMut, Item, Table};
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
    #[error(
        "the `model_providers.{OMNIBRIDGE_PROVIDER_ID}` entry in config.toml is not the one this \
tool wrote, so it will not be overwritten. This usually means you already had a provider with \
that name, or the block was edited by hand. Rename your entry (or remove it) in config.toml \
and run `codex-mp sync` again; use `codex-mp restore` if a previous sync was interrupted"
    )]
    UserChangedProvider,
    #[error(
        "a previous sync was interrupted before it finished; run `codex-mp restore` to \
         return to your original configuration, then sync again"
    )]
    InterruptedInstall,
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
    /// True when our `[model_providers.omnibridge]` entry already existed.
    #[serde(default)]
    pub provider_table_present: bool,
    /// True when the config already had a `model_providers` table before this
    /// install. Restore may only drop that table when we created it ourselves;
    /// inferring from "is it empty afterwards" deleted a user's own (possibly
    /// comment-only) table, because Toml's table view cannot tell the two apart.
    #[serde(default)]
    pub model_providers_table_present: bool,
    #[serde(default)]
    pub capability_path: Option<PathBuf>,
    #[serde(default)]
    pub catalog_schema_fingerprint: Option<String>,
    #[serde(default)]
    pub official_model_count: usize,
    /// Two-phase-commit marker.
    ///
    /// The manifest is the only record of the values this project overwrote, so
    /// it must exist *before* `config.toml` is touched. A manifest written with
    /// `pending: true` says "an install was started but never confirmed"; it is
    /// flushed back to `false` only after the config, catalog and registry all
    /// agree. Without this, a crash between the config write and the manifest
    /// write left `model_provider = "omnibridge"` on disk with no manifest, and
    /// every guard then refused both reinstall and restore — the state was only
    /// recoverable by hand-editing `config.toml`.
    ///
    /// Absent in manifests written by earlier releases, where it is treated as
    /// `false`, matching their actual meaning.
    #[serde(default)]
    pub pending: bool,
    /// Whether `config.toml` already existed when this install started.
    ///
    /// `restore` rewrites the managed keys back to their originals, which for a
    /// fresh install means writing an empty document. That left a 0-byte
    /// `config.toml` behind where the user had none — not the undo it promises.
    /// Older manifests default to `true` (do not delete), which is the safe
    /// choice: never remove a config we are not certain we created.
    #[serde(default = "default_true")]
    pub config_existed: bool,
}

fn default_true() -> bool {
    true
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
    // One exclusive lock covers the whole install transaction. Without it a
    // concurrent `restore` (or a second `sync` from the web panel) could
    // interleave with this one, leaving `config.toml` and its manifest recording
    // different revisions — a state every guard rejects, with no CLI path out.
    let _guard = FileLock::acquire(&paths.manifest)?;
    let config_existed = paths.codex_config.exists();
    let config_content = read_config(&paths.codex_config)?;
    // Read-only view used for the guards below; mutations go through a separate
    // layout-preserving document (see `parse_config_document`).
    let config = parse_config(&config_content)?;
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
    // Inspect the layout-preserving document, not the `toml::Value`: only the
    // document can say whether the user's own `model_providers` table existed.
    let document_before = parse_config_document(&config_content)?;
    let user_had_providers_table = document_before.as_table().get("model_providers").is_some();

    let (
        original_value,
        original_key_present,
        original_provider,
        original_provider_present,
        original_providers_table_present,
    ) = if let Some(previous) = &existing_manifest {
        // A pending manifest means a previous install was interrupted: the file
        // may hold our values, the user's, or a mixture. Its `original_*` fields
        // are still trustworthy, but "does the config still match what we wrote"
        // is not, so refuse rather than guess. `restore` knows how to complete an
        // interrupted install.
        if previous.pending {
            return Err(IntegrationError::InterruptedInstall);
        }
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
            // Carry the first install's answer forward. Recomputing it here would
            // always report `true`, because our own table is present by now, and
            // restore would then leave our empty table behind for ever.
            previous.model_providers_table_present,
        )
    } else {
        if current_provider.as_deref() == Some(OMNIBRIDGE_PROVIDER_ID) || provider_table_present {
            return Err(IntegrationError::UserChangedProvider);
        }
        (
            current_value.clone(),
            current_value.is_some(),
            current_provider.clone(),
            current_provider.is_some(),
            user_had_providers_table,
        )
    };

    let official = discover_official_catalog(&codex_binary)?;
    let official_ids = official_model_ids(&official)?;
    let catalog_schema_fingerprint = Some(schema_fingerprint(&official)?);

    // Compare against the fingerprint recorded by the previous install.
    //
    // `schema_fingerprint`'s documented purpose is that "a future Codex binary
    // cannot silently consume an unreviewed shape" — but nothing ever read the
    // stored value, so a Codex upgrade that changed the catalog shape was applied
    // silently. Report the drift; the merged catalog is still produced, so this
    // is a warning rather than a failure (the user may simply have upgraded).
    if let Some(previous) = existing_manifest.as_ref()
        && let Some(previous_fingerprint) = previous.catalog_schema_fingerprint.as_deref()
        && previous_fingerprint != catalog_schema_fingerprint.as_deref().unwrap_or_default()
    {
        eprintln!(
            "codex-mp: the Codex model-catalog schema changed since the last sync \
             (fingerprint {previous_fingerprint} -> {}); \
             re-check that custom models still behave as expected",
            catalog_schema_fingerprint.as_deref().unwrap_or("<none>")
        );
    }
    let merged = merge_catalog(&official, registry)?;

    // Everything the manifest needs, computed *before* any state is mutated so
    // the intent record can be written first. `codex_version` in particular runs
    // a user-supplied binary; computing it after the config write put an
    // unbounded subprocess between "config hijacked" and "undo record exists".
    let capability_path = paths.capability_path();
    let applied_value = paths.catalog.to_string_lossy().to_string();
    let router_base_url = paths.router_base_url();
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
        model_providers_table_present: original_providers_table_present,
        capability_path: Some(capability_path.clone()),
        catalog_schema_fingerprint,
        official_model_count: official_ids.len(),
        pending: true,
        config_existed,
    };

    // Phase 1: record the intent (and the original values) durably.
    save_manifest(&paths.manifest, &manifest)?;

    // Phase 2: mutate, rolling back to the recorded originals on any failure.
    let applied = (|| -> Result<(), IntegrationError> {
        let capability = ensure_capability(&capability_path)?;
        write_catalog_atomic(&merged, &paths.catalog)?;
        // Mutate a layout-preserving document so user comments and unrelated
        // tables survive the sync byte-for-byte.
        let mut document = document_before;
        apply_omnibridge_config_document(
            &mut document,
            &manifest.applied_value,
            &router_base_url,
            &capability,
        )?;
        let updated = document_with_original_line_endings(&document, &config_content);
        write_config(&paths.codex_config, &updated)?;

        let mut effective_registry = registry.clone();
        effective_registry.set_official_model_ids(official_ids);
        effective_registry.save()?;
        Ok(())
    })();

    if let Err(error) = applied {
        let config_rollback =
            restore_config_snapshot(&paths.codex_config, &config_content, config_existed);
        let catalog_rollback =
            restore_catalog_snapshot(&paths.catalog, previous_catalog.as_deref());
        // The intent record must go too, otherwise a failed sync leaves a
        // "pending" manifest behind and blocks the next attempt.
        let manifest_rollback = fs::remove_file(&paths.manifest);
        if config_rollback.is_err() || catalog_rollback.is_err() {
            return Err(IntegrationError::InvalidManifest(format!(
                "sync failed ({error}) and rollback was incomplete"
            )));
        }
        if let Err(rollback_error) = manifest_rollback
            && rollback_error.kind() != std::io::ErrorKind::NotFound
        {
            return Err(IntegrationError::InvalidManifest(format!(
                "sync failed ({error}) and the intent record could not be removed: {rollback_error}"
            )));
        }
        return Err(error);
    }

    // Phase 3: confirm. Until this lands the manifest reads as "in progress",
    // which is what lets a later run recover instead of dead-ending.
    let confirmed = IntegrationManifest {
        pending: false,
        ..manifest
    };
    save_manifest(&paths.manifest, &confirmed)?;
    Ok(confirmed)
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

/// Parse the same text into a layout-preserving document.
///
/// `toml::Value` has no representation for comments or whitespace, so writing the
/// config back out through it rewrote the user's entire file and silently dropped
/// every comment, blank line and alignment — including in tables this project
/// does not manage. All mutations therefore go through `toml_edit`; the guard
/// logic above keeps using the plain value, which is easier to reason about.
fn parse_config_document(content: &str) -> Result<DocumentMut, IntegrationError> {
    if content.trim().is_empty() {
        return Ok(DocumentMut::new());
    }
    content.parse::<DocumentMut>().map_err(|error| {
        IntegrationError::InvalidManifest(format!("config.toml is not valid TOML: {error}"))
    })
}

/// Serialize an edited document back using the line ending the file already had.
///
/// `toml_edit` normalises every line ending to LF when it serializes, so a CRLF
/// config (what Notepad and PowerShell write on Windows, and what this project
/// explicitly promises to preserve — see `docs/codex-integration.md`) came back
/// as LF after every sync. That is a whole-file rewrite the user never asked for.
///
/// Returns the document text with `original`'s single dominant line ending
/// restored. A file with no CRLF keeps plain LF, which is the existing behaviour.
fn document_with_original_line_endings(document: &DocumentMut, original: &str) -> String {
    let rendered = document.to_string();
    let crlf = original.matches("\r\n").count();
    let lf = original.matches('\n').count().saturating_sub(crlf);
    if crlf == 0 || lf > crlf {
        return rendered;
    }
    // The file was CRLF-dominant: normalise the rendered LF output back to CRLF.
    rendered.replace("\r\n", "\n").replace('\n', "\r\n")
}

/// Set a root-level string without disturbing the key's existing formatting.
///
/// Assigning through `document[key] = toml_edit::value(..)` replaces the whole
/// `Item`, and both the leading whitespace and the trailing comment of a line
/// live in the *value's* decoration. Overwriting it therefore deleted e.g. the
/// `# keep this note` in `model_provider = "openai" # keep this note`. Keeping
/// the existing decor and swapping only the value preserves the line verbatim.
fn set_root_string(document: &mut DocumentMut, key: &str, value: &str) {
    match document.as_table_mut().get_mut(key) {
        Some(item) => match item.as_value_mut() {
            Some(existing) => {
                let decor = existing.decor().clone();
                let mut replacement = toml_edit::Value::from(value);
                *replacement.decor_mut() = decor;
                *existing = replacement;
            }
            // The key exists but holds a table or array: replacing it wholesale is
            // the only correct outcome, and there is no scalar line to preserve.
            None => *item = toml_edit::Item::Value(toml_edit::Value::from(value)),
        },
        None => {
            document[key] = toml_edit::value(value);
        }
    }
}

fn remove_root_key(document: &mut DocumentMut, key: &str) {
    document.as_table_mut().remove(key);
}

/// Write the OmniBridge provider configuration into `document`, touching only
/// `model_provider`, `model_catalog_json` and `[model_providers.omnibridge]`.
fn apply_omnibridge_config_document(
    document: &mut DocumentMut,
    catalog_path: &str,
    base_url: &str,
    capability: &str,
) -> Result<(), IntegrationError> {
    set_root_string(document, MANAGED_PROVIDER_KEY, OMNIBRIDGE_PROVIDER_ID);
    set_root_string(document, MANAGED_KEY, catalog_path);

    // Rebuilt from scratch so a field written by an older release cannot survive.
    let mut bridge = Table::new();
    bridge["name"] = toml_edit::value("OpenAI");
    bridge["base_url"] = toml_edit::value(base_url);
    bridge["wire_api"] = toml_edit::value("responses");
    bridge["requires_openai_auth"] = toml_edit::value(true);
    bridge["supports_websockets"] = toml_edit::value(false);
    let mut headers = Table::new();
    headers["x-codex-omnibridge-token"] = toml_edit::value(capability);
    bridge["http_headers"] = Item::Table(headers);

    let root = document.as_table_mut();
    if !root.contains_key("model_providers") {
        root.insert("model_providers", Item::Table(Table::new()));
    }
    let providers = root
        .get_mut("model_providers")
        .and_then(Item::as_table_mut)
        .ok_or_else(|| {
            IntegrationError::InvalidManifest("model_providers must be a table".into())
        })?;
    providers.insert(OMNIBRIDGE_PROVIDER_ID, Item::Table(bridge));
    Ok(())
}

/// Undo everything `apply_omnibridge_config_document` wrote.
///
/// `table_was_ours` must be false when the user's config already had a
/// `model_providers` table before we installed. The previous implementation
/// removed the table whenever it looked empty after dropping our entry, which
/// deleted a user's own table — including one holding only comments, which
/// `toml::Value` cannot see at all.
fn restore_config_document(
    document: &mut DocumentMut,
    managed_field: &str,
    original_value: Option<&str>,
    managed_provider: &str,
    original_provider: Option<&str>,
    table_was_ours: bool,
) {
    match original_value {
        Some(value) => set_root_string(document, managed_field, value),
        None => remove_root_key(document, managed_field),
    }
    match original_provider {
        Some(value) => set_root_string(document, managed_provider, value),
        None => remove_root_key(document, managed_provider),
    }

    let providers_became_empty = {
        let root = document.as_table_mut();
        match root.get_mut("model_providers") {
            Some(providers) => match providers {
                Item::Table(table) => {
                    table.remove(OMNIBRIDGE_PROVIDER_ID);
                    table.is_empty()
                }
                // `model_providers = { omnibridge = { .. } }` is a legal
                // spelling of the same thing. `Item::as_table_mut` returns
                // `None` for an inline table, so the old code silently removed
                // nothing while `has_omnibridge_provider` (which goes through
                // `toml::Value`) still reported success — leaving the provider
                // in the user's config with the manifest already deleted.
                Item::Value(value) => match value.as_inline_table_mut() {
                    Some(inline) => {
                        inline.remove(OMNIBRIDGE_PROVIDER_ID);
                        inline.is_empty()
                    }
                    None => false,
                },
                _ => false,
            },
            None => false,
        }
    };
    if providers_became_empty && table_was_ours {
        remove_root_key(document, "model_providers");
    }
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
    // This file *is* the Router capability token, which authorises every Router
    // endpoint. `fs::write` applies the umask (typically 0664), so the token was
    // group/other-readable until the chmod below ran. Created 0600 instead.
    codex_mp_core::write_private_atomic(path, format!("{value}\n").as_bytes())?;
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

/// Reject a manifest whose recorded paths do not match the paths this process
/// was told to manage, or whose cleanup targets escape our own config directory.
///
/// The manifest lives in a user-writable directory and is also driven by the web
/// panel, so it must be treated as untrusted input. `restore` deletes
/// `catalog_path` and `capability_path` and rewrites `config_path` from these
/// fields; without this check a hand-edited or foreign manifest turned
/// "uninstall" into arbitrary file deletion (e.g. `capability_path` pointing at
/// `~/.ssh/id_rsa`). `build_and_install` already validated `config_path` and
/// `catalog_path`; restore did not.
fn validate_manifest_paths(
    manifest: &IntegrationManifest,
    paths: &IntegrationPaths,
) -> Result<(), IntegrationError> {
    if manifest.config_path != paths.codex_config {
        return Err(IntegrationError::InvalidConfigPath(
            manifest.config_path.clone(),
        ));
    }
    if manifest.catalog_path != paths.catalog {
        return Err(IntegrationError::InvalidManifest(
            "catalog path changed outside the integration manifest".into(),
        ));
    }
    if let Some(capability) = manifest.capability_path.as_ref()
        && capability != &paths.capability_path()
    {
        return Err(IntegrationError::InvalidManifest(
            "capability path changed outside the integration manifest".into(),
        ));
    }
    Ok(())
}

/// Port the managed config advertises for the Router, for the given registry.
///
/// Stock Codex dials the `base_url` recorded in `config.toml`; it never reads the
/// Router's endpoint file. Anything that starts a Router for Codex to talk to must
/// therefore bind *this* port, or every request is sent to a port nobody owns.
/// When the managed config is absent or unreadable, use the configured Router
/// base URL (8787 by default) so a first panel start remains reachable by the
/// Codex integration instead of choosing an undiscoverable random port.
pub fn router_port_for_registry(registry_path: impl AsRef<Path>) -> u16 {
    let paths = IntegrationPaths::for_registry(registry_path);
    let fallback_port = port_from_base_url(&paths.router_base_url()).unwrap_or(8787);
    let Ok(content) = fs::read_to_string(&paths.codex_config) else {
        return fallback_port;
    };
    let Ok(document) = content.parse::<toml_edit::DocumentMut>() else {
        return fallback_port;
    };
    let base_url = document
        .get("model_providers")
        .and_then(|providers| providers.get(OMNIBRIDGE_PROVIDER_ID))
        .and_then(|bridge| bridge.get("base_url"))
        .and_then(|value| value.as_str());
    base_url
        .and_then(port_from_base_url)
        .unwrap_or(fallback_port)
}

/// Extract a port from a `base_url` such as `http://127.0.0.1:8787/v1`.
fn port_from_base_url(base_url: &str) -> Option<u16> {
    let parsed = url::Url::parse(base_url).ok()?;
    parsed.port().or_else(|| match parsed.scheme() {
        // No explicit port: use the scheme default so we still bind something
        // coherent instead of silently choosing a random port.
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    })
}

pub fn restore(paths: &IntegrationPaths) -> Result<(), IntegrationError> {
    // Serialized against `build_and_install`, for the same reason.
    let _guard = FileLock::acquire(&paths.manifest)?;

    // A manifest this project wrote but can no longer parse must not trap the
    // user. It is *our* record, not the user's content: if it is unreadable we
    // cannot undo the managed config keys safely, but we can still stop claiming
    // to manage an installation, remove our generated catalog, and drop the
    // record — then tell the user exactly what to fix.
    //
    // This is the third instance of the same trap: N-65 (`config.toml`),
    // N-87 (`providers.json`) and now the manifest. Each previously made
    // `restore`/`uninstall` fail outright, leaving a hijacked Codex config and no
    // CLI way out.
    let manifest = match load_manifest(&paths.manifest) {
        Ok(manifest) => manifest,
        Err(error) => {
            let catalog = paths.catalog.clone();
            let _ = fs::remove_file(&catalog);
            let _ = fs::remove_file(&paths.manifest);
            // The inner error already names the manifest path and the remedy, so
            // this only adds the part that is specific to `restore`: what was
            // cleaned up and what the user still has to do by hand.
            return Err(IntegrationError::InvalidManifest(format!(
                "the managed keys in config.toml could not be rewritten. This \
project's manifest and generated catalog were removed; check config.toml for a \
`model_provider` or `model_catalog_json` entry that points at this tool and \
remove it, then re-run `codex-mp sync` if you want it back. ({error})"
            )));
        }
    };
    validate_manifest_paths(&manifest, paths)?;
    if manifest.schema_version >= 2 {
        let content = read_config(&manifest.config_path)?;
        // A config.toml the user broke (or an interrupted write left) must not
        // trap them: `restore`, `repair` and `uninstall` all stop here, so the
        // only escape was deleting files by hand. Fall back to "cannot rewrite
        // the user's TOML" while still removing everything this project created,
        // and say exactly what to fix.
        let config = match parse_config(&content) {
            Ok(config) => config,
            Err(_) => {
                let config_path = manifest.config_path.display().to_string();
                match fs::remove_file(&manifest.catalog_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
                fs::remove_file(&paths.manifest)?;
                return Err(IntegrationError::InvalidManifest(format!(
                    "{} is not valid TOML, so the managed keys could not be rewritten. \
                     This project's records and generated catalog were removed; fix the \
                     syntax in that file (the parser error is shown above) and re-run \
                     `codex-mp sync`.",
                    config_path
                )));
            }
        };

        // "Already restored" has to be decided per field and against what the
        // manifest recorded for *that* field. The previous version required the
        // catalog key to have existed originally before it would even look at the
        // provider, and it compared the provider against `original_provider`
        // without consulting `original_provider_present`. A config that was
        // already back to its original values could therefore still be reported as
        // `UserChangedProvider` forever, with no way out.
        let field_restored =
            |current: Option<&str>, originally_present: bool, original: Option<&str>| match (
                originally_present,
                original,
            ) {
                (true, Some(value)) => current == Some(value),
                // The key was absent before we installed, so restored means absent now.
                _ => current.is_none(),
            };
        let already_restored = field_restored(
            root_string(&config, &manifest.managed_field).as_deref(),
            manifest.original_key_present,
            manifest.original_value.as_deref(),
        ) && field_restored(
            root_string(&config, &manifest.managed_provider).as_deref(),
            manifest.original_provider_present,
            manifest.original_provider.as_deref(),
        );

        if !already_restored {
            // A `pending` manifest records an install that was interrupted before
            // it was confirmed. Some of our values may already be on disk and some
            // may not, so the strict equality guard below cannot be satisfied —
            // yet the leftover state is ours and must still be cleaned up. Verify
            // only that we are not about to overwrite something the user changed
            // by hand: at least one of our markers must still be present.
            if manifest.pending {
                let our_catalog_field = root_string(&config, &manifest.managed_field).as_deref()
                    == Some(manifest.applied_value.as_str());
                let our_provider = root_string(&config, &manifest.managed_provider).as_deref()
                    == Some(manifest.applied_provider.as_str())
                    || has_omnibridge_provider(&config);
                if !our_catalog_field && !our_provider {
                    // Nothing of ours is on disk. The install died before it
                    // changed anything, so just drop the record and report clean.
                    fs::remove_file(&paths.manifest)?;
                    return Ok(());
                }
            } else if root_string(&config, &manifest.managed_field).as_deref()
                != Some(manifest.applied_value.as_str())
                || root_string(&config, &manifest.managed_provider).as_deref()
                    != Some(manifest.applied_provider.as_str())
                || !has_omnibridge_provider(&config)
            {
                return Err(IntegrationError::UserChangedProvider);
            }
            let mut document = parse_config_document(&content)?;
            restore_config_document(
                &mut document,
                &manifest.managed_field,
                manifest.original_value.as_deref(),
                &manifest.managed_provider,
                manifest.original_provider.as_deref(),
                !manifest.model_providers_table_present,
            );
            let restored = document_with_original_line_endings(&document, &content);
            // A fresh install has no original keys, so restoring writes an empty
            // document. If we created `config.toml` ourselves, remove it instead
            // of leaving a 0-byte file behind — `restore` must undo the install,
            // not replace "no file" with "an empty file".
            if manifest.config_existed || !restored.trim().is_empty() {
                write_config(&manifest.config_path, &restored)?;
            } else {
                match fs::remove_file(&manifest.config_path) {
                    Ok(()) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
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
    let path = path.as_ref();
    let content = fs::read_to_string(path)?;
    // Report the file and a remedy rather than a bare parser message. This
    // manifest is our own record; when it is unreadable, `sync`, `repair` and
    // `restore` all stop here, so a message without the path left the user with
    // no idea which file to fix. `restore` additionally recovers (see below);
    // `sync`/`repair` at least say what to do.
    let manifest: IntegrationManifest = serde_json::from_str(&content).map_err(|error| {
        IntegrationError::InvalidManifest(format!(
            "the integration manifest at {} is not valid JSON ({error}); delete that \
file (or run `codex-mp uninstall`) to recover, then re-run `codex-mp sync`",
            path.display()
        ))
    })?;
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
    let mut bytes = serde_json::to_vec_pretty(manifest)?;
    bytes.push(b'\n');
    // The manifest records the config path and applied values; created 0600 so it
    // is never transiently readable by other local users.
    codex_mp_core::write_private_atomic(path, &bytes)?;
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
    codex_mp_core::write_private_atomic(path, bytes)?;
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
    // `config.toml` carries the Router capability token, so it is created 0600
    // rather than being umask-readable until a later chmod.
    codex_mp_core::write_private_atomic(path, content.as_bytes())?;
    Ok(())
}

/// Read the Codex binary's version, bounded in time.
///
/// `Command::output()` waits for the child with no deadline, so a wrapper script
/// or a malfunctioning install that never exits made `codex-mp sync` hang for
/// ever. The version is metadata for the manifest, never a reason to block, so a
/// timeout degrades to `None`.
fn codex_version(path: &Path) -> Option<String> {
    const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
    let mut command = command_for_executable(path);
    command.arg("--version");
    let output = codex_mp_core::run_with_timeout(&mut command, VERSION_TIMEOUT).ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_core::{ProviderConfig, ProviderRegistry};
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn test_codex_binary(dir: &Path) -> PathBuf {
        // The fixture must satisfy Codex's real catalog schema, which
        // `validate_catalog` now enforces: `shell_type` a string,
        // `supported_reasoning_levels` an array, `supports_search_tool` a bool,
        // `experimental_supported_tools` an array, and at least one of
        // `base_instructions` / `model_messages`.
        let catalog = r#"{"models":[{"slug":"gpt-5.6-sol","display_name":"GPT-5.6-Sol","shell_type":"unified_exec","model_messages":{"persistent_instructions":"safe","instructions_template":"safe"},"supported_reasoning_levels":[{"effort":"low","description":"low"}],"supports_search_tool":false,"experimental_supported_tools":[] }]}"#;

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
            model_providers_table_present: false,
            pending: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
            config_existed: true,
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

    /// Regression: `restore` rewrote the managed keys back to their originals.
    /// For a fresh install (no original keys) that means writing an empty
    /// document, so a user who had **no** `config.toml` was left with a 0-byte
    /// one — `restore` promised to undo the install but instead created a file.
    /// It must remove a config it created, and must still preserve one that
    /// already existed (including a deliberately empty one).
    #[test]
    fn restore_removes_a_config_it_created_but_keeps_a_preexisting_one() {
        // Case A: no config.toml before the install.
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());
        assert!(!config.exists(), "precondition: no config.toml yet");

        build_and_install(&paths, &registry, &codex_binary).unwrap();
        assert!(config.exists(), "sync must create the config");
        restore(&paths).unwrap();
        assert!(
            !config.exists(),
            "restore must remove a config.toml that sync created"
        );

        // Case B: a pre-existing config must be preserved byte-for-byte.
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        let original = "# my settings\nmodel = \"gpt-5.6-sol\"\n";
        fs::write(&config, original).unwrap();
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());

        build_and_install(&paths, &registry, &codex_binary).unwrap();
        restore(&paths).unwrap();
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            original,
            "a pre-existing config must survive the round trip unchanged"
        );

        // Case C: a pre-existing *empty* config is still a pre-existing file.
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        fs::write(&config, "").unwrap();
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());

        build_and_install(&paths, &registry, &codex_binary).unwrap();
        restore(&paths).unwrap();
        assert!(
            config.exists(),
            "an empty config.toml that already existed must not be deleted"
        );
    }

    /// Regression: an unparseable `config.toml` (a hand-edit that broke the
    /// syntax, or a torn write) trapped the user. `restore`, `repair` and
    /// `uninstall` all parsed the config first and returned an error, so the
    /// **only** escape was deleting files by hand — while the manifest kept
    /// claiming Codex was still hijacked.
    ///
    /// `restore` must instead drop this project's records and generated catalog,
    /// leave the user's own file untouched, and explain what to fix.
    /// Regression: the manifest is this project's *own* record, but a corrupted
    /// one made `restore`/`uninstall` fail outright — leaving the Codex config
    /// still hijacked with no CLI way out. That is the third instance of the same
    /// trap (N-65 for `config.toml`, N-87 for `providers.json`).
    ///
    /// An unreadable manifest must instead stop claiming the installation, remove
    /// the generated catalog and the record, and say what to fix.
    /// Regression: `sync` and `repair` loaded the manifest with a bare `?`, so a
    /// corrupted record surfaced only the parser message
    /// (`control character ... at line 2 column 0`) — no file name, no remedy.
    /// `restore` recovers (see the test above), but the other two commands just
    /// failed, leaving the user to guess which file to fix.
    #[test]
    fn an_unreadable_manifest_error_names_the_file_and_the_remedy() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("integration.json");
        fs::write(&manifest_path, b"{\"broken").unwrap();

        let error = load_manifest(&manifest_path)
            .expect_err("an unreadable manifest must be reported")
            .to_string();
        assert!(
            error.contains("integration.json"),
            "the error must name the file, got: {error}"
        );
        assert!(
            error.contains("codex-mp sync"),
            "the error must state how to recover, got: {error}"
        );
        assert!(
            error.contains("not valid JSON"),
            "the error must say what is wrong, got: {error}"
        );
    }

    #[test]
    fn restore_recovers_from_an_unparseable_manifest() {
        let dir = tempdir().unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: dir.path().join("config.toml"),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());
        build_and_install(&paths, &registry, &codex_binary).unwrap();
        assert!(paths.manifest.exists(), "precondition: manifest exists");
        assert!(paths.catalog.exists(), "precondition: catalog exists");

        // The manifest — this project's own record — is corrupted.
        fs::write(&paths.manifest, b"{\"broken").unwrap();

        let error = restore(&paths)
            .expect_err("an unreadable manifest must be reported")
            .to_string();
        assert!(
            error.contains("integration.json"),
            "the error must name the manifest, got: {error}"
        );
        assert!(
            error.contains("codex-mp sync"),
            "the error must state the recovery step, got: {error}"
        );
        assert!(
            !paths.manifest.exists(),
            "the unreadable manifest must be removed so the next run is not trapped"
        );
        assert!(
            !paths.catalog.exists(),
            "the generated catalog must be removed too"
        );

        // The follow-up run must now succeed rather than fail the same way.
        assert!(
            !restore_if_present(&paths).unwrap_or(false),
            "with the record gone there is nothing left to restore"
        );
    }

    #[test]
    fn restore_recovers_from_an_unparseable_config_keeps_user_file() {
        let dir = tempdir().unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: dir.path().join("config.toml"),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());
        build_and_install(&paths, &registry, &codex_binary).unwrap();
        assert!(paths.manifest.exists(), "precondition: manifest exists");
        assert!(paths.catalog.exists(), "precondition: catalog exists");

        // The manifest — this project's own record — is corrupted.
        fs::write(&paths.manifest, b"{\"broken").unwrap();

        let error = restore(&paths)
            .expect_err("an unreadable manifest must be reported")
            .to_string();
        assert!(
            error.contains("integration.json"),
            "the error must name the manifest, got: {error}"
        );
        assert!(
            error.contains("codex-mp sync"),
            "the error must state the recovery step, got: {error}"
        );
        assert!(
            !paths.manifest.exists(),
            "the unreadable manifest must be removed so the next run is not trapped"
        );
        assert!(
            !paths.catalog.exists(),
            "the generated catalog must be removed too"
        );

        // The follow-up run must now succeed rather than fail the same way.
        assert!(
            !restore_if_present(&paths).unwrap_or(false),
            "with the record gone there is nothing left to restore"
        );
    }

    #[test]
    fn restore_recovers_from_an_unparseable_config() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());
        build_and_install(&paths, &registry, &codex_binary).unwrap();

        // The user breaks the syntax.
        fs::write(&config, "garbage = \n").unwrap();
        assert!(paths.manifest.exists(), "precondition: manifest exists");
        assert!(paths.catalog.exists(), "precondition: catalog exists");

        let error = restore(&paths).expect_err("restore must report the bad config");
        let message = error.to_string();
        assert!(
            message.contains("not valid TOML"),
            "the error must name the problem, got: {message}"
        );
        assert!(
            message.contains("codex-mp sync"),
            "the error must say how to recover, got: {message}"
        );

        // Our records are gone, so the user is no longer trapped...
        assert!(!paths.manifest.exists(), "the manifest must be cleared");
        assert!(
            !paths.catalog.exists(),
            "the generated catalog must be removed"
        );
        // ...but the user's own file is left exactly as they left it.
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "garbage = \n",
            "restore must not modify a file it could not parse"
        );
    }

    /// Regression: `toml_edit` normalises every line ending to LF when it
    /// serializes. A CRLF `config.toml` (what Notepad and PowerShell write on
    /// Windows) therefore came back as LF after every `sync`, contradicting the
    /// LF/CRLF preservation this project documents — and rewriting the whole file
    /// for a user who only wanted one key changed. The round trip must be
    /// byte-identical.
    #[test]
    fn sync_and_restore_preserve_crlf_line_endings() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        let original = "# my comment\r\nmodel_provider = \"openai\"\r\n\r\n[profiles.work]\r\nmodel = \"sol\"\r\n";
        fs::write(&config, original).unwrap();

        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());
        build_and_install(&paths, &registry, &codex_binary).unwrap();

        let synced = fs::read_to_string(&config).unwrap();
        assert!(
            synced.contains("model_provider = \"omnibridge\"\r\n"),
            "the managed key must be written with CRLF:\n{synced:?}"
        );
        // Every line must still end CRLF; no lone LF may have crept in.
        let lone_lf = synced
            .match_indices('\n')
            .filter(|(index, _)| *index == 0 || synced.as_bytes()[index - 1] != b'\r')
            .count();
        assert_eq!(lone_lf, 0, "sync introduced LF-only lines:\n{synced:?}");

        restore(&paths).unwrap();
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            original,
            "restore must reproduce the original bytes, including CRLF"
        );
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
            model_providers_table_present: false,
            pending: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
            config_existed: true,
        };
        save_manifest(&manifest_path, &manifest).unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: manifest_catalog.clone(),
            manifest: manifest_path,
        };
        restore(&paths).unwrap();
        assert!(!manifest_catalog.exists());
        assert!(unrelated_catalog.exists());
        assert!(!fs::read_to_string(config).unwrap().contains(MANAGED_KEY));
    }

    /// The manifest is untrusted input: it lives in a user-writable directory and
    /// the web panel drives these operations. A tampered `capability_path` used to
    /// be deleted unconditionally, so "uninstall" could remove any file the user
    /// could write (e.g. an SSH key). Paths that do not match what this process
    /// was asked to manage must be refused outright.
    #[test]
    fn restore_refuses_a_manifest_that_points_outside_our_paths() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let catalog = dir.path().join("models.json");
        let manifest_path = dir.path().join("integration.json");
        let victim = dir.path().join("victim.txt");

        fs::write(&config, "model_provider = \"omnibridge\"\n").unwrap();
        fs::write(&catalog, "{}\n").unwrap();
        fs::write(&victim, "do not delete me\n").unwrap();

        let manifest = IntegrationManifest {
            capability_path: Some(victim.clone()),
            ..pending_manifest(
                &config,
                &catalog,
                "/tmp/omnibridge-models.json",
                Some("openai"),
            )
        };
        save_manifest(&manifest_path, &manifest).unwrap();

        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path.clone(),
        };

        assert!(matches!(
            restore(&paths),
            Err(IntegrationError::InvalidManifest(_))
        ));
        assert!(victim.exists(), "restore deleted a file it does not own");
        assert!(catalog.exists(), "nothing should be cleaned up on refusal");
    }

    /// The same guard must cover the config path, which restore rewrites.
    #[test]
    fn restore_refuses_a_manifest_targeting_another_config_file() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let other = dir.path().join("other.toml");
        let catalog = dir.path().join("models.json");
        let manifest_path = dir.path().join("integration.json");

        fs::write(&config, "model_provider = \"openai\"\n").unwrap();
        let mut tampered = pending_manifest(
            &other,
            &catalog,
            "/tmp/omnibridge-models.json",
            Some("openai"),
        );
        tampered.pending = false;
        save_manifest(&manifest_path, &tampered).unwrap();

        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path,
        };

        assert!(matches!(
            restore(&paths),
            Err(IntegrationError::InvalidConfigPath(_))
        ));
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
            model_providers_table_present: false,
            pending: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
            config_existed: true,
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

    /// Regression: a user who already had a provider named `omnibridge` got only
    /// `managed Codex provider configuration changed outside Codex MultiProvider`
    /// — no file, no cause, no remedy. The refusal itself is correct (the tool
    /// must not clobber config it did not write), but the user was left to guess.
    ///
    /// The message must name the table, and the user's entry must survive intact.
    #[test]
    fn a_provider_name_collision_explains_itself_and_preserves_the_user_entry() {
        let dir = tempdir().unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: dir.path().join("config.toml"),
            catalog: dir.path().join("generated.json"),
            manifest: dir.path().join("integration.json"),
        };
        // The user already owns a provider with our reserved name.
        fs::write(
            &paths.codex_config,
            "model = \"gpt-5.5\"\n\n[model_providers.omnibridge]\n\
name = \"My Own Provider\"\nbase_url = \"https://my-own.example/v1\"\nwire_api = \"chat\"\n",
        )
        .unwrap();
        let registry = ProviderRegistry::empty(dir.path().join("providers.json"));
        let codex_binary = test_codex_binary(dir.path());

        let error = build_and_install(&paths, &registry, &codex_binary)
            .expect_err("a name collision must not be overwritten")
            .to_string();
        assert!(
            error.contains("model_providers.omnibridge"),
            "the error must name the conflicting table, got: {error}"
        );
        assert!(
            error.contains("Rename") && error.contains("codex-mp sync"),
            "the error must state the remedy, got: {error}"
        );

        // The user's own provider is untouched, byte for byte.
        let after = fs::read_to_string(&paths.codex_config).unwrap();
        assert!(after.contains("My Own Provider"));
        assert!(after.contains("https://my-own.example/v1"));
        assert!(after.contains("wire_api = \"chat\""));
        assert!(
            !after.contains(MANAGED_KEY),
            "nothing of ours may be written when we refuse"
        );
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

    /// Regression: syncing used to re-serialize the whole config through
    /// `toml::Value`, which cannot represent comments or layout. Every comment,
    /// blank line and aligned table in the user's file was silently deleted, in
    /// sections this project does not even manage.
    #[test]
    fn document_edit_preserves_comments_and_unmanaged_tables() {
        let original = concat!(
            "# my precious comment\n",
            "model_provider = \"openai\" # trailing note\n",
            "\n",
            "[profiles.work]\n",
            "model = \"gpt-5.6-sol\"\n",
            "\n",
            "[mcp_servers.local]\n",
            "command = \"node\"\n",
        );

        let mut document = parse_config_document(original).unwrap();
        apply_omnibridge_config_document(
            &mut document,
            "/tmp/models.json",
            "http://127.0.0.1:8787/v1",
            "capability-token",
        )
        .unwrap();
        let updated = document.to_string();

        for preserved in [
            "# my precious comment",
            "# trailing note",
            "[profiles.work]",
            "[mcp_servers.local]",
            "command = \"node\"",
            "model = \"gpt-5.6-sol\"",
        ] {
            assert!(
                updated.contains(preserved),
                "sync destroyed {preserved:?}:\n{updated}"
            );
        }
        assert!(updated.contains("model_provider = \"omnibridge\""));
        assert!(updated.contains("model_catalog_json = \"/tmp/models.json\""));
        assert!(updated.contains("[model_providers.omnibridge]"));
        assert!(updated.contains("x-codex-omnibridge-token = \"capability-token\""));
    }

    #[test]
    fn document_restore_keeps_comments_and_removes_only_our_provider() {
        let original = concat!(
            "# keep me\n",
            "model_provider = \"openai\"\n",
            "\n",
            "[mcp_servers.x]\n",
            "command = \"node\"\n",
        );

        let mut document = parse_config_document(original).unwrap();
        apply_omnibridge_config_document(
            &mut document,
            "/tmp/models.json",
            "http://127.0.0.1:8787/v1",
            "capability-token",
        )
        .unwrap();
        restore_config_document(
            &mut document,
            MANAGED_KEY,
            None,
            MANAGED_PROVIDER_KEY,
            Some("openai"),
            // This fixture's config has no `model_providers` table of its own.
            true,
        );
        let restored = document.to_string();

        assert!(
            restored.contains("# keep me"),
            "lost a comment:\n{restored}"
        );
        assert!(restored.contains("model_provider = \"openai\""));
        assert!(restored.contains("[mcp_servers.x]"));
        assert!(
            !restored.contains(OMNIBRIDGE_PROVIDER_ID),
            "restore left our provider behind:\n{restored}"
        );
        assert!(
            !restored.contains(MANAGED_KEY),
            "restore left the catalog field behind:\n{restored}"
        );
    }

    /// Regression: restore used to drop the `model_providers` table whenever it
    /// looked empty after removing our entry, which destroyed a table the user
    /// owned. A comment-only table is the sharpest case: `toml::Value` cannot see
    /// the comments at all, so the table reads as empty and vanishes.
    #[test]
    fn restore_keeps_a_user_owned_providers_table() {
        let original = concat!(
            "# providers I care about\n",
            "[model_providers]\n",
            "\n",
            "# a note about my own provider, added later\n",
        );

        let mut document = parse_config_document(original).unwrap();
        apply_omnibridge_config_document(
            &mut document,
            "/tmp/models.json",
            "http://127.0.0.1:8787/v1",
            "capability-token",
        )
        .unwrap();
        // The user's table already existed, so it is not ours to delete.
        restore_config_document(
            &mut document,
            MANAGED_KEY,
            None,
            MANAGED_PROVIDER_KEY,
            None,
            false,
        );
        let restored = document.to_string();

        assert!(
            restored.contains("# providers I care about"),
            "restore deleted a user-owned model_providers table:\n{restored:?}"
        );
        assert!(
            !restored.contains(OMNIBRIDGE_PROVIDER_ID),
            "restore left our provider behind:\n{restored}"
        );
    }

    /// Regression: an inline `model_providers = { .. }` table was skipped by
    /// `Item::as_table_mut`, so restore reported success, deleted its own
    /// manifest, and left the OmniBridge provider in the user's config with
    /// nothing left to track it.
    #[test]
    fn restore_removes_our_provider_from_an_inline_table() {
        let original = "model_provider = \"openai\"\n";
        let mut document = parse_config_document(original).unwrap();
        // Write the provider as an inline table, as a hand-edited config may.
        document["model_providers"] = toml_edit::Item::Value(toml_edit::Value::InlineTable({
            let mut table = toml_edit::InlineTable::new();
            table.insert(
                OMNIBRIDGE_PROVIDER_ID,
                toml_edit::Value::InlineTable(toml_edit::InlineTable::new()),
            );
            table
        }));

        restore_config_document(
            &mut document,
            MANAGED_KEY,
            None,
            MANAGED_PROVIDER_KEY,
            Some("openai"),
            true,
        );
        let restored = document.to_string();

        assert!(
            !restored.contains(OMNIBRIDGE_PROVIDER_ID),
            "restore did not remove the provider from an inline table:\n{restored}"
        );
        assert!(restored.contains("model_provider = \"openai\""));
    }

    /// Regression: stock Codex dials the `base_url` from `config.toml` and never
    /// reads the Router's endpoint file, so anything launching Codex must bind the
    /// port the config advertises. `launch` used `--port 0` (ephemeral), which sent
    /// every request to a port nobody owned.
    #[test]
    fn router_port_is_read_from_the_managed_config() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let registry = dir.path().join("providers.json");
        fs::write(
            &config,
            "model_provider = \"omnibridge\"\n             [model_providers.omnibridge]\n             name = \"OpenAI\"\n             base_url = \"http://127.0.0.1:8787/v1\"\n",
        )
        .unwrap();
        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: dir.path().join("models.json"),
            manifest: dir.path().join("integration.json"),
        };
        // `for_registry` derives from the registry path; our fixture writes the
        // config directly, so point the derivation at it explicitly.
        assert_eq!(paths.codex_config, config);
        assert_eq!(router_port_for_registry(&registry), 8787);
    }

    /// A missing or malformed config must use the same configured/default port
    /// as `sync`, so the first panel start is reachable before the first `sync`.
    #[test]
    fn router_port_degrades_when_the_config_is_unusable() {
        let dir = tempdir().unwrap();
        let registry = dir.path().join("providers.json");
        assert_eq!(router_port_for_registry(&registry), 8787);
    }

    /// The port must be parsed out of an explicit base_url, including the scheme
    /// default when no port is written.
    #[test]
    fn port_from_base_url_handles_explicit_and_default_ports() {
        assert_eq!(port_from_base_url("http://127.0.0.1:8787/v1"), Some(8787));
        assert_eq!(port_from_base_url("http://127.0.0.1/v1"), Some(80));
        assert_eq!(port_from_base_url("https://example.test/v1"), Some(443));
        assert_eq!(port_from_base_url("not a url"), None);
    }

    /// Regression: a partial restore used to become a permanent dead end. Once
    /// the config had been written back but a later cleanup step failed, every
    /// subsequent `restore` reported `UserChangedProvider` forever.
    #[test]
    fn restore_completes_after_a_previous_partial_restore() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let catalog = dir.path().join("models.json");
        let manifest_path = dir.path().join("integration.json");

        // Simulate the state left behind when the config write succeeded but the
        // catalog/manifest cleanup did not: config is back to the user's values,
        // the manifest still claims our values are applied.
        fs::write(&config, "model_provider = \"openai\"\n").unwrap();
        fs::write(&catalog, "{}\n").unwrap();
        let manifest = IntegrationManifest {
            schema_version: 2,
            config_path: config.clone(),
            managed_field: MANAGED_KEY.into(),
            applied_value: "/tmp/omnibridge-models.json".into(),
            original_value: None,
            original_key_present: false,
            catalog_path: catalog.clone(),
            codex_binary: PathBuf::from("codex"),
            codex_version: None,
            managed_provider: MANAGED_PROVIDER_KEY.into(),
            applied_provider: OMNIBRIDGE_PROVIDER_ID.into(),
            original_provider: Some("openai".into()),
            original_provider_present: true,
            provider_table_present: false,
            model_providers_table_present: false,
            pending: false,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
            config_existed: true,
        };
        save_manifest(&manifest_path, &manifest).unwrap();

        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path.clone(),
        };

        restore(&paths).expect("restore must complete rather than dead-end");

        assert!(!catalog.exists(), "catalog should have been removed");
        assert!(!manifest_path.exists(), "manifest should have been removed");
        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            "model_provider = \"openai\"\n",
            "config must be left untouched when it is already restored"
        );
    }

    /// Build a manifest for `config`/`catalog` as a crashed install would leave it.
    fn pending_manifest(
        config: &Path,
        catalog: &Path,
        applied_value: &str,
        original_value: Option<&str>,
    ) -> IntegrationManifest {
        IntegrationManifest {
            schema_version: 2,
            config_path: config.to_path_buf(),
            managed_field: MANAGED_KEY.into(),
            applied_value: applied_value.into(),
            original_value: original_value.map(str::to_owned),
            original_key_present: original_value.is_some(),
            catalog_path: catalog.to_path_buf(),
            codex_binary: PathBuf::from("codex"),
            codex_version: None,
            managed_provider: MANAGED_PROVIDER_KEY.into(),
            applied_provider: OMNIBRIDGE_PROVIDER_ID.into(),
            original_provider: Some("openai".into()),
            original_provider_present: true,
            provider_table_present: false,
            model_providers_table_present: false,
            pending: true,
            capability_path: None,
            catalog_schema_fingerprint: None,
            official_model_count: 0,
            config_existed: true,
        }
    }

    /// Regression: the manifest used to be written *after* `config.toml`, so a
    /// crash in between left `model_provider = "omnibridge"` on disk with no
    /// undo record. Reinstall then failed with a misleading "changed outside
    /// Codex MultiProvider" and `restore_if_present` reported success without
    /// restoring anything, leaving hand-editing as the only way out.
    #[test]
    fn restore_recovers_a_manifest_left_pending_by_a_crash() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let catalog = dir.path().join("models.json");
        let manifest_path = dir.path().join("integration.json");

        // The crashed install got as far as writing the config, then died before
        // confirming the manifest.
        fs::write(
            &config,
            "model_provider = \"omnibridge\"\n[model_providers.omnibridge]\nname = \"OpenAI\"\n",
        )
        .unwrap();
        fs::write(&catalog, "{}\n").unwrap();
        let manifest = pending_manifest(
            &config,
            &catalog,
            "/tmp/omnibridge-models.json",
            Some("openai"),
        );
        save_manifest(&manifest_path, &manifest).unwrap();

        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path.clone(),
        };

        restore(&paths).expect("a pending manifest must be recoverable");

        let restored = fs::read_to_string(&config).unwrap();
        assert!(
            restored.contains("model_provider = \"openai\""),
            "the user's original provider must be restored:\n{restored}"
        );
        assert!(
            !restored.contains(OMNIBRIDGE_PROVIDER_ID),
            "our provider must not survive a pending-manifest recovery:\n{restored}"
        );
        assert!(!catalog.exists(), "catalog should have been removed");
        assert!(!manifest_path.exists(), "manifest should have been removed");
    }

    /// A crash *before* anything was changed must simply drop the record, not
    /// invent a restore of values that were never applied.
    #[test]
    fn restore_discards_a_pending_manifest_that_changed_nothing() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let catalog = dir.path().join("models.json");
        let manifest_path = dir.path().join("integration.json");

        let original = "# untouched\nmodel_provider = \"openai\"\n";
        fs::write(&config, original).unwrap();
        let manifest = pending_manifest(
            &config,
            &catalog,
            "/tmp/omnibridge-models.json",
            Some("openai"),
        );
        save_manifest(&manifest_path, &manifest).unwrap();

        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path.clone(),
        };

        restore(&paths).expect("nothing-to-do must not be an error");

        assert_eq!(
            fs::read_to_string(&config).unwrap(),
            original,
            "a config we never touched must be left byte-identical"
        );
        assert!(!manifest_path.exists(), "manifest should have been removed");
    }

    /// A confirmed (non-pending) manifest must keep the strict guard: silently
    /// reverting a config the user edited by hand would lose their changes.
    #[test]
    fn restore_still_refuses_a_confirmed_manifest_after_a_user_edit() {
        let dir = tempdir().unwrap();
        let config = dir.path().join("config.toml");
        let catalog = dir.path().join("models.json");
        let manifest_path = dir.path().join("integration.json");

        fs::write(&config, "model_provider = \"some-other-gateway\"\n").unwrap();
        let manifest = IntegrationManifest {
            pending: false,
            ..pending_manifest(
                &config,
                &catalog,
                "/tmp/omnibridge-models.json",
                Some("openai"),
            )
        };
        save_manifest(&manifest_path, &manifest).unwrap();

        let paths = IntegrationPaths {
            config_dir: dir.path().into(),
            codex_config: config.clone(),
            catalog: catalog.clone(),
            manifest: manifest_path.clone(),
        };

        assert!(matches!(
            restore(&paths),
            Err(IntegrationError::UserChangedProvider)
        ));
    }
}
