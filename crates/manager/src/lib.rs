//! Lifecycle management for the local Router process.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use codex_mp_core::{
    CoreError, CustomModel, ModelCapabilities, ModelEdit, ProviderConfig, ProviderProtocol,
    ProviderRegistry,
};
use codex_mp_credentials::{CredentialStore, CredentialStoreError, NativeCredentialStore};
use codex_mp_router::{RouterEndpoint, RouterError, load_router_endpoint};
use reqwest::Client;
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};

const STARTUP_TIMEOUT: Duration = Duration::from_secs(5);
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

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
    pub enabled: bool,
    pub models: Vec<ModelSummary>,
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
            http_client: Client::new(),
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
        let mut registry = self.load_registry()?;
        let previous = registry.clone();
        let mut provider = ProviderConfig::new(name, base_url)?;
        provider.protocol = protocol;
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
        let mut registry = self.load_registry()?;
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
        let reference = provider.credential_reference.clone();
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
        let mut registry = self.load_registry()?;
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
        let key = self.credentials.get(&provider.credential_reference)?;
        let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
        let response = self
            .http_client
            .get(url)
            .bearer_auth(key.expose_secret())
            .send()
            .await?;
        let status = response.status();
        let body = response.text().await?;
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
        let mut registry = self.load_registry()?;
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
        let mut registry = self.load_registry()?;
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
        let mut registry = self.load_registry()?;
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
        let mut registry = self.load_registry()?;
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
        registry.save()?;
        Ok(summary)
    }

    pub fn remove_model(&self, logical_model_id: &str) -> Result<(), ProviderManagerError> {
        let mut registry = self.load_registry()?;
        registry.remove_model(logical_model_id)?;
        registry.save()?;
        Ok(())
    }

    pub fn purge_provider_data(
        &self,
        remove_registry: bool,
    ) -> Result<usize, ProviderManagerError> {
        let registry = self.load_registry()?;
        let references: BTreeSet<String> = registry
            .providers()
            .iter()
            .map(|provider| provider.credential_reference.clone())
            .collect();
        let mut credentials = Vec::new();
        for reference in references {
            match self.credentials.get(&reference) {
                Ok(secret) => credentials.push((reference, secret)),
                Err(CredentialStoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }

        let mut deleted = Vec::new();
        for (reference, secret) in &credentials {
            if let Err(error) = self.credentials.delete(reference) {
                restore_deleted_credentials(&*self.credentials, &deleted)?;
                return Err(error.into());
            }
            deleted.push((reference.clone(), secret.clone()));
        }

        if remove_registry
            && self.registry_path.exists()
            && let Err(error) = std::fs::remove_file(&self.registry_path)
        {
            restore_deleted_credentials(&*self.credentials, &deleted)?;
            return Err(CoreError::Io(error).into());
        }
        Ok(deleted.len())
    }

    fn load_registry(&self) -> Result<ProviderRegistry, ProviderManagerError> {
        Ok(ProviderRegistry::load(&self.registry_path)?)
    }

    fn save_provider_change(
        &self,
        registry: &ProviderRegistry,
        previous: &ProviderRegistry,
        reference: &str,
        secret: Option<&SecretString>,
    ) -> Result<(), ProviderManagerError> {
        let previous_secret = if secret.is_some() {
            self.credentials.get(reference).ok()
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

fn restore_deleted_credentials(
    store: &dyn CredentialStore,
    deleted: &[(String, SecretString)],
) -> Result<(), ProviderManagerError> {
    for (reference, secret) in deleted {
        store.set(reference, secret)?;
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum ManagerError {
    #[error("router process error: {0}")]
    Process(#[from] std::io::Error),
    #[error("router endpoint error: {0}")]
    Endpoint(#[from] RouterError),
    #[error("router startup timed out waiting for `{0}`")]
    StartupTimeout(PathBuf),
    #[error("router request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("router reload returned HTTP {status}: {message}")]
    ReloadFailed { status: u16, message: String },
    #[error("router shutdown returned HTTP {status}: {message}")]
    ShutdownFailed { status: u16, message: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouterStatus {
    pub running: bool,
    pub healthy: bool,
}

pub struct RouterSupervisor {
    executable: PathBuf,
    registry_path: PathBuf,
    endpoint_file: PathBuf,
    client: Client,
    child: Option<Child>,
    owns_endpoint: bool,
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
            client: Client::new(),
            child: None,
            owns_endpoint: false,
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

    pub async fn start(&mut self) -> Result<RouterEndpoint, ManagerError> {
        if let Some(child) = self.child.as_mut()
            && child.try_wait()?.is_none()
        {
            return Ok(load_router_endpoint(&self.endpoint_file)?);
        }
        self.child = None;
        self.owns_endpoint = false;
        remove_stale_endpoint(&self.endpoint_file)?;

        let mut child = Command::new(&self.executable);
        child
            .arg("--registry")
            .arg(&self.registry_path)
            .arg("router")
            .arg("--port")
            .arg("0")
            .arg("--endpoint-file")
            .arg(&self.endpoint_file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        self.child = Some(child.spawn()?);
        let endpoint_file = self.endpoint_file.clone();
        let endpoint = timeout(STARTUP_TIMEOUT, async {
            loop {
                if let Ok(endpoint) = load_router_endpoint(&endpoint_file) {
                    return Ok(endpoint);
                }
                if let Some(child) = self.child.as_mut()
                    && child.try_wait()?.is_some()
                {
                    return Err(ManagerError::StartupTimeout(endpoint_file.clone()));
                }
                sleep(POLL_INTERVAL).await;
            }
        })
        .await
        .map_err(|_| ManagerError::StartupTimeout(self.endpoint_file.clone()))??;
        self.owns_endpoint = true;
        Ok(endpoint)
    }

    pub async fn reload(&self) -> Result<(), ManagerError> {
        let endpoint = load_router_endpoint(&self.endpoint_file)?;
        let response = self
            .client
            .post(format!("{}/admin/reload", endpoint.base_url))
            .bearer_auth(endpoint.capability_token.expose_secret())
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();
        Err(ManagerError::ReloadFailed { status, message })
    }

    pub async fn shutdown(&self) -> Result<(), ManagerError> {
        let endpoint = load_router_endpoint(&self.endpoint_file)?;
        let response = self
            .client
            .post(format!("{}/admin/shutdown", endpoint.base_url))
            .bearer_auth(endpoint.capability_token.expose_secret())
            .send()
            .await?;
        if response.status().is_success() {
            return Ok(());
        }
        let status = response.status().as_u16();
        let message = response.text().await.unwrap_or_default();
        Err(ManagerError::ShutdownFailed { status, message })
    }

    pub async fn status(&mut self) -> Result<RouterStatus, ManagerError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(RouterStatus {
                running: false,
                healthy: false,
            });
        };
        if child.try_wait()?.is_some() {
            self.child = None;
            self.owns_endpoint = false;
            return Ok(RouterStatus {
                running: false,
                healthy: false,
            });
        }
        let endpoint = match load_router_endpoint(&self.endpoint_file) {
            Ok(endpoint) => endpoint,
            Err(_) => {
                return Ok(RouterStatus {
                    running: true,
                    healthy: false,
                });
            }
        };
        let healthy = self
            .client
            .get(format!("{}/healthz", endpoint.base_url))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success());
        Ok(RouterStatus {
            running: true,
            healthy,
        })
    }

    pub async fn stop(&mut self) -> Result<(), ManagerError> {
        if let Some(mut child) = self.child.take()
            && child.try_wait()?.is_none()
        {
            let _ = self.shutdown().await;
            if timeout(SHUTDOWN_TIMEOUT, child.wait()).await.is_err() {
                child.kill().await?;
                let _ = child.wait().await;
            }
        }
        remove_stale_endpoint(&self.endpoint_file)?;
        self.owns_endpoint = false;
        Ok(())
    }
}

impl Drop for RouterSupervisor {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.start_kill();
        }
        if self.owns_endpoint {
            let _ = std::fs::remove_file(&self.endpoint_file);
        }
    }
}

fn remove_stale_endpoint(path: &Path) -> Result<(), std::io::Error> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use codex_mp_credentials::MemoryCredentialStore;
    use tempfile::tempdir;

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

    #[test]
    fn purge_provider_data_removes_registry_and_credentials() {
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

        let removed = manager.purge_provider_data(true).unwrap();

        assert_eq!(removed, 1);
        assert!(!directory.path().join("providers.json").exists());
        assert!(matches!(
            store.get("provider:newapi"),
            Err(CredentialStoreError::NotFound(_))
        ));
    }
}
