use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use codex_mp_catalog::{default_catalog_path, discover_official_catalog, merge_catalog};
use codex_mp_core::{
    CustomModel, ModelEdit, ProviderConfig, ProviderProtocol, ProviderRegistry,
    default_registry_path,
};
use codex_mp_credentials::{CredentialStore, CredentialStoreError, NativeCredentialStore};
use codex_mp_integration::{IntegrationPaths, build_and_install, repair, restore_if_present};
use codex_mp_manager::RouterSupervisor;
use codex_mp_router::{
    RouterConfig, RouterState, default_router_endpoint_path, new_capability_token, serve,
};
use secrecy::SecretString;

#[derive(Debug, Parser)]
#[command(
    name = "codex-mp",
    version,
    about = "Codex MultiProvider: secure provider registry and loopback model router"
)]
struct Cli {
    #[arg(long, env = "CODEX_MP_REGISTRY")]
    registry: Option<PathBuf>,
    #[arg(long, env = "CODEX_BIN", default_value = "codex")]
    codex_bin: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Provider {
        #[command(subcommand)]
        command: ProviderCommand,
    },
    Model {
        #[command(subcommand)]
        command: ModelCommand,
    },
    Models,
    Sync,
    Repair,
    /// Restore the Codex config and remove the generated catalog.
    Restore,
    /// Stop local services and remove MultiProvider-owned state safely.
    Uninstall(UninstallArgs),
    Status,
    Router(RouterArgs),
    Manager(ManagerArgs),
}

#[derive(Debug, Subcommand)]
enum ProviderCommand {
    Add(AddProviderArgs),
    List,
    Edit(EditProviderArgs),
    Remove(RemoveProviderArgs),
    FetchModels(FetchModelsArgs),
}

#[derive(Debug, Subcommand)]
enum ModelCommand {
    Add(AddModelArgs),
    Edit(EditModelArgs),
    Remove { logical_model_id: String },
    Enable { logical_model_id: String },
    Disable { logical_model_id: String },
}

#[derive(Debug, Args)]
struct AddProviderArgs {
    name: String,
    base_url: String,
    #[arg(long, value_enum, default_value_t = ProtocolArg::Responses)]
    protocol: ProtocolArg,
    /// Read the API key from stdin. It is never written to the registry.
    #[arg(long, conflicts_with = "api_key_env")]
    api_key_stdin: bool,
    /// Read the API key from the named environment variable.
    #[arg(long, conflicts_with = "api_key_stdin")]
    api_key_env: Option<String>,
}

#[derive(Debug, Args)]
struct EditProviderArgs {
    id: String,
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    base_url: Option<String>,
    #[arg(long, value_enum)]
    protocol: Option<ProtocolArg>,
    #[arg(long)]
    enable: bool,
    #[arg(long)]
    disable: bool,
    #[arg(long, conflicts_with = "api_key_env")]
    api_key_stdin: bool,
    #[arg(long, conflicts_with = "api_key_stdin")]
    api_key_env: Option<String>,
}

#[derive(Debug, Args)]
struct RemoveProviderArgs {
    id: String,
    #[arg(long)]
    purge_credential: bool,
}

#[derive(Debug, Args)]
struct AddModelArgs {
    provider_id: String,
    upstream_model_id: String,
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long)]
    context_window: Option<u64>,
    #[arg(long)]
    no_tools: bool,
    #[arg(long)]
    images: bool,
}

#[derive(Debug, Args)]
struct EditModelArgs {
    logical_model_id: String,
    #[arg(long)]
    display_name: Option<String>,
    #[arg(long, conflicts_with = "clear_context_window")]
    context_window: Option<u64>,
    #[arg(long)]
    clear_context_window: bool,
}

#[derive(Debug, Args)]
struct FetchModelsArgs {
    id: String,
    #[arg(long = "add", value_name = "MODEL_ID")]
    add: Vec<String>,
    #[arg(long, conflicts_with = "add")]
    all: bool,
}

#[derive(Debug, Args)]
struct RouterArgs {
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Secure endpoint file read by the patched Core dispatcher.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ManagerArgs {
    /// Secure endpoint file shared with the patched Codex process.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct UninstallArgs {
    /// Keep the Provider registry and keyring entries for a later reinstall.
    #[arg(long)]
    keep_provider_data: bool,
    /// Endpoint file used by an already running panel/manager.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ProtocolArg {
    Responses,
    ChatCompletions,
}

impl From<ProtocolArg> for ProviderProtocol {
    fn from(value: ProtocolArg) -> Self {
        match value {
            ProtocolArg::Responses => Self::Responses,
            ProtocolArg::ChatCompletions => Self::ChatCompletions,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter("codex_mp=info")
        .with_target(false)
        .init();
    let cli = Cli::parse();
    let registry_path = cli.registry.unwrap_or_else(default_registry_path);
    match cli.command {
        Command::Provider { command } => provider_command(&registry_path, command).await,
        Command::Model { command } => model_command(&registry_path, command),
        Command::Models => list_models(&registry_path, &cli.codex_bin),
        Command::Sync => sync(&registry_path, &cli.codex_bin),
        Command::Repair => repair_integration(&registry_path, &cli.codex_bin),
        Command::Restore => restore_integration(),
        Command::Uninstall(args) => uninstall(&registry_path, args).await,
        Command::Status => status(&registry_path, &cli.codex_bin),
        Command::Router(args) => run_router(&registry_path, args).await,
        Command::Manager(args) => run_manager(&registry_path, args).await,
    }
}

async fn provider_command(path: &PathBuf, command: ProviderCommand) -> Result<()> {
    match command {
        ProviderCommand::Add(args) => {
            let secret = read_secret(args.api_key_stdin, args.api_key_env)?;
            let mut registry = ProviderRegistry::load(path)?;
            let previous_registry = registry.clone();
            let mut provider = ProviderConfig::new(&args.name, &args.base_url)?;
            provider.protocol = args.protocol.into();
            let reference = provider.credential_reference.clone();
            registry.add_provider(provider)?;
            save_provider_change(
                &registry,
                &previous_registry,
                &NativeCredentialStore::default(),
                &reference,
                secret.as_ref(),
            )?;
            println!(
                "added provider `{reference}` metadata at {}",
                path.display()
            );
        }
        ProviderCommand::List => {
            let registry = ProviderRegistry::load(path)?;
            if registry.providers().is_empty() {
                println!("No providers configured.");
            } else {
                for provider in registry.providers() {
                    println!(
                        "{}\t{}\t{}\t{} models\t{}",
                        provider.id,
                        provider.name,
                        provider.base_url,
                        provider.models.len(),
                        if provider.enabled {
                            "enabled"
                        } else {
                            "disabled"
                        }
                    );
                }
            }
        }
        ProviderCommand::Edit(args) => {
            if args.enable && args.disable {
                bail!("--enable and --disable are mutually exclusive");
            }
            let secret = read_secret(args.api_key_stdin, args.api_key_env)?;
            let mut registry = ProviderRegistry::load(path)?;
            let previous_registry = registry.clone();
            let provider = registry
                .provider_mut(&args.id)
                .with_context(|| format!("provider `{}` was not found", args.id))?;
            if let Some(name) = args.name {
                provider.name = name;
            }
            if let Some(base_url) = args.base_url {
                provider.base_url = codex_mp_core::normalize_base_url(&base_url)?;
            }
            if let Some(protocol) = args.protocol {
                provider.protocol = protocol.into();
            }
            if args.enable {
                provider.enabled = true;
            }
            if args.disable {
                provider.enabled = false;
            }
            let reference = provider.credential_reference.clone();
            save_provider_change(
                &registry,
                &previous_registry,
                &NativeCredentialStore::default(),
                &reference,
                secret.as_ref(),
            )?;
            println!("updated provider `{}`", args.id);
        }
        ProviderCommand::Remove(args) => {
            let mut registry = ProviderRegistry::load(path)?;
            let previous_registry = registry.clone();
            let provider = registry.remove_provider(&args.id)?;
            let store = NativeCredentialStore::default();
            let previous_secret = if args.purge_credential {
                delete_credential_if_present(&store, &provider.credential_reference)?
            } else {
                None
            };
            if let Err(error) = registry.save() {
                let registry_rollback = previous_registry.save();
                let credential_rollback: Result<()> =
                    previous_secret.as_ref().map_or(Ok(()), |secret| {
                        store
                            .set(&provider.credential_reference, secret)
                            .map_err(anyhow::Error::new)
                    });
                return combine_rollback_errors(
                    error.into(),
                    registry_rollback,
                    credential_rollback,
                    "provider removal failed",
                );
            }
            println!("removed provider `{}`", args.id);
        }
        ProviderCommand::FetchModels(args) => fetch_models(path, args).await?,
    }
    Ok(())
}

fn model_command(path: &PathBuf, command: ModelCommand) -> Result<()> {
    let mut registry = ProviderRegistry::load(path)?;
    match command {
        ModelCommand::Add(args) => {
            let provider = registry
                .provider(&args.provider_id)
                .with_context(|| format!("provider `{}` was not found", args.provider_id))?;
            let display_name = args
                .display_name
                .unwrap_or_else(|| format!("{} / {}", provider.name, args.upstream_model_id));
            let mut model =
                CustomModel::new(&args.provider_id, &args.upstream_model_id, &display_name)?;
            model.context_window = args.context_window;
            model.capabilities.tools = !args.no_tools;
            model.capabilities.images = args.images;
            registry.add_model(model.clone())?;
            registry.save()?;
            println!("added model `{}`", model.logical_model_id);
        }
        ModelCommand::Edit(args) => {
            if args.display_name.is_none()
                && args.context_window.is_none()
                && !args.clear_context_window
            {
                bail!(
                    "model edit requires --display-name, --context-window, or --clear-context-window"
                );
            }
            let logical_model_id = args.logical_model_id.clone();
            registry.edit_model(
                &logical_model_id,
                ModelEdit {
                    display_name: args.display_name,
                    context_window: if args.clear_context_window {
                        Some(None)
                    } else {
                        args.context_window.map(Some)
                    },
                },
            )?;
            registry.save()?;
            println!("updated model `{logical_model_id}`");
        }
        ModelCommand::Remove { logical_model_id } => {
            registry.remove_model(&logical_model_id)?;
            registry.save()?;
            println!("removed model `{logical_model_id}`");
        }
        ModelCommand::Enable { logical_model_id } => {
            set_model_enabled(&mut registry, &logical_model_id, true)?
        }
        ModelCommand::Disable { logical_model_id } => {
            set_model_enabled(&mut registry, &logical_model_id, false)?
        }
    }
    Ok(())
}

fn set_model_enabled(
    registry: &mut ProviderRegistry,
    logical_model_id: &str,
    enabled: bool,
) -> Result<()> {
    let (provider_id, _) = logical_model_id
        .split_once('/')
        .context("custom model ids must be namespaced")?;
    let provider = registry
        .provider_mut(provider_id)
        .context("provider not found")?;
    let model = provider
        .models
        .iter_mut()
        .find(|m| m.logical_model_id == logical_model_id)
        .context("model not found")?;
    model.enabled = enabled;
    registry.save()?;
    println!(
        "{} `{logical_model_id}`",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn list_models(path: &PathBuf, codex_bin: &PathBuf) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let official = discover_official_catalog(codex_bin)?;
    let merged = merge_catalog(&official, &registry)?;
    for model in merged["models"]
        .as_array()
        .context("catalog models missing")?
    {
        println!(
            "{}\t{}",
            model["slug"].as_str().unwrap_or("?"),
            model["display_name"].as_str().unwrap_or("?")
        );
    }
    Ok(())
}

fn sync(path: &PathBuf, codex_bin: &PathBuf) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let paths = IntegrationPaths::default();
    let manifest = build_and_install(&paths, &registry, codex_bin)?;
    println!("merged catalog: {}", paths.catalog.display());
    println!(
        "managed Codex field: {} = {}",
        manifest.managed_field, manifest.applied_value
    );
    println!("Codex provider was not changed; OAuth credentials were not read.");
    Ok(())
}

fn repair_integration(path: &PathBuf, codex_bin: &PathBuf) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let paths = IntegrationPaths::default();
    let manifest = repair(&paths, &registry, codex_bin)?;
    println!(
        "repaired catalog integration for {}",
        manifest.codex_version.as_deref().unwrap_or("unknown Codex")
    );
    Ok(())
}

fn restore_integration() -> Result<()> {
    let paths = IntegrationPaths::default();
    if restore_if_present(&paths)? {
        println!("restored Codex config and removed the generated catalog");
    } else {
        println!("no MultiProvider integration manifest was found");
    }
    Ok(())
}

fn status(path: &std::path::Path, codex_bin: &std::path::Path) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    println!("registry: {}", path.display());
    println!("providers: {}", registry.providers().len());
    println!(
        "enabled custom models: {}",
        registry.enabled_custom_models().count()
    );
    println!("catalog: {}", default_catalog_path().display());
    println!("Codex: {}", codex_bin.display());
    println!("router bind policy: 127.0.0.1 only");
    println!("OAuth: not accessed by codex-mp");
    Ok(())
}

async fn run_router(path: &PathBuf, args: RouterArgs) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let endpoint_file = args
        .endpoint_file
        .unwrap_or_else(default_router_endpoint_path);
    let state = RouterState::with_capability_token(
        registry,
        Arc::new(NativeCredentialStore::default()),
        new_capability_token(),
    );
    println!(
        "router starting on loopback; endpoint file: {}",
        endpoint_file.display()
    );
    serve(
        RouterConfig {
            bind_ip: codex_mp_router::DEFAULT_BIND_IP,
            port: args.port,
            endpoint_file: Some(endpoint_file),
        },
        state,
    )
    .await?;
    Ok(())
}

async fn run_manager(path: &Path, args: ManagerArgs) -> Result<()> {
    let endpoint_file = args
        .endpoint_file
        .unwrap_or_else(default_router_endpoint_path);
    let executable = std::env::current_exe()?;
    let mut supervisor = RouterSupervisor::new(executable, path.to_path_buf(), &endpoint_file);
    let endpoint = supervisor.start().await?;
    println!("router started on {}", endpoint.base_url);
    println!("manager is running; press Ctrl-C to stop the Router");
    tokio::signal::ctrl_c().await?;
    supervisor.stop().await?;
    println!("router stopped and endpoint file removed");
    Ok(())
}

async fn uninstall(path: &Path, args: UninstallArgs) -> Result<()> {
    let endpoint_file = args
        .endpoint_file
        .unwrap_or_else(default_router_endpoint_path);
    if endpoint_file.exists() {
        let supervisor =
            RouterSupervisor::new(std::env::current_exe()?, path.to_path_buf(), &endpoint_file);
        supervisor
            .shutdown()
            .await
            .context("running Router did not accept the shutdown request")?;
        for _ in 0..40 {
            if !endpoint_file.exists() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if endpoint_file.exists() {
            bail!(
                "Router accepted shutdown but endpoint file remains: {}",
                endpoint_file.display()
            );
        }
    }

    let paths = IntegrationPaths::default();
    let restored = restore_if_present(&paths)
        .context("Codex integration was not restored; no state was removed")?;
    let manager = codex_mp_manager::ProviderManager::new(path.to_path_buf());
    let remove_registry = !args.keep_provider_data && *path == default_registry_path();
    let removed_credentials = if args.keep_provider_data {
        0
    } else {
        manager.purge_provider_data(remove_registry).context(
            "Provider data cleanup failed; the executable can remain installed for retry",
        )?
    };
    if remove_registry {
        remove_empty_state_directory(path);
    }
    println!(
        "uninstalled integration (restored_config={restored}, removed_credentials={removed_credentials}, removed_registry={remove_registry})"
    );
    if !remove_registry && !args.keep_provider_data {
        println!(
            "custom registry preserved at {}; use --keep-provider-data to preserve its keyring entries too",
            path.display()
        );
    }
    Ok(())
}

fn remove_empty_state_directory(registry_path: &std::path::Path) {
    let Some(directory) = registry_path.parent() else {
        return;
    };
    let _ = std::fs::remove_dir(directory);
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DiscoveredModel {
    upstream_model_id: String,
    display_name: Option<String>,
}

fn parse_discovered_models(value: &serde_json::Value) -> Result<Vec<DiscoveredModel>> {
    let list = value
        .as_array()
        .or_else(|| value.get("data").and_then(serde_json::Value::as_array))
        .or_else(|| value.get("models").and_then(serde_json::Value::as_array))
        .context("provider response has no data/models array")?;
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
        bail!("provider response contained no usable model ids");
    }
    Ok(models)
}

async fn fetch_models(path: &PathBuf, args: FetchModelsArgs) -> Result<()> {
    if args.all && !args.add.is_empty() {
        bail!("--all and --add are mutually exclusive");
    }
    let id = args.id;
    let registry = ProviderRegistry::load(path)?;
    let provider = registry
        .provider(&id)
        .with_context(|| format!("provider `{id}` was not found"))?;
    let provider_name = provider.name.clone();
    let key = NativeCredentialStore::default().get(&provider.credential_reference)?;
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    let response = reqwest::Client::new()
        .get(url)
        .bearer_auth(secrecy::ExposeSecret::expose_secret(&key))
        .send()
        .await?;
    let status = response.status();
    let body = response.text().await?;
    let value: serde_json::Value = serde_json::from_str(&body)
        .with_context(|| format!("provider returned non-JSON response (HTTP {status})"))?;
    if !status.is_success() {
        bail!(
            "provider returned {status}: {}",
            value
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("request failed")
        );
    }
    let models = parse_discovered_models(&value)?;
    let selection_requested = args.all || !args.add.is_empty();
    let selected = if args.all {
        models
            .iter()
            .map(|model| model.upstream_model_id.clone())
            .collect()
    } else {
        args.add
    };
    let mut added = 0usize;
    if !selected.is_empty() {
        let mut updated = ProviderRegistry::load(path)?;
        for upstream_model_id in selected {
            let discovered = models
                .iter()
                .find(|model| model.upstream_model_id == upstream_model_id)
                .with_context(|| {
                    format!("model `{upstream_model_id}` was not returned by provider")
                })?;
            if updated.provider(&id).is_some_and(|provider| {
                provider
                    .models
                    .iter()
                    .any(|model| model.upstream_model_id == discovered.upstream_model_id)
            }) {
                continue;
            }
            let display = discovered
                .display_name
                .as_deref()
                .unwrap_or(&discovered.upstream_model_id);
            updated.add_model(CustomModel::new(
                &id,
                &discovered.upstream_model_id,
                &format!("{provider_name} / {display}"),
            )?)?;
            added += 1;
        }
        updated.save()?;
    }
    for discovered in &models {
        println!(
            "{}\t{}{}",
            discovered.upstream_model_id,
            discovered.display_name.as_deref().unwrap_or(""),
            if selection_requested {
                ""
            } else {
                "\tavailable"
            }
        );
    }
    println!(
        "discovered {} models for `{id}` ({} added)",
        models.len(),
        added
    );
    Ok(())
}

fn save_provider_change(
    registry: &ProviderRegistry,
    previous_registry: &ProviderRegistry,
    store: &NativeCredentialStore,
    reference: &str,
    secret: Option<&SecretString>,
) -> Result<()> {
    let previous_secret = if secret.is_some() {
        store.get(reference).ok()
    } else {
        None
    };
    if let Some(secret) = secret {
        store.set(reference, secret)?;
    }
    if let Err(error) = registry.save() {
        let registry_rollback = previous_registry.save();
        let credential_rollback = restore_secret(store, reference, previous_secret.as_ref());
        return combine_rollback_errors(
            error.into(),
            registry_rollback,
            credential_rollback,
            "provider metadata save failed",
        );
    }
    Ok(())
}

fn combine_rollback_errors(
    original_error: anyhow::Error,
    registry_rollback: Result<(), impl std::fmt::Display>,
    credential_rollback: Result<(), impl std::fmt::Display>,
    operation: &str,
) -> Result<()> {
    match (registry_rollback, credential_rollback) {
        (Ok(()), Ok(())) => Err(original_error),
        (Err(registry_error), Ok(())) => Err(original_error).context(format!(
            "{operation}; registry rollback also failed: {registry_error}"
        )),
        (Ok(()), Err(credential_error)) => Err(original_error).context(format!(
            "{operation}; credential rollback also failed: {credential_error}"
        )),
        (Err(registry_error), Err(credential_error)) => Err(original_error).context(format!(
            "{operation}; registry rollback failed: {registry_error}; credential rollback failed: {credential_error}"
        )),
    }
}

fn delete_credential_if_present(
    store: &NativeCredentialStore,
    reference: &str,
) -> Result<Option<SecretString>> {
    match store.get(reference) {
        Ok(secret) => {
            store.delete(reference)?;
            Ok(Some(secret))
        }
        Err(CredentialStoreError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn restore_secret(
    store: &NativeCredentialStore,
    reference: &str,
    previous_secret: Option<&SecretString>,
) -> Result<()> {
    match previous_secret {
        Some(secret) => store.set(reference, secret)?,
        None => match store.delete(reference) {
            Ok(()) | Err(CredentialStoreError::NotFound(_)) => {}
            Err(error) => return Err(error.into()),
        },
    }
    Ok(())
}

fn read_secret(use_stdin: bool, env_name: Option<String>) -> Result<Option<SecretString>> {
    if let Some(name) = env_name {
        let value = std::env::var(&name)
            .with_context(|| format!("environment variable `{name}` is not set"))?;
        return Ok(Some(SecretString::from(value)));
    }
    if use_stdin {
        let mut value = String::new();
        io::stdin().read_to_string(&mut value)?;
        let value = value.trim_end_matches(['\r', '\n']).to_owned();
        if value.is_empty() {
            bail!("stdin did not contain an API key");
        }
        return Ok(Some(SecretString::from(value)));
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_model_list_shapes() {
        let value = serde_json::json!({
            "data": [
                {"id": "qwen3.8", "name": "Qwen 3.8"},
                "deepseek-v4"
            ]
        });
        assert_eq!(
            parse_discovered_models(&value).unwrap(),
            vec![
                DiscoveredModel {
                    upstream_model_id: "qwen3.8".into(),
                    display_name: Some("Qwen 3.8".into()),
                },
                DiscoveredModel {
                    upstream_model_id: "deepseek-v4".into(),
                    display_name: None,
                }
            ]
        );
    }

    #[test]
    fn parses_top_level_string_array() {
        let value = serde_json::json!(["qwen3.8", "deepseek-v4"]);
        assert_eq!(parse_discovered_models(&value).unwrap().len(), 2);
    }
}
