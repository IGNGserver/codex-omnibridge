pub mod account;

pub use account::{
    AccountError, AccountManager, AccountSummary, AccountTokens, AccountUsageSnapshot,
    ActiveAccountStatus, ManagedAccount, RateLimitSummary, ReserveLimitSummary, RestartCodexReport,
    UsageWindow, default_accounts_path, default_codex_home,
};

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use codex_mp_core::{
    AuthStrategy, CoreError, CustomModel, ModelCapabilities, ModelEdit, ProviderConfig,
    ProviderProtocol, ProviderRegistry,
};
use codex_mp_credentials::{CredentialStore, CredentialStoreError, NativeCredentialStore};
use codex_mp_router::{CAPABILITY_HEADER, RouterEndpoint, RouterError, load_router_endpoint};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::io::AsyncReadExt;
use tokio::process::{Child, Command};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(15);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
/// How long a second process waits for an in-progress startup before giving up.
const STARTUP_LOCK_TIMEOUT: Duration = Duration::from_secs(20);
/// A lock older than this is reclaimed even if its PID still looks alive.
const STARTUP_LOCK_STALE: Duration = Duration::from_secs(120);
/// Health/lifecycle probes must not block on a stale endpoint whose loopback
/// port was since reallocated to a service that accepts and then stalls.
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);
/// Keep startup diagnostics useful without allowing a broken Router to fill
/// memory or leak a credential into the web status response.
const MAX_ROUTER_STARTUP_DIAGNOSTIC_BYTES: usize = 4 * 1024;

#[derive(Debug, Error)]
pub enum ProviderManagerError {
    #[error("provider registry error: {0}")]
    Core(#[from] CoreError),
    #[error("credential store error: {0}")]
    Credential(#[from] CredentialStoreError),
    #[error("provider response error: {0}")]
    Response(#[from] reqwest::Error),
    #[error("provider returned HTTP {status}: {message}")]
    Http { status: u16, message: String },
    #[error("provider response is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("provider response has no data/models array")]
    MissingModelList,
    #[error("provider response contained no usable model ids")]
    EmptyModelList,
    #[error("model `{0}` was not returned by provider")]
    ModelNotDiscovered(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DiscoveredModel {
    pub upstream_model_id: String,
    pub display_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelSummary {
    pub logical_model_id: String,
    pub upstream_model_id: String,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
    pub enabled: bool,
    pub context_window: Option<u64>,
    pub reasoning_levels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProviderSummary {
    pub id: String,
    pub name: String,
    pub base_url: String,
    pub protocol: ProviderProtocol,
    pub auth_strategy: AuthStrategy,
    pub enabled: bool,
    pub models: Vec<ModelSummary>,
}

/// Deadline for a provider model-discovery round trip.
/// Largest model-list response accepted from a provider.
///
/// A model list is a small document; anything larger is a hostile or broken
/// provider, and `text()` would buffer all of it into memory.
const MAX_DISCOVERY_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Client used for `GET {base_url}/models`.
///
/// This request carries the provider's credential, so it must not follow
/// redirects: reqwest only strips `authorization` on a cross-host redirect, so a
/// gateway answering `302 Location: https://attacker.example/models` would
/// receive the user's key verbatim for the `x-api-key` and custom-header auth
/// strategies. A plain `Client::new()` also has no timeout, which let a hung
/// provider wedge the panel request that triggered discovery.
fn build_discovery_client() -> Client {
    Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(10))
        .timeout(DISCOVERY_TIMEOUT)
        .build()
        .unwrap_or_else(|error| {
            eprintln!(
                "codex-mp: could not build the hardened discovery client ({error}); \
                 retrying without a request timeout"
            );
            // Keep the security-critical knob (no redirects) even in the retry.
            Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(Duration::from_secs(10))
                .build()
                .unwrap_or_else(|_| {
                    // Last resort, and a genuine downgrade: `Client::new()` uses
                    // reqwest's default policy, which FOLLOWS redirects, so a
                    // provider could bounce the discovery request (and the API key
                    // attached to it) to an arbitrary host. Say so loudly instead
                    // of leaving a silent security regression — the router's
                    // equivalent fallback already reports it in the same terms.
                    eprintln!(
                        "codex-mp: FATAL: no hardened discovery client available; \
                         provider redirects will be followed during model discovery"
                    );
                    Client::new()
                })
        })
}

pub struct ProviderManager {
    registry_path: PathBuf,
    credentials: Arc<dyn CredentialStore>,
    http_client: Client,
}

impl ProviderManager {
    pub fn new(registry_path: impl Into<PathBuf>) -> Self {
        Self::with_credentials(registry_path, Arc::new(NativeCredentialStore::default()))
    }

    pub fn with_credentials(
        registry_path: impl Into<PathBuf>,
        credentials: Arc<dyn CredentialStore>,
    ) -> Self {
        Self {
            registry_path: registry_path.into(),
            credentials,
            http_client: build_discovery_client(),
        }
    }

    pub fn registry_path(&self) -> &Path {
        &self.registry_path
    }

    pub fn list_providers(&self) -> Result<Vec<ProviderSummary>, ProviderManagerError> {
        let registry = self.load_registry()?;
        Ok(registry.providers().iter().map(provider_summary).collect())
    }

    pub fn add_provider(
        &self,
        name: &str,
        base_url: &str,
        protocol: ProviderProtocol,
        api_key: Option<SecretString>,
    ) -> Result<ProviderSummary, ProviderManagerError> {
        self.add_provider_with_auth(name, base_url, protocol, AuthStrategy::Bearer, api_key)
    }

    pub fn add_provider_with_auth(
        &self,
        name: &str,
        base_url: &str,
        protocol: ProviderProtocol,
        auth_strategy: AuthStrategy,
        api_key: Option<SecretString>,
    ) -> Result<ProviderSummary, ProviderManagerError> {
        // Hold the cross-process lock across the whole read-modify-write. Using
        // the unlocked loader here lost concurrent updates: 20 simultaneous HTTP
        // `providers/add` calls all returned 200 while only one provider landed.
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        let previous = registry.clone();
        let mut provider = ProviderConfig::new(name, base_url)?;
        provider.protocol = protocol;
        provider.auth_strategy = auth_strategy;
        let provider_id = provider.id.clone();
        let reference = provider.credential_reference.clone();
        registry.add_provider(provider)?;
        self.save_provider_change(&registry, &previous, &reference, api_key.as_ref())?;
        let provider = registry
            .provider(&provider_id)
            .expect("provider was added before save");
        Ok(provider_summary(provider))
    }

    pub fn edit_provider(
        &self,
        id: &str,
        name: Option<String>,
        base_url: Option<String>,
        protocol: Option<ProviderProtocol>,
        enabled: Option<bool>,
        api_key: Option<SecretString>,
    ) -> Result<ProviderSummary, ProviderManagerError> {
        self.edit_provider_with_auth(id, name, base_url, protocol, enabled, None, api_key)
    }

    pub fn edit_provider_with_auth(
        &self,
        id: &str,
        name: Option<String>,
        base_url: Option<String>,
        protocol: Option<ProviderProtocol>,
        enabled: Option<bool>,
        auth_strategy: Option<AuthStrategy>,
        api_key: Option<SecretString>,
    ) -> Result<ProviderSummary, ProviderManagerError> {
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        let previous = registry.clone();
        let provider = registry
            .provider_mut(id)
            .ok_or_else(|| CoreError::ProviderNotFound(id.to_owned()))?;
        if let Some(name) = name {
            provider.name = name;
        }
        if let Some(base_url) = base_url {
            provider.base_url = codex_mp_core::normalize_base_url(&base_url)?;
        }
        if let Some(protocol) = protocol {
            provider.protocol = protocol;
        }
        if let Some(enabled) = enabled {
            provider.enabled = enabled;
        }
        if let Some(auth_strategy) = auth_strategy {
            provider.auth_strategy = auth_strategy;
        }
        let reference = provider.credential_reference.clone();
        // Everything mutated above (base_url, protocol, enabled) is routing state,
        // and the credential may be replaced below. A running Router caches
        // resolved credentials per `registry.generation()`, and only a *change* of
        // generation clears that cache — so without this bump an edited API key
        // kept resolving to the previous secret until the Router restarted.
        registry.bump_generation();
        self.save_provider_change(&registry, &previous, &reference, api_key.as_ref())?;
        Ok(provider_summary(
            registry
                .provider(id)
                .expect("provider was validated before save"),
        ))
    }

    pub fn remove_provider(
        &self,
        id: &str,
        purge_credential: bool,
    ) -> Result<(), ProviderManagerError> {
        // Locked for the same reason as `add_provider` above.
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        let previous = registry.clone();
        let provider = registry.remove_provider(id)?;
        let previous_secret = if purge_credential {
            match self.credentials.get(&provider.credential_reference) {
                Ok(secret) => {
                    self.credentials.delete(&provider.credential_reference)?;
                    Some(secret)
                }
                Err(CredentialStoreError::NotFound(_)) => None,
                Err(error) => return Err(error.into()),
            }
        } else {
            None
        };
        if let Err(error) = registry.save() {
            let registry_rollback = previous.save();
            let credential_rollback = match previous_secret.as_ref() {
                Some(secret) => self
                    .credentials
                    .set(&provider.credential_reference, secret)
                    .map_err(|rollback| rollback.to_string()),
                None => Ok(()),
            };
            if let Err(rollback) = registry_rollback {
                return Err(ProviderManagerError::Core(rollback));
            }
            if let Err(rollback) = credential_rollback {
                return Err(ProviderManagerError::Credential(
                    CredentialStoreError::Backend(rollback),
                ));
            }
            return Err(error.into());
        }
        Ok(())
    }

    pub async fn discover_models(
        &self,
        id: &str,
    ) -> Result<Vec<DiscoveredModel>, ProviderManagerError> {
        let registry = self.load_registry()?;
        let provider = registry
            .provider(id)
            .ok_or_else(|| CoreError::ProviderNotFound(id.to_owned()))?;
        // Offloaded: see `codex_mp_credentials::run_blocking`. A direct keyring
        // call here panics under `#[tokio::main]` on Linux (zbus calls block_on).
        let key = codex_mp_credentials::get_blocking(
            self.credentials.clone(),
            provider.credential_reference.clone(),
        )
        .await?;
        let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
        let mut request = self.http_client.get(url);
        // Probe with the provider's *configured* auth strategy. Hardcoding a
        // Bearer token broke discovery for every api_key/header provider.
        if let Some((name, value)) = provider
            .auth_strategy
            .credential_header(key.expose_secret())
        {
            request = request.header(name, value);
        }
        let response = request.send().await?;
        let status = response.status();
        // Bound the read: `text()` buffers the whole body, so a hostile or broken
        // provider could exhaust the panel's memory with a multi-gigabyte "model
        // list". The router already caps upstream responses; discovery must too.
        if let Some(length) = response.content_length()
            && length > MAX_DISCOVERY_RESPONSE_BYTES as u64
        {
            return Err(ProviderManagerError::Http {
                status: status.as_u16(),
                message: format!(
                    "provider returned {length} bytes, which exceeds the \
                     {MAX_DISCOVERY_RESPONSE_BYTES} byte limit for a model list"
                ),
            });
        }
        let body = response.text().await?;
        if body.len() > MAX_DISCOVERY_RESPONSE_BYTES {
            return Err(ProviderManagerError::Http {
                status: status.as_u16(),
                message: format!(
                    "provider returned {} bytes, which exceeds the \
                     {MAX_DISCOVERY_RESPONSE_BYTES} byte limit for a model list",
                    body.len()
                ),
            });
        }
        let value: serde_json::Value = serde_json::from_str(&body)?;
        if !status.is_success() {
            return Err(ProviderManagerError::Http {
                status: status.as_u16(),
                message: value
                    .get("error")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("request failed")
                    .to_owned(),
            });
        }
        parse_discovered_models(&value)
    }

    pub fn import_models(
        &self,
        id: &str,
        discovered: &[DiscoveredModel],
        selected_ids: &[String],
    ) -> Result<Vec<ModelSummary>, ProviderManagerError> {
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        let provider_name = registry
            .provider(id)
            .ok_or_else(|| CoreError::ProviderNotFound(id.to_owned()))?
            .name
            .clone();
        for selected_id in selected_ids {
            let discovered_model = discovered
                .iter()
                .find(|model| &model.upstream_model_id == selected_id)
                .ok_or_else(|| ProviderManagerError::ModelNotDiscovered(selected_id.clone()))?;
            if registry.provider(id).is_some_and(|provider| {
                provider
                    .models
                    .iter()
                    .any(|model| model.upstream_model_id == discovered_model.upstream_model_id)
            }) {
                continue;
            }
            let display = discovered_model
                .display_name
                .as_deref()
                .unwrap_or(&discovered_model.upstream_model_id);
            registry.add_model(CustomModel::new(
                id,
                &discovered_model.upstream_model_id,
                &format!("{provider_name} / {display}"),
            )?)?;
        }
        registry.save()?;
        Ok(registry
            .provider(id)
            .expect("provider was validated before import")
            .models
            .iter()
            .map(model_summary)
            .collect())
    }

    pub fn add_model(
        &self,
        provider_id: &str,
        upstream_model_id: &str,
        display_name: &str,
        context_window: Option<u64>,
        capabilities: ModelCapabilities,
    ) -> Result<ModelSummary, ProviderManagerError> {
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        let mut model = CustomModel::new(provider_id, upstream_model_id, display_name)?;
        model.context_window = context_window;
        model.capabilities = capabilities;
        registry.add_model(model.clone())?;
        registry.save()?;
        Ok(model_summary(&model))
    }

    pub fn edit_model(
        &self,
        logical_model_id: &str,
        edit: ModelEdit,
    ) -> Result<ModelSummary, ProviderManagerError> {
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        registry.edit_model(logical_model_id, edit)?;
        registry.save()?;
        let (provider_id, _) = logical_model_id
            .split_once('/')
            .ok_or_else(|| CoreError::InvalidModelId(logical_model_id.to_owned()))?;
        let model = registry
            .provider(provider_id)
            .and_then(|provider| {
                provider
                    .models
                    .iter()
                    .find(|model| model.logical_model_id == logical_model_id)
            })
            .expect("model was validated before save");
        Ok(model_summary(model))
    }

    pub fn set_model_enabled(
        &self,
        logical_model_id: &str,
        enabled: bool,
    ) -> Result<ModelSummary, ProviderManagerError> {
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        let (provider_id, _) = logical_model_id
            .split_once('/')
            .ok_or_else(|| CoreError::InvalidModelId(logical_model_id.to_owned()))?;
        let provider = registry
            .provider_mut(provider_id)
            .ok_or_else(|| CoreError::ProviderNotFound(provider_id.to_owned()))?;
        let model = provider
            .models
            .iter_mut()
            .find(|model| model.logical_model_id == logical_model_id)
            .ok_or_else(|| CoreError::ModelNotFound(logical_model_id.to_owned()))?;
        model.enabled = enabled;
        let summary = model_summary(model);
        // The enabled set is routing state; bump so a running Router (and the
        // per-generation credential cache) sees a new generation.
        registry.bump_generation();
        registry.save()?;
        Ok(summary)
    }

    pub fn remove_model(&self, logical_model_id: &str) -> Result<(), ProviderManagerError> {
        let (mut registry, _registry_lock) = self.load_registry_locked()?;
        registry.remove_model(logical_model_id)?;
        registry.save()?;
        Ok(())
    }

    /// Delete every stored provider credential, and optionally the registry.
    ///
    /// Keyring work is offloaded per reference. On Linux the keyring reaches the
    /// Secret Service through `zbus`, which bridges to its synchronous API with
    /// `tokio::runtime::Runtime::block_on`; calling that from the async
    /// `codex-mp uninstall` path panicked with "Cannot start a runtime from
    /// within a runtime" and aborted the uninstall after the config had already
    /// been restored, leaving our own state behind.
    pub async fn purge_provider_data(
        &self,
        remove_registry: bool,
    ) -> Result<usize, ProviderManagerError> {
        // When the registry is being removed, an unreadable one must not block the
        // purge. A hand-edit that broke the JSON used to make `codex-mp uninstall`
        // fail outright: the user was left with a hijacked Codex config, live
        // credentials, and no CLI way out — the same trap N-65 fixed for a broken
        // `config.toml`. There are no known credential references to delete in that
        // case, which is exactly what "purge" should do: remove the file.
        let registry = if remove_registry {
            self.load_registry().ok()
        } else {
            Some(self.load_registry()?)
        };
        let references: BTreeSet<String> = registry
            .as_ref()
            .map(|registry| {
                registry
                    .providers()
                    .iter()
                    .map(|provider| provider.credential_reference.clone())
                    .collect()
            })
            .unwrap_or_default();
        let mut credentials = Vec::new();
        for reference in references {
            match codex_mp_credentials::get_blocking(self.credentials.clone(), reference.clone())
                .await
            {
                Ok(secret) => credentials.push((reference, secret)),
                Err(CredentialStoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }

        let mut deleted = Vec::new();
        for (reference, secret) in &credentials {
            if let Err(error) =
                codex_mp_credentials::delete_blocking(self.credentials.clone(), reference.clone())
                    .await
            {
                restore_deleted_credentials_blocking(self.credentials.clone(), &deleted).await?;
                return Err(error.into());
            }
            deleted.push((reference.clone(), secret.clone()));
        }

        if remove_registry
            && self.registry_path.exists()
            && let Err(error) = std::fs::remove_file(&self.registry_path)
        {
            restore_deleted_credentials_blocking(self.credentials.clone(), &deleted).await?;
            return Err(CoreError::Io(error).into());
        }
        Ok(deleted.len())
    }

    fn load_registry(&self) -> Result<ProviderRegistry, ProviderManagerError> {
        Ok(ProviderRegistry::load(&self.registry_path)?)
    }

    /// Load the registry while holding the cross-process lock.
    ///
    /// Callers that mutate **must** use this and keep the returned guard alive
    /// until after `save()`. `load_registry()` + `save()` is a lost-update race:
    /// two writers (the panel and the CLI, say) can each read revision N and both
    /// replace it, silently discarding one change.
    fn load_registry_locked(
        &self,
    ) -> Result<(ProviderRegistry, codex_mp_core::FileLock), ProviderManagerError> {
        let guard = codex_mp_core::FileLock::acquire(&self.registry_path).map_err(CoreError::Io)?;
        let registry = ProviderRegistry::load(&self.registry_path)?;
        Ok((registry, guard))
    }

    fn save_provider_change(
        &self,
        registry: &ProviderRegistry,
        previous: &ProviderRegistry,
        reference: &str,
        secret: Option<&SecretString>,
    ) -> Result<(), ProviderManagerError> {
        // Distinguish "there was no previous secret" from "the read failed".
        // `.ok()` collapsed both to `None`, and the rollback below deletes the
        // credential when it sees `None` — so a transient keyring read failure
        // followed by a failed save would destroy the user's still-valid key.
        let previous_secret = if secret.is_some() {
            match self.credentials.get(reference) {
                Ok(previous) => Some(previous),
                Err(CredentialStoreError::NotFound(_)) => None,
                Err(error) => {
                    return Err(ProviderManagerError::Credential(error));
                }
            }
        } else {
            None
        };
        if let Some(secret) = secret {
            self.credentials.set(reference, secret)?;
        }
        if let Err(error) = registry.save() {
            let _ = previous.save();
            let _ = restore_secret(&*self.credentials, reference, previous_secret.as_ref());
            return Err(error.into());
        }
        Ok(())
    }
}

fn provider_summary(provider: &ProviderConfig) -> ProviderSummary {
    ProviderSummary {
        id: provider.id.clone(),
        name: provider.name.clone(),
        base_url: provider.base_url.clone(),
        protocol: provider.protocol,
        auth_strategy: provider.auth_strategy.clone(),
        enabled: provider.enabled,
        models: provider.models.iter().map(model_summary).collect(),
    }
}

fn model_summary(model: &CustomModel) -> ModelSummary {
    ModelSummary {
        logical_model_id: model.logical_model_id.clone(),
        upstream_model_id: model.upstream_model_id.clone(),
        display_name: model.display_name.clone(),
        capabilities: model.capabilities.clone(),
        enabled: model.enabled,
        context_window: model.context_window,
        reasoning_levels: model.reasoning_levels.clone(),
    }
}

fn parse_discovered_models(
    value: &serde_json::Value,
) -> Result<Vec<DiscoveredModel>, ProviderManagerError> {
    let list = value
        .as_array()
        .or_else(|| value.get("data").and_then(serde_json::Value::as_array))
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .ok_or(ProviderManagerError::MissingModelList)?;
    let mut models = Vec::with_capacity(list.len());
    for item in list {
        if let Some(upstream_model_id) = item.as_str() {
            let upstream_model_id = upstream_model_id.trim();
            if !upstream_model_id.is_empty() {
                models.push(DiscoveredModel {
                    upstream_model_id: upstream_model_id.to_owned(),
                    display_name: None,
                });
            }
            continue;
        }
        let Some(object) = item.as_object() else {
            continue;
        };
        let Some(upstream_model_id) = ["id", "slug", "model", "name"]
            .iter()
            .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let display_name = ["display_name", "name"]
            .iter()
            .find_map(|key| object.get(*key).and_then(serde_json::Value::as_str))
            .map(str::to_owned);
        models.push(DiscoveredModel {
            upstream_model_id: upstream_model_id.to_owned(),
            display_name,
        });
    }
    if models.is_empty() && !list.is_empty() {
        return Err(ProviderManagerError::EmptyModelList);
    }
    Ok(models)
}

fn restore_secret(
    store: &dyn CredentialStore,
    reference: &str,
    previous_secret: Option<&SecretString>,
) -> Result<(), CredentialStoreError> {
    match previous_secret {
        Some(secret) => store.set(reference, secret),
        None => match store.delete(reference) {
            Ok(()) | Err(CredentialStoreError::NotFound(_)) => Ok(()),
            Err(error) => Err(error),
        },
    }
}

/// Async rollback counterpart; see [`ProviderManager::purge_provider_data`].
async fn restore_deleted_credentials_blocking(
    store: Arc<dyn CredentialStore>,
    deleted: &[(String, SecretString)],
) -> Result<(), ProviderManagerError> {
    for (reference, secret) in deleted {
        codex_mp_credentials::set_blocking(store.clone(), reference.clone(), secret.clone())
            .await?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("router process error: {0}")]
    Process(#[from] std::io::Error),
    #[error("router endpoint error: {0}")]
    Endpoint(#[from] RouterError),
    #[error(
        "router startup failed ({reason}) waiting for `{endpoint}`; executable `{executable}`, \
         expected loopback port {port}"
    )]
    StartupTimeout {
        endpoint: PathBuf,
        executable: PathBuf,
        port: u16,
        reason: String,
    },
    #[error("router request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("router reload returned HTTP {status}: {message}")]
    ReloadFailed { status: u16, message: String },
    #[error("router shutdown returned HTTP {status}: {message}")]
    ShutdownFailed { status: u16, message: String },
    #[error("another process is already starting the router (lock file `{0}`); retry in a moment")]
    StartupLocked(PathBuf),
    #[error(
        "the port this Codex config expects for the router ({port}) is already in use; \
         stop whatever is listening there, or change the port with CODEX_MP_ROUTER_BASE_URL \
         and re-run `codex-mp sync`"
    )]
    PortInUse { port: u16 },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouterStatus {
    pub running: bool,
    pub healthy: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// Health of a published Router endpoint relative to the local registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EndpointHealth {
    /// Reachable, identifies as OmniBridge, and sees the current registry.
    Current,
    /// Reachable and ours, but built from an older registry revision.
    StaleGeneration,
    /// Not reachable, not ours, or the local registry cannot be read.
    Unreachable,
}

/// Cross-process startup lock.
///
/// Two `codex-mp` processes starting at the same moment both used to observe
/// "no healthy endpoint", both delete the endpoint file, and both spawn a Router
/// on a random port (`--port 0`). The loser then overwrote the winner's endpoint
/// file (or deleted it on exit) and became an undiscoverable orphan. An exclusive
/// lock file makes the second process wait for the first instead.
struct StartupLock {
    path: PathBuf,
    acquired: bool,
}

impl StartupLock {
    /// Try to take the lock, waiting up to `STARTUP_LOCK_TIMEOUT` for whoever
    /// holds it. A stale lock (dead PID, or older than `STARTUP_LOCK_STALE`) is
    /// reclaimed so a crashed process cannot wedge startup for ever.
    async fn acquire(endpoint_file: &Path) -> Result<Self, ManagerError> {
        let path = lock_path_for(endpoint_file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let deadline = tokio::time::Instant::now() + STARTUP_LOCK_TIMEOUT;
        loop {
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(mut file) => {
                    use std::io::Write;
                    let _ = write!(file, "{}", std::process::id());
                    return Ok(Self {
                        path,
                        acquired: true,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if lock_is_stale(&path) {
                        // A previous holder died without cleaning up.
                        let _ = std::fs::remove_file(&path);
                        continue;
                    }
                    if tokio::time::Instant::now() >= deadline {
                        return Err(ManagerError::StartupLocked(path));
                    }
                    sleep(POLL_INTERVAL).await;
                }
                Err(error) => return Err(error.into()),
            }
        }
    }
}

impl Drop for StartupLock {
    fn drop(&mut self) {
        if self.acquired {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn lock_path_for(endpoint_file: &Path) -> PathBuf {
    endpoint_file.with_extension("lock")
}

/// A lock is stale when its PID is gone, or when it is older than
/// `STARTUP_LOCK_STALE`. The age check matters when the PID was reused.
fn lock_is_stale(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return true;
    };
    if let Ok(modified) = metadata.modified()
        && let Ok(age) = std::time::SystemTime::now().duration_since(modified)
        && age > STARTUP_LOCK_STALE
    {
        return true;
    }
    let Ok(contents) = std::fs::read_to_string(path) else {
        return false;
    };
    match contents.trim().parse::<u32>() {
        Ok(pid) => !process_is_alive(pid),
        // An unreadable/empty lock is treated as live; the age check above
        // eventually reclaims it.
        Err(_) => false,
    }
}

#[cfg(unix)]
fn process_is_alive(pid: u32) -> bool {
    // `kill(pid, 0)` performs error checking without sending a signal.
    unsafe { libc::kill(pid as i32, 0) == 0 }
}

#[cfg(windows)]
fn process_is_alive(pid: u32) -> bool {
    // `tasklist` is the only dependency-free probe available here. A failure to
    // run it reports "alive", which is the safe direction: the age check is the
    // backstop.
    std::process::Command::new("tasklist.exe")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stdout);
            text.contains(&format!("\"{pid}\""))
        })
        .unwrap_or(true)
}

#[cfg(not(any(unix, windows)))]
fn process_is_alive(_pid: u32) -> bool {
    true
}

pub struct RouterSupervisor {
    executable: PathBuf,
    registry_path: PathBuf,
    endpoint_file: PathBuf,
    client: Client,
    child: Option<Child>,
    owns_endpoint: bool,
    reused_endpoint: bool,
    /// Port to bind the supervised Router on. `0` means "let the OS choose".
    ///
    /// `sync` writes a fixed `base_url` into `config.toml`
    /// (`http://127.0.0.1:8787/v1` by default) and stock Codex reads *that*, not
    /// the endpoint file. Starting the Router on a random port therefore left
    /// Codex talking to a port nobody listened on. Callers that launch stock
    /// Codex pass the same port the config advertises.
    port: u16,
    /// The endpoint this supervisor is actually responsible for.
    ///
    /// `reload()`/`shutdown()` used to re-read the shared endpoint file, so if a
    /// second supervisor had rewritten it in the meantime this one would send
    /// `/admin/shutdown` to a Router it does not own, then SIGKILL its own child
    /// and delete the other owner's endpoint file. Remembering the endpoint we
    /// were handed pins every subsequent action to the right process.
    active_endpoint: Option<RouterEndpoint>,
    /// Last error recorded during router startup or operation.
    last_error: Option<String>,
}

impl RouterSupervisor {
    pub fn new(
        executable: impl Into<PathBuf>,
        registry_path: impl Into<PathBuf>,
        endpoint_file: impl Into<PathBuf>,
    ) -> Self {
        Self {
            executable: executable.into(),
            registry_path: registry_path.into(),
            endpoint_file: endpoint_file.into(),
            // A short timeout so a stale endpoint pointing at a wedged loopback
            // service cannot hang `start()`/`status()`/`stop()` indefinitely.
            // Redirects are refused: these requests carry the Router capability
            // token, and reqwest's default policy would resend them (body
            // included) to whatever host a 307/308 names.
            client: Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(PROBE_TIMEOUT)
                .connect_timeout(PROBE_TIMEOUT)
                .build()
                .unwrap_or_else(|_| {
                    eprintln!(
                        "codex-mp: FATAL: no hardened router-probe client available; \
                         redirects will be followed"
                    );
                    Client::new()
                }),
            child: None,
            owns_endpoint: false,
            reused_endpoint: false,
            active_endpoint: None,
            port: 0,
            last_error: None,
        }
    }

    pub fn endpoint_file(&self) -> &Path {
        &self.endpoint_file
    }

    pub fn set_executable(&mut self, executable: impl Into<PathBuf>) {
        if self.child.is_none() {
            self.executable = executable.into();
        }
    }

    /// Override the endpoint this supervisor acts on. Test-only: production code
    /// gets this from `start()`.
    #[cfg(test)]
    fn set_active_endpoint_for_test(&mut self, endpoint: RouterEndpoint) {
        self.active_endpoint = Some(endpoint);
    }

    /// Bind the supervised Router on a fixed port instead of an ephemeral one.
    ///
    /// Must match the `base_url` written into `config.toml`, because stock Codex
    /// dials that address directly and ignores the endpoint file.
    pub fn with_port(mut self, port: u16) -> Self {
        self.port = port;
        self
    }

    pub async fn start(&mut self) -> Result<RouterEndpoint, ManagerError> {
        match self.start_inner().await {
            Ok(endpoint) => {
                self.last_error = None;
                Ok(endpoint)
            }
            Err(err) => {
                self.last_error = Some(err.to_string());
                Err(err)
            }
        }
    }

    async fn start_inner(&mut self) -> Result<RouterEndpoint, ManagerError> {
        if let Some(child) = self.child.as_mut()
            && child.try_wait()?.is_none()
        {
            if let Some(endpoint) = self.active_endpoint.clone() {
                return Ok(endpoint);
            }
            let endpoint = load_router_endpoint(&self.endpoint_file)?;
            self.active_endpoint = Some(endpoint.clone());
            return Ok(endpoint);
        }
        self.child = None;
        self.owns_endpoint = false;
        self.reused_endpoint = false;
        self.active_endpoint = None;

        // Serialize startup across processes. Without this, two processes both
        // see "no healthy endpoint", both delete the file and both spawn a
        // Router; the loser becomes an orphan holding a port.
        let _lock = StartupLock::acquire(&self.endpoint_file).await?;

        // Re-check under the lock: the process that held it may have just
        // published a healthy Router we can share.
        if let Ok(endpoint) = load_router_endpoint(&self.endpoint_file) {
            match self.endpoint_health(&endpoint).await {
                EndpointHealth::Current => {
                    self.reused_endpoint = true;
                    self.active_endpoint = Some(endpoint.clone());
                    return Ok(endpoint);
                }
                // The Router is alive but has a stale registry view. Ask it to
                // reload instead of abandoning it: replacing it deleted the live
                // process's endpoint file and leaked an orphan on every registry
                // change made outside the running Router (e.g. a CLI
                // `provider add` while the panel's Router serves traffic).
                EndpointHealth::StaleGeneration => {
                    if self.request_reload(&endpoint).await.is_ok()
                        && matches!(
                            self.endpoint_health(&endpoint).await,
                            EndpointHealth::Current
                        )
                    {
                        self.reused_endpoint = true;
                        self.active_endpoint = Some(endpoint.clone());
                        return Ok(endpoint);
                    }
                    // Reload refused or did not take: fall through and replace it,
                    // but stop the old one first so it cannot linger.
                    let _ = self.request_shutdown(&endpoint).await;
                }
                EndpointHealth::Unreachable => {}
            }
        }
        remove_stale_endpoint(&self.endpoint_file)?;

        // The port is fixed (it must match `config.toml`), so a conflict is a real
        // failure mode rather than something the OS routes around. Detect it up
        // front so the operator gets an actionable message instead of a silent
        // startup timeout.
        if self.port != 0 && !port_is_available(self.port) {
            return Err(ManagerError::PortInUse { port: self.port });
        }

        let mut child = Command::new(&self.executable);
        child
            .arg("--registry")
            .arg(&self.registry_path)
            .arg("router")
            .arg("--port")
            .arg(self.port.to_string())
            .arg("--endpoint-file")
            .arg(&self.endpoint_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            // Without this, SIGTERM (or a panicking/aborting host) left the
            // Router running as an orphan holding its port and endpoint file.
            .kill_on_drop(true);
        configure_background_process(&mut child);
        let mut spawned_child = child.spawn()?;
        let stderr_task = spawned_child.stderr.take().map(|mut stderr| {
            tokio::spawn(async move {
                let mut retained = Vec::with_capacity(MAX_ROUTER_STARTUP_DIAGNOSTIC_BYTES);
                let mut buffer = [0_u8; 1024];
                loop {
                    match stderr.read(&mut buffer).await {
                        Ok(0) | Err(_) => break,
                        Ok(read) => {
                            if retained.len() < MAX_ROUTER_STARTUP_DIAGNOSTIC_BYTES {
                                let remaining =
                                    MAX_ROUTER_STARTUP_DIAGNOSTIC_BYTES - retained.len();
                                retained.extend_from_slice(&buffer[..read.min(remaining)]);
                            }
                        }
                    }
                }
                sanitize_router_startup_diagnostic(&retained)
            })
        });
        self.child = Some(spawned_child);
        let endpoint_file = self.endpoint_file.clone();
        let executable = self.executable.clone();
        let port = self.port;
        let startup_result = timeout(STARTUP_TIMEOUT, async {
            loop {
                if let Ok(endpoint) = load_router_endpoint(&endpoint_file) {
                    return Ok(endpoint);
                }
                if let Some(child) = self.child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(status)) => {
                            return Err(format!("child exited with {status}"));
                        }
                        Ok(None) => {}
                        Err(error) => {
                            return Err(format!("could not inspect child: {error}"));
                        }
                    }
                }
                sleep(POLL_INTERVAL).await;
            }
        })
        .await;
        let endpoint = match startup_result {
            Ok(Ok(endpoint)) => endpoint,
            Ok(Err(reason)) => {
                let diagnostic = self.abort_failed_start(stderr_task).await;
                let reason = append_router_startup_diagnostic(reason, diagnostic);
                return Err(ManagerError::StartupTimeout {
                    endpoint: self.endpoint_file.clone(),
                    executable,
                    port,
                    reason,
                });
            }
            Err(_) => {
                let diagnostic = self.abort_failed_start(stderr_task).await;
                let reason = append_router_startup_diagnostic(
                    format!("timed out after {} seconds", STARTUP_TIMEOUT.as_secs()),
                    diagnostic,
                );
                return Err(ManagerError::StartupTimeout {
                    endpoint: self.endpoint_file.clone(),
                    executable,
                    port,
                    reason,
                });
            }
        };
        self.owns_endpoint = true;
        self.active_endpoint = Some(endpoint.clone());
        Ok(endpoint)
    }

    /// Kill and reap a child that failed before it published a usable endpoint.
    /// The normal `stop()` path cannot be used here because there is no trusted
    /// endpoint to call, and leaving `self.child` populated makes the next
    /// restart look like an already-running Router.
    async fn abort_failed_start(&mut self, stderr_task: Option<JoinHandle<String>>) -> String {
        if let Some(mut child) = self.child.take() {
            match child.try_wait() {
                Ok(Some(_)) => {}
                Ok(None) | Err(_) => {
                    let _ = child.kill().await;
                    let _ = child.wait().await;
                }
            }
        }
        self.owns_endpoint = false;
        self.reused_endpoint = false;
        self.active_endpoint = None;
        let _ = remove_stale_endpoint(&self.endpoint_file);

        let Some(task) = stderr_task else {
            return String::new();
        };
        match timeout(Duration::from_millis(500), task).await {
            Ok(Ok(diagnostic)) => diagnostic,
            Ok(Err(error)) => format!("stderr reader failed: {error}"),
            Err(_) => String::new(),
        }
    }

    /// Classify a published endpoint relative to the current registry.
    async fn endpoint_health(&self, endpoint: &RouterEndpoint) -> EndpointHealth {
        // A registry we cannot read must not be treated as "any generation is
        // fine": that accepted a Router built from a corrupt/unreadable registry.
        let Ok(expected_generation) = codex_mp_core::ProviderRegistry::load(&self.registry_path)
            .map(|registry| registry.generation())
        else {
            return EndpointHealth::Unreachable;
        };
        let request = self
            .client
            .get(format!("{}/readyz", endpoint.base_url))
            .header(CAPABILITY_HEADER, endpoint.capability_token.expose_secret())
            .send();
        let Ok(response) = request.await else {
            return EndpointHealth::Unreachable;
        };
        if !response.status().is_success() {
            return EndpointHealth::Unreachable;
        }
        let Ok(payload) = response.json::<serde_json::Value>().await else {
            return EndpointHealth::Unreachable;
        };
        if payload.get("instance").and_then(serde_json::Value::as_str) != Some("omnibridge") {
            return EndpointHealth::Unreachable;
        }
        match payload
            .get("registry_generation")
            .and_then(serde_json::Value::as_u64)
        {
            Some(generation) if generation == expected_generation => EndpointHealth::Current,
            _ => EndpointHealth::StaleGeneration,
        }
    }

    async fn endpoint_is_healthy(&self, endpoint: &RouterEndpoint) -> bool {
        matches!(
            self.endpoint_health(endpoint).await,
            EndpointHealth::Current
        )
    }

    /// Ask the Router at `endpoint` to reload its registry.
    async fn request_reload(&self, endpoint: &RouterEndpoint) -> Result<(), ManagerError> {
        let response = self
            .client
            .post(format!("{}/admin/reload", endpoint.base_url))
            .header(CAPABILITY_HEADER, endpoint.capability_token.expose_secret())
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();
        Err(ManagerError::ReloadFailed { status, message })
    }

    /// Ask the Router at `endpoint` to stop.
    async fn request_shutdown(&self, endpoint: &RouterEndpoint) -> Result<(), ManagerError> {
        let response = self
            .client
            .post(format!("{}/admin/shutdown", endpoint.base_url))
            .header(CAPABILITY_HEADER, endpoint.capability_token.expose_secret())
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();
        Err(ManagerError::ShutdownFailed { status, message })
    }

    /// The endpoint this supervisor is responsible for.
    ///
    /// Prefers the endpoint captured by `start()`; only falls back to the shared
    /// file when we never started or adopted one (e.g. `status()` on a fresh
    /// supervisor).
    fn current_endpoint(&self) -> Result<RouterEndpoint, ManagerError> {
        match self.active_endpoint.clone() {
            Some(endpoint) => Ok(endpoint),
            None => Ok(load_router_endpoint(&self.endpoint_file)?),
        }
    }

    pub async fn reload(&self) -> Result<(), ManagerError> {
        let endpoint = self.current_endpoint()?;
        self.request_reload(&endpoint).await
    }

    pub async fn shutdown(&self) -> Result<(), ManagerError> {
        let endpoint = self.current_endpoint()?;
        self.request_shutdown(&endpoint).await
    }

    pub async fn status(&mut self) -> Result<RouterStatus, ManagerError> {
        let last_error = self.last_error.clone();
        let Some(child) = self.child.as_mut() else {
            if self.reused_endpoint {
                let endpoint = match self.current_endpoint() {
                    Ok(endpoint) => endpoint,
                    Err(_) => {
                        return Ok(RouterStatus {
                            running: false,
                            healthy: false,
                            last_error,
                        });
                    }
                };
                let healthy = self.endpoint_is_healthy(&endpoint).await;
                return Ok(RouterStatus {
                    running: true,
                    healthy,
                    last_error,
                });
            }
            return Ok(RouterStatus {
                running: false,
                healthy: false,
                last_error,
            });
        };
        if child.try_wait()?.is_some() {
            self.child = None;
            self.owns_endpoint = false;
            self.reused_endpoint = false;
            self.active_endpoint = None;
            return Ok(RouterStatus {
                running: false,
                healthy: false,
                last_error,
            });
        }
        let endpoint = match self.current_endpoint() {
            Ok(endpoint) => endpoint,
            Err(_) => {
                return Ok(RouterStatus {
                    running: true,
                    healthy: false,
                    last_error,
                });
            }
        };
        let healthy = self.endpoint_is_healthy(&endpoint).await;
        Ok(RouterStatus {
            running: true,
            healthy,
            last_error,
        })
    }

    pub async fn stop(&mut self) -> Result<(), ManagerError> {
        let preserve_endpoint = self.reused_endpoint;
        // Only ever stop the Router we are actually responsible for.
        let owned_endpoint = self.active_endpoint.clone();
        if let Some(mut child) = self.child.take()
            && child.try_wait()?.is_none()
        {
            if let Some(endpoint) = owned_endpoint.as_ref() {
                let _ = self.request_shutdown(endpoint).await;
            }
            if timeout(SHUTDOWN_TIMEOUT, child.wait()).await.is_err() {
                child.kill().await?;
                let _ = child.wait().await;
            }
        }
        // Delete the endpoint file only if it still describes the Router we
        // owned. Otherwise we would erase an endpoint published by another
        // process that took over while we were stopping.
        if !preserve_endpoint && self.endpoint_file_still_ours(owned_endpoint.as_ref()) {
            remove_stale_endpoint(&self.endpoint_file)?;
        }
        self.owns_endpoint = false;
        self.reused_endpoint = false;
        self.active_endpoint = None;
        Ok(())
    }

    pub async fn restart(&mut self) -> Result<RouterEndpoint, ManagerError> {
        self.stop().await?;
        self.start().await
    }

    /// True when the shared endpoint file still contains `endpoint`, or when we
    /// have no endpoint to compare against and the file is simply ours to clear.
    fn endpoint_file_still_ours(&self, endpoint: Option<&RouterEndpoint>) -> bool {
        let Some(expected) = endpoint else {
            return true;
        };
        match load_router_endpoint(&self.endpoint_file) {
            Ok(current) => {
                current.base_url == expected.base_url
                    && current.capability_token.expose_secret()
                        == expected.capability_token.expose_secret()
            }
            // The file is gone or unreadable; nothing of another owner's to lose.
            Err(_) => true,
        }
    }
}

impl Drop for RouterSupervisor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // SIGKILL the child, then reap it: without the wait, a long-lived
            // host (the web server path) accumulates zombies.
            let _ = child.start_kill();
            let _ = child.try_wait();
        }
        if self.owns_endpoint && self.endpoint_file_still_ours(self.active_endpoint.as_ref()) {
            let _ = std::fs::remove_file(&self.endpoint_file);
        }
    }
}

/// Best-effort check that a loopback TCP port is free.
///
/// Only used to produce a clear error before spawning; the Router's own bind is
/// still the authority, and a small race here just means the Router reports the
/// conflict itself.
fn port_is_available(port: u16) -> bool {
    std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).is_ok()
}

fn sanitize_router_startup_diagnostic(raw: &[u8]) -> String {
    let mut lines = Vec::new();
    for line in String::from_utf8_lossy(raw).lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let lower = line.to_ascii_lowercase();
        if ["token", "authorization", "api-key", "api_key", "secret"]
            .iter()
            .any(|marker| lower.contains(marker))
        {
            lines.push("[sensitive Router diagnostic redacted]".to_owned());
        } else {
            lines.push(line.to_owned());
        }
    }
    lines.join(" | ")
}

fn append_router_startup_diagnostic(reason: String, diagnostic: String) -> String {
    if diagnostic.is_empty() {
        reason
    } else {
        format!("{reason}; child stderr: {diagnostic}")
    }
}

fn remove_stale_endpoint(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn configure_background_process(command: &mut Command) {
    #[cfg(windows)]
    {
        // CREATE_NO_WINDOW keeps the Router hidden when launched by the
        // Windows panel or by a .cmd wrapper.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    #[cfg(not(windows))]
    let _ = command;
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_credentials::MemoryCredentialStore;
    use tempfile::tempdir;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn stop_removes_stale_endpoint() {
        let directory = tempdir().unwrap();
        let endpoint_file = directory.path().join("router-endpoint.json");
        std::fs::write(&endpoint_file, "stale").unwrap();
        let mut supervisor = RouterSupervisor::new(
            "/does/not/exist",
            directory.path().join("providers.json"),
            &endpoint_file,
        );

        supervisor.stop().await.unwrap();

        assert!(!endpoint_file.exists());
    }

    #[tokio::test]
    async fn reused_endpoint_survives_supervisor_stop() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let responder = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let body = br#"{"instance":"omnibridge","registry_generation":1}"#;
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(body).await.unwrap();
        });

        let directory = tempdir().unwrap();
        let endpoint_file = directory.path().join("router-endpoint.json");
        std::fs::write(
            &endpoint_file,
            serde_json::json!({
                "schema_version": codex_mp_router::ROUTER_ENDPOINT_SCHEMA_VERSION,
                "base_url": format!("http://{address}"),
                "capability_token": "test-token"
            })
            .to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&endpoint_file, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }
        let mut supervisor = RouterSupervisor::new(
            "/does/not/exist",
            directory.path().join("providers.json"),
            &endpoint_file,
        );

        supervisor.start().await.unwrap();
        supervisor.stop().await.unwrap();

        assert!(endpoint_file.exists());
        responder.await.unwrap();
    }

    /// Regression: `purge_provider_data` began with `load_registry()?`, so a
    /// registry whose JSON was broken by a hand-edit made `codex-mp uninstall`
    /// fail outright. The user was left with a hijacked Codex config, live
    /// credentials, and no CLI way out — the same trap N-65 fixed for a broken
    /// `config.toml`.
    ///
    /// When the registry is being removed anyway, an unreadable one must not block
    /// the purge: there are no known credential references to delete, and the file
    /// itself is what the user wants gone.
    #[tokio::test]
    async fn purge_provider_data_tolerates_an_unreadable_registry() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        std::fs::write(&registry_path, b"{\"providers\": [broken").unwrap();

        let store = Arc::new(MemoryCredentialStore::default());
        let manager = ProviderManager::with_credentials(registry_path.clone(), store.clone());

        let removed = manager
            .purge_provider_data(true)
            .await
            .expect("a corrupt registry must not block the purge that removes it");
        assert_eq!(removed, 0, "no references could be enumerated");
        assert!(
            !registry_path.exists(),
            "the unreadable registry must still be removed"
        );
    }

    /// The same call with `remove_registry = false` must keep failing loudly: the
    /// caller asked to preserve provider data, so an unreadable registry is a real
    /// error rather than something to delete.
    #[tokio::test]
    async fn purge_provider_data_still_fails_when_the_registry_is_kept() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        std::fs::write(&registry_path, b"{\"providers\": [broken").unwrap();

        let store = Arc::new(MemoryCredentialStore::default());
        let manager = ProviderManager::with_credentials(registry_path.clone(), store.clone());

        assert!(
            manager.purge_provider_data(false).await.is_err(),
            "an unreadable registry must be reported when it is not being removed"
        );
        assert!(registry_path.exists(), "the file must be left untouched");
    }

    #[tokio::test]
    async fn purge_provider_data_removes_registry_and_credentials() {
        let directory = tempdir().unwrap();
        let store = Arc::new(MemoryCredentialStore::default());
        let manager = ProviderManager::with_credentials(
            directory.path().join("providers.json"),
            store.clone(),
        );
        manager
            .add_provider(
                "NewAPI",
                "https://example.test/v1",
                ProviderProtocol::Responses,
                Some(SecretString::from("provider-secret")),
            )
            .unwrap();

        let removed = manager.purge_provider_data(true).await.unwrap();

        assert_eq!(removed, 1);
        assert!(!directory.path().join("providers.json").exists());
        assert!(matches!(
            store.get("provider:newapi"),
            Err(CredentialStoreError::NotFound(_))
        ));
    }

    /// Regression: `reload()`/`shutdown()` re-read the shared endpoint file, so a
    /// supervisor whose Router had been replaced under it would send
    /// `/admin/shutdown` to the *other* process and then delete its endpoint file.
    /// Actions must target the endpoint captured by `start()`.
    #[tokio::test]
    async fn shutdown_targets_the_endpoint_we_started_not_the_shared_file() {
        // Our endpoint: records whether it was asked to shut down.
        let ours = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let ours_addr = ours.local_addr().unwrap();
        let (ours_tx, mut ours_rx) = tokio::sync::mpsc::channel::<String>(4);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = ours.accept().await {
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).await.unwrap_or(0);
                let text = String::from_utf8_lossy(&request[..read]).to_string();
                let _ = ours_tx.send(text).await;
                let body = br#"{"instance":"omnibridge","registry_generation":1}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });

        // The impostor: whatever is in the shared file afterwards must be left alone.
        let other = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let other_addr = other.local_addr().unwrap();
        let (other_tx, mut other_rx) = tokio::sync::mpsc::channel::<String>(4);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = other.accept().await {
                let mut request = [0_u8; 2048];
                let read = stream.read(&mut request).await.unwrap_or(0);
                let _ = other_tx
                    .send(String::from_utf8_lossy(&request[..read]).to_string())
                    .await;
                let body = br#"{"instance":"omnibridge","registry_generation":1}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });

        let directory = tempdir().unwrap();
        let endpoint_file = directory.path().join("router-endpoint.json");
        let write_endpoint = |addr: std::net::SocketAddr, token: &str| {
            std::fs::write(
                &endpoint_file,
                serde_json::json!({
                    "schema_version": codex_mp_router::ROUTER_ENDPOINT_SCHEMA_VERSION,
                    "base_url": format!("http://{addr}"),
                    "capability_token": token
                })
                .to_string(),
            )
            .unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&endpoint_file, std::fs::Permissions::from_mode(0o600))
                    .unwrap();
            }
        };

        write_endpoint(ours_addr, "our-token");
        let mut supervisor = RouterSupervisor::new(
            "/does/not/exist",
            directory.path().join("providers.json"),
            &endpoint_file,
        );
        // Adopt our Router (it is healthy and identifies as OmniBridge). This
        // performs a `GET /readyz`, so drain that before asserting on shutdown.
        supervisor.start().await.unwrap();
        let readiness = ours_rx.try_recv().expect("start() probes /readyz");
        assert!(readiness.starts_with("GET /readyz"), "{readiness}");

        // Another process takes over the shared endpoint file.
        write_endpoint(other_addr, "other-token");
        // Point our supervisor at an endpoint it believes it owns but which is
        // now a different Router.
        supervisor.set_active_endpoint_for_test(RouterEndpoint {
            base_url: format!("http://{ours_addr}"),
            capability_token: SecretString::from("our-token"),
        });

        supervisor.shutdown().await.unwrap();

        let ours_request = ours_rx.try_recv().expect("our Router must be shut down");
        assert!(
            ours_request.contains("/admin/shutdown"),
            "unexpected request to our endpoint: {ours_request}"
        );
        assert!(
            other_rx.try_recv().is_err(),
            "shutdown reached a Router we do not own"
        );
        assert!(
            endpoint_file.exists(),
            "we deleted another owner's endpoint"
        );
    }

    /// Regression: a stale endpoint file (its Router was SIGKILLed, or the host
    /// rebooted) made `shutdown()` fail, which aborted `codex-mp uninstall` before
    /// it restored the Codex config. The user was left with a hijacked config and
    /// no working way to undo it. Shutting down an unreachable Router must report
    /// the failure to the caller so it can treat it as "already stopped".
    #[tokio::test]
    async fn shutdown_of_an_unreachable_router_reports_an_error() {
        let directory = tempdir().unwrap();
        let endpoint_file = directory.path().join("router-endpoint.json");
        // A port nothing is listening on.
        std::fs::write(
            &endpoint_file,
            serde_json::json!({
                "schema_version": codex_mp_router::ROUTER_ENDPOINT_SCHEMA_VERSION,
                "base_url": "http://127.0.0.1:1",
                "capability_token": "dead-token"
            })
            .to_string(),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&endpoint_file, std::fs::Permissions::from_mode(0o600))
                .unwrap();
        }

        let supervisor = RouterSupervisor::new(
            "/does/not/exist",
            directory.path().join("providers.json"),
            &endpoint_file,
        );
        // The caller (uninstall) distinguishes this from a real shutdown and
        // continues; the important part is that it is an `Err`, not a hang.
        assert!(
            supervisor.shutdown().await.is_err(),
            "an unreachable router must be reported as a failed shutdown"
        );
    }

    /// Regression: `edit_provider` and `set_model_enabled` mutated routing state
    /// through `provider_mut`/`iter_mut` and saved, but never bumped the registry
    /// generation. A running Router caches resolved credentials per generation and
    /// only clears that cache when the generation *changes*, so editing a
    /// provider's API key kept resolving to the previous secret until the Router
    /// was restarted.
    #[test]
    fn every_routing_mutation_bumps_the_registry_generation() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let manager = ProviderManager::with_credentials(
            &registry_path,
            Arc::new(codex_mp_credentials::MemoryCredentialStore::default()),
        );

        let added = manager
            .add_provider(
                "Probe",
                "https://example.test/v1",
                ProviderProtocol::Responses,
                Some(SecretString::from("sk-one")),
            )
            .unwrap();
        manager
            .add_model(
                &added.id,
                "m1",
                "M1",
                None,
                codex_mp_core::ModelCapabilities::default(),
            )
            .unwrap();

        let generation = || ProviderRegistry::load(&registry_path).unwrap().generation();
        let mut previous = generation();

        // Editing a provider must invalidate a running Router's credential cache.
        manager
            .edit_provider(
                &added.id,
                None,
                None,
                None,
                None,
                Some(SecretString::from("sk-two")),
            )
            .unwrap();
        let after_edit = generation();
        assert!(
            after_edit > previous,
            "edit_provider must bump the generation ({previous} -> {after_edit})"
        );
        previous = after_edit;

        // Toggling a model changes the enabled set the Router routes on.
        manager.set_model_enabled("probe/m1", false).unwrap();
        let after_toggle = generation();
        assert!(
            after_toggle > previous,
            "set_model_enabled must bump the generation ({previous} -> {after_toggle})"
        );
    }

    /// A store whose reads fail but whose other operations succeed, so a test can
    /// tell "the read failed" apart from "there is no previous secret".
    struct FailingReadStore {
        deletes: Arc<std::sync::Mutex<Vec<String>>>,
    }

    impl CredentialStore for FailingReadStore {
        fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
            Err(CredentialStoreError::Backend(format!(
                "keyring unavailable for {reference}"
            )))
        }
        fn set(&self, _reference: &str, _value: &SecretString) -> Result<(), CredentialStoreError> {
            Ok(())
        }
        fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
            self.deletes.lock().unwrap().push(reference.to_owned());
            Ok(())
        }
    }

    /// Regression: `save_provider_change` used `credentials.get(reference).ok()`,
    /// which collapses "no previous secret" and "the read failed" into `None`. The
    /// rollback path deletes the credential when it sees `None`, so a transient
    /// keyring failure followed by a failed save destroyed a still-valid key.
    /// A failed read must abort the operation instead.
    #[test]
    fn a_failed_credential_read_does_not_delete_the_stored_key() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let deletes = Arc::new(std::sync::Mutex::new(Vec::new()));
        let manager = ProviderManager::with_credentials(
            &registry_path,
            Arc::new(FailingReadStore {
                deletes: deletes.clone(),
            }),
        );

        let result = manager.add_provider(
            "Probe",
            "https://example.test/v1",
            ProviderProtocol::Responses,
            Some(SecretString::from("sk-probe")),
        );

        assert!(
            result.is_err(),
            "a failed credential read must abort the provider change"
        );
        assert!(
            deletes.lock().unwrap().is_empty(),
            "a failed read must never trigger a credential deletion: {:?}",
            deletes.lock().unwrap()
        );
    }

    /// Regression: `ProviderManager`'s mutating methods used `load_registry()` +
    /// `save()`, an unlocked read-modify-write. Concurrent edits (the web panel
    /// and a CLI invocation are both normal) could each read revision N and both
    /// replace it, silently dropping one change.
    #[test]
    fn concurrent_manager_edits_do_not_lose_a_model() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let store = Arc::new(MemoryCredentialStore::default());
        let manager = ProviderManager::with_credentials(&registry_path, store.clone());
        manager
            .add_provider(
                "M",
                "https://example.test/v1",
                ProviderProtocol::Responses,
                None,
            )
            .unwrap();

        let mut handles = Vec::new();
        for index in 0..8 {
            let registry_path = registry_path.clone();
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                let manager = ProviderManager::with_credentials(&registry_path, store);
                manager
                    .add_model(
                        "m",
                        &format!("model-{index}"),
                        &format!("Model {index}"),
                        None,
                        ModelCapabilities::default(),
                    )
                    .unwrap();
            }));
        }
        for handle in handles {
            handle.join().unwrap();
        }

        let registry = ProviderRegistry::load(&registry_path).unwrap();
        let models = registry
            .provider("m")
            .map(|provider| provider.models.len())
            .unwrap_or(0);
        assert_eq!(
            models, 8,
            "a model was lost to a concurrent read-modify-write"
        );
    }

    /// Regression: `add_provider` and `remove_provider` were the two mutators
    /// still using the **unlocked** `load_registry()`. 20 simultaneous HTTP
    /// `providers/add` calls all returned 200 while only one provider landed.
    /// Every mutator must hold the cross-process lock across its read-modify-write.
    #[test]
    fn concurrent_provider_adds_do_not_lose_a_provider() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let store = Arc::new(MemoryCredentialStore::default());

        const THREADS: usize = 12;
        let mut handles = Vec::new();
        for index in 0..THREADS {
            let registry_path = registry_path.clone();
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                let manager = ProviderManager::with_credentials(&registry_path, store);
                manager
                    .add_provider(
                        &format!("P{index}"),
                        &format!("https://e{index}.test/v1"),
                        ProviderProtocol::Responses,
                        None,
                    )
                    .is_ok()
            }));
        }
        for handle in handles {
            assert!(handle.join().expect("a worker thread panicked"));
        }

        let registry = ProviderRegistry::load(&registry_path).unwrap();
        assert_eq!(
            registry.providers().len(),
            THREADS,
            "a provider was lost to a concurrent unlocked read-modify-write"
        );
    }

    /// Same as `concurrent_provider_adds_do_not_lose_a_provider`, but with the
    /// process's real credential store — the web server uses `ProviderManager::new`,
    /// which is keyring-backed and therefore much slower per write. A slow store
    /// widens any window between "read" and "write", so it is the configuration
    /// that actually reproduces the panel's lost update.
    ///
    /// Ignored by default: it touches the developer's real keyring. Run with
    /// `cargo test -p codex-mp-manager -- --ignored concurrent_provider_adds_with_keyring`.
    #[test]
    #[ignore = "touches the real OS keyring"]
    fn concurrent_provider_adds_with_keyring_do_not_lose_a_provider() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let store = Arc::new(codex_mp_credentials::NativeCredentialStore::default());

        const THREADS: usize = 12;
        let mut handles = Vec::new();
        for index in 0..THREADS {
            let registry_path = registry_path.clone();
            let store = store.clone();
            handles.push(std::thread::spawn(move || {
                let manager = ProviderManager::with_credentials(&registry_path, store.clone());
                let result = manager.add_provider(
                    &format!("K{index}"),
                    &format!("https://k{index}.test/v1"),
                    ProviderProtocol::Responses,
                    Some(SecretString::from(format!("sk-{index}"))),
                );
                let _ = store.delete(&format!("provider:k{index}"));
                result
            }));
        }
        for handle in handles {
            let _ = handle.join().expect("a worker thread panicked");
        }

        let registry = ProviderRegistry::load(&registry_path).unwrap();
        assert_eq!(
            registry.providers().len(),
            THREADS,
            "a provider was lost with the keyring-backed store"
        );
    }

    /// Reproduces the web server's exact execution model: mutations run on
    /// `spawn_blocking` workers, which are **reused pooled threads**. A
    /// `FileLock` reentrancy registry keyed on `thread_local` state interacts
    /// with that reuse, so the loss the panel showed (5 concurrent `providers/add`
    /// all returning 200 with one missing) may only appear here.
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_provider_adds_on_blocking_pool_do_not_lose_a_provider() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let store = Arc::new(MemoryCredentialStore::default());

        const TASKS: usize = 12;
        let mut handles = Vec::new();
        for index in 0..TASKS {
            let registry_path = registry_path.clone();
            let store = store.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let manager = ProviderManager::with_credentials(&registry_path, store);
                manager
                    .add_provider(
                        &format!("B{index}"),
                        &format!("https://b{index}.test/v1"),
                        ProviderProtocol::Responses,
                        None,
                    )
                    .expect("concurrent add_provider must succeed");
            }));
        }
        for handle in handles {
            handle.await.expect("a blocking task panicked");
        }

        let registry = ProviderRegistry::load(&registry_path).unwrap();
        assert_eq!(
            registry.providers().len(),
            TASKS,
            "a provider was lost on the blocking pool"
        );
    }

    /// Heavy contention on the blocking pool: 40 simultaneous `add_provider` calls
    /// across a small worker pool. This is the shape that exposed the
    /// `thread_local` reentrancy bug (a guard dropped on a different pooled thread
    /// left a phantom "already held" entry and silently skipped locking).
    ///
    /// A small pool is deliberate: it maximises thread reuse and therefore the
    /// number of times a guard is dropped by a thread other than its acquirer.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn heavy_blocking_pool_contention_loses_nothing() {
        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let store = Arc::new(MemoryCredentialStore::default());

        const TASKS: usize = 40;
        let mut handles = Vec::new();
        for index in 0..TASKS {
            let registry_path = registry_path.clone();
            let store = store.clone();
            handles.push(tokio::task::spawn_blocking(move || {
                let manager = ProviderManager::with_credentials(&registry_path, store);
                manager
                    .add_provider(
                        &format!("H{index}"),
                        &format!("https://h{index}.test/v1"),
                        ProviderProtocol::Responses,
                        None,
                    )
                    .is_ok()
            }));
        }
        for handle in handles {
            assert!(handle.await.expect("a blocking task panicked"));
        }

        let registry = ProviderRegistry::load(&registry_path).unwrap();
        assert_eq!(
            registry.providers().len(),
            TASKS,
            "providers were lost under heavy blocking-pool contention"
        );
    }

    /// Regression: the Router must bind the port `config.toml` advertises, and an
    /// occupied port must fail immediately with an actionable message rather than
    /// a silent startup timeout. (`--port 0` previously hid this entirely by always
    /// finding a free port nobody was told about.)
    #[tokio::test]
    async fn an_occupied_configured_port_fails_fast_and_explains_itself() {
        // Hold a loopback port, then point a supervisor at it.
        let holder = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = holder.local_addr().unwrap().port();

        let directory = tempdir().unwrap();
        let mut supervisor = RouterSupervisor::new(
            "/does/not/exist",
            directory.path().join("providers.json"),
            directory.path().join("router-endpoint.json"),
        )
        .with_port(port);

        let started = std::time::Instant::now();
        let error = supervisor
            .start()
            .await
            .expect_err("an occupied port must be reported");
        assert!(
            matches!(error, ManagerError::PortInUse { port: reported } if reported == port),
            "unexpected error: {error}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the conflict should be detected before spawning, not by timing out"
        );
        drop(holder);
    }

    /// The lock must actually serialize concurrent startups.
    #[tokio::test]
    async fn startup_lock_is_exclusive_and_reclaims_stale_holders() {
        let directory = tempdir().unwrap();
        let endpoint_file = directory.path().join("router-endpoint.json");
        let lock_path = lock_path_for(&endpoint_file);

        let first = StartupLock::acquire(&endpoint_file).await.unwrap();
        assert!(lock_path.exists());
        // A second acquire must not succeed while the first is held.
        let blocked = tokio::time::timeout(
            Duration::from_millis(300),
            StartupLock::acquire(&endpoint_file),
        )
        .await;
        assert!(
            blocked.is_err(),
            "a second lock was granted while the first was held"
        );

        drop(first);
        assert!(!lock_path.exists(), "the lock file was not released");

        // A lock naming a dead PID must be reclaimed instead of blocking for ever.
        std::fs::write(&lock_path, "999999999").unwrap();
        let reclaimed =
            tokio::time::timeout(Duration::from_secs(2), StartupLock::acquire(&endpoint_file))
                .await
                .expect("a stale lock was never reclaimed")
                .expect("reclaim failed");
        drop(reclaimed);
    }

    /// An unreadable registry must not make any generation look healthy.
    #[tokio::test]
    async fn unreadable_registry_is_never_healthy() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                let body = br#"{"instance":"omnibridge","registry_generation":7}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });

        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        // Deliberately corrupt so `ProviderRegistry::load` fails.
        std::fs::write(&registry_path, "{ this is not json").unwrap();

        let supervisor = RouterSupervisor::new(
            "/does/not/exist",
            &registry_path,
            directory.path().join("router-endpoint.json"),
        );
        let endpoint = RouterEndpoint {
            base_url: format!("http://{address}"),
            capability_token: SecretString::from("token"),
        };
        assert_eq!(
            supervisor.endpoint_health(&endpoint).await,
            EndpointHealth::Unreachable,
            "a Router was accepted as healthy while the local registry was unreadable"
        );
    }

    /// A Router built from an older registry must be classified as stale (and thus
    /// reloaded) rather than replaced.
    #[tokio::test]
    async fn generation_mismatch_is_reported_as_stale() {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut request = [0_u8; 1024];
                let _ = stream.read(&mut request).await;
                let body = br#"{"instance":"omnibridge","registry_generation":1}"#;
                let header = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(header.as_bytes()).await;
                let _ = stream.write_all(body).await;
            }
        });

        let directory = tempdir().unwrap();
        let registry_path = directory.path().join("providers.json");
        let mut registry = ProviderRegistry::empty(&registry_path);
        registry
            .add_provider(ProviderConfig::new("NewAPI", "https://example.test/v1").unwrap())
            .unwrap();
        registry.save().unwrap();

        let supervisor = RouterSupervisor::new(
            "/does/not/exist",
            &registry_path,
            directory.path().join("router-endpoint.json"),
        );
        let endpoint = RouterEndpoint {
            base_url: format!("http://{address}"),
            capability_token: SecretString::from("token"),
        };
        // The fixture always reports generation 1; the saved registry is past that.
        assert_ne!(
            supervisor.endpoint_health(&endpoint).await,
            EndpointHealth::Current
        );
    }
}
