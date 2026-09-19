use std::ffi::OsString;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};
use codex_mp_catalog::{discover_official_catalog, merge_catalog};
use codex_mp_core::{
    AuthStrategy, CustomModel, ModelEdit, ProviderConfig, ProviderProtocol, ProviderRegistry,
    command_for_executable, default_registry_path, executable_variants, resolve_executable,
};
use codex_mp_credentials::{CredentialStoreError, NativeCredentialStore};
use codex_mp_desktop::{
    DESKTOP_LAUNCHER_CONFIG_FILE, DesktopInstallOptions, DesktopIntegrationState,
    DesktopLauncherConfig, DesktopPaths, DesktopStatus, status_for,
};
use codex_mp_integration::{IntegrationPaths, build_and_install, repair, restore_if_present};
use codex_mp_manager::RouterSupervisor;
use codex_mp_router::{RouterConfig, RouterState, serve};
use secrecy::SecretString;

mod tray;

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
    /// Select the credential persistence backend. File storage is opt-in and
    /// is protected with 0600 permissions; keyring is the default.
    #[arg(long, value_enum, default_value_t = SecretBackendArg::Keyring)]
    secret_backend: SecretBackendArg,
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
    /// Start a stock Codex binary with a managed local Router.
    Launch(LaunchArgs),
    /// Resume an existing stock Codex thread with an explicit OmniBridge provider override.
    Resume(ResumeArgs),
    /// Install, inspect, or restore the ChatGPT Desktop runtime adapter.
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,
    },
    /// Start or manage the embedded Web configuration panel.
    Web {
        #[command(subcommand)]
        command: WebCommand,
    },
}

#[derive(Debug, Subcommand)]
enum DesktopCommand {
    /// Print the detected Desktop release and managed-runtime state.
    Status,
    /// Install a reversible launcher in the user-level Desktop standalone release.
    Install(DesktopInstallArgs),
    /// Restore the original Desktop entrypoint and remove the managed runtime.
    Restore,
}

#[derive(Debug, Subcommand)]
enum WebCommand {
    /// Start the Web control panel.
    Start(WebStartArgs),
    /// Set or update the access password for the Web panel.
    Password(WebPasswordArgs),
    /// Configure external/remote network access for the Web panel.
    Remote(WebRemoteArgs),
    /// Enable or disable web browser access.
    Access(WebAccessArgs),
    /// Show Web panel status and security configuration.
    Status,
}

#[derive(Debug, Args)]
struct WebAccessArgs {
    /// Enable web browser access (requires password to be set)
    #[arg(long)]
    enable: bool,
    /// Disable web browser access (local desktop direct access only)
    #[arg(long)]
    disable: bool,
}

#[derive(Debug, Args)]
struct WebStartArgs {
    /// Listen port for the Web panel (default: 31828, or saved config)
    #[arg(long, short)]
    port: Option<u16>,
    /// Allow external/LAN network access (bind 0.0.0.0; requires password)
    #[arg(long)]
    allow_remote: bool,
    /// Run purely in headless/terminal mode without creating a system tray icon
    #[arg(long)]
    headless: bool,
    /// Automatically open the Web panel in the default browser on launch
    #[arg(long)]
    open: bool,
    /// Secure endpoint file used by the background router
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
    /// Loopback secret token for local desktop IPC / embedded window direct access
    #[arg(long, env = "CODEX_MP_LOCAL_TOKEN", alias = "token")]
    local_token: Option<String>,
}

#[derive(Debug, Args)]
struct WebPasswordArgs {
    /// The password to set. If not provided, will read from stdin or prompt.
    #[arg(value_name = "PASSWORD")]
    password: Option<String>,
    /// Read password from stdin
    #[arg(long)]
    stdin: bool,
    /// Clear the password (will also disable remote access)
    #[arg(long)]
    clear: bool,
}

#[derive(Debug, Args)]
struct WebRemoteArgs {
    /// Enable external/LAN access (bind 0.0.0.0; requires password to be set)
    #[arg(long)]
    enable: bool,
    /// Disable external access (bind loopback 127.0.0.1 only)
    #[arg(long)]
    disable: bool,
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
    #[arg(long, value_enum, default_value_t = AuthStrategyArg::Bearer)]
    auth_strategy: AuthStrategyArg,
    #[arg(long, requires = "auth_strategy")]
    auth_header: Option<String>,
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
    #[arg(long, value_enum)]
    auth_strategy: Option<AuthStrategyArg>,
    #[arg(long)]
    auth_header: Option<String>,
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
    /// Secure endpoint file used by the legacy experimental runtime adapter.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
struct ManagerArgs {
    /// Secure endpoint file shared with the stock Codex process.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
}

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
struct LaunchArgs {
    /// Use the stock Codex terminal binary instead of the default side-by-side artifact.
    #[arg(long, conflicts_with = "app_server_binary")]
    codex_binary: Option<PathBuf>,
    /// Use a standalone legacy experimental codex-app-server binary.
    #[arg(long, conflicts_with = "codex_binary")]
    app_server_binary: Option<PathBuf>,
    /// Endpoint file shared with the stock Codex process.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
    /// Extra arguments passed verbatim to the selected Codex binary.
    #[arg()]
    args: Vec<OsString>,
}

#[derive(Debug, Args)]
#[command(trailing_var_arg = true)]
struct ResumeArgs {
    /// Require the explicit provider migration guard. This never edits rollout/history files.
    #[arg(long)]
    through_omnibridge: bool,
    /// Stock Codex binary to invoke; otherwise use the global --codex-bin value.
    #[arg(long)]
    codex_binary: Option<PathBuf>,
    /// Arguments after `resume`, such as a session id, --last, or a prompt.
    #[arg()]
    args: Vec<OsString>,
}

#[derive(Debug, Args)]
struct DesktopInstallArgs {
    /// Path to the raw legacy experimental `codex-app-server` binary.
    #[arg(long)]
    app_server_binary: Option<PathBuf>,
    /// Build metadata emitted next to the legacy experimental app-server artifact.
    #[arg(long)]
    build_metadata: Option<PathBuf>,
    /// Path to the `codex-mp` manager binary used by the Desktop launcher.
    #[arg(long)]
    codex_mp_binary: Option<PathBuf>,
    /// Endpoint file passed to the legacy experimental app-server.
    #[arg(long)]
    endpoint_file: Option<PathBuf>,
    /// Do not update model_catalog_json before installing the runtime.
    #[arg(long)]
    skip_catalog: bool,
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

#[derive(Debug, Clone, Copy, ValueEnum)]
enum SecretBackendArg {
    Keyring,
    File,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum AuthStrategyArg {
    Bearer,
    ApiKey,
    Header,
    None,
}

fn auth_strategy(value: AuthStrategyArg, header_name: Option<String>) -> Result<AuthStrategy> {
    let strategy = match value {
        AuthStrategyArg::Bearer => AuthStrategy::Bearer,
        AuthStrategyArg::ApiKey => AuthStrategy::ApiKey,
        AuthStrategyArg::None => AuthStrategy::None,
        AuthStrategyArg::Header => AuthStrategy::Header {
            name: header_name
                .clone()
                .context("--auth-header is required with --auth-strategy header")?,
        },
    };
    // `--auth-header` used to be accepted and silently discarded for every other
    // strategy, so the command reported success while ignoring what was asked.
    if header_name.is_some() && !matches!(strategy, AuthStrategy::Header { .. }) {
        bail!("--auth-header is only valid together with --auth-strategy header");
    }
    Ok(strategy)
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
    if let Some(config_path) = desktop_launcher_config_path()? {
        return run_desktop_launcher(&config_path).await;
    }
    tracing_subscriber::fmt()
        .with_env_filter("codex_mp=info")
        .with_target(false)
        .init();
    let cli = Cli::parse();
    if matches!(cli.secret_backend, SecretBackendArg::File) {
        // This process-local setting is established only by the explicit CLI
        // option; the library default remains fail-closed on keyring errors.
        unsafe { std::env::set_var("CODEX_MP_SECRET_BACKEND", "file") };
    }
    let registry_path = cli.registry.unwrap_or_else(default_registry_path);
    match cli.command {
        Command::Provider { command } => provider_command(&registry_path, command).await,
        Command::Model { command } => model_command(&registry_path, command).await,
        Command::Models => list_models(&registry_path, &cli.codex_bin),
        Command::Sync => sync(&registry_path, &cli.codex_bin),
        Command::Repair => repair_integration(&registry_path, &cli.codex_bin),
        Command::Restore => restore_integration(&registry_path),
        Command::Uninstall(args) => uninstall(&registry_path, args).await,
        Command::Status => status(&registry_path, &cli.codex_bin),
        Command::Router(args) => run_router(&registry_path, args).await,
        Command::Manager(args) => run_manager(&registry_path, args).await,
        Command::Launch(args) => launch(&registry_path, args).await.map(|status| {
            if let Some(code) = status.code() {
                std::process::exit(code);
            }
            std::process::exit(1);
        }),
        Command::Resume(args) => resume(&registry_path, &cli.codex_bin, args)
            .await
            .map(|status| {
                if let Some(code) = status.code() {
                    std::process::exit(code);
                }
                std::process::exit(1);
            }),
        Command::Desktop { command } => desktop_command(&registry_path, command),
        Command::Web { command } => web_command(&registry_path, &cli.codex_bin, command).await,
    }
}

fn desktop_launcher_config_path() -> Result<Option<PathBuf>> {
    let executable = std::env::current_exe()?;
    let Some(parent) = executable.parent() else {
        return Ok(None);
    };
    let config = parent.join(DESKTOP_LAUNCHER_CONFIG_FILE);
    match fs::symlink_metadata(&config) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            bail!(
                "Desktop launcher configuration must not be a symlink: {}",
                config.display()
            )
        }
        Ok(metadata) if metadata.is_file() => Ok(Some(config)),
        Ok(_) => bail!(
            "Desktop launcher configuration is not a regular file: {}",
            config.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

async fn run_desktop_launcher(config_path: &Path) -> Result<()> {
    let config: DesktopLauncherConfig = serde_json::from_str(&fs::read_to_string(config_path)?)
        .with_context(|| {
            format!(
                "reading Desktop launcher configuration: {}",
                config_path.display()
            )
        })?;
    if config.schema_version != 1 {
        bail!(
            "unsupported Desktop launcher configuration schema: {}",
            config.schema_version
        );
    }

    let current_executable = fs::canonicalize(std::env::current_exe()?)?;
    let manager_binary = resolve_executable(&config.manager_binary);
    if !manager_binary.is_file() {
        bail!(
            "codex-mp manager binary was not found: {}",
            manager_binary.display()
        );
    }
    if fs::canonicalize(&manager_binary).ok().as_ref() == Some(&current_executable) {
        bail!("Desktop launcher manager points back to the launcher itself");
    }
    if !config.app_server_binary.is_file() {
        bail!(
            "managed Desktop app-server binary was not found: {}",
            config.app_server_binary.display()
        );
    }

    let endpoint_file = std::env::var_os("CODEX_MP_ROUTER_ENDPOINT_FILE")
        .map(PathBuf::from)
        .unwrap_or_else(|| config.endpoint_file.clone());
    let mut command = tokio::process::Command::from(command_for_executable(&manager_binary));
    command
        .arg("launch")
        .arg("--app-server-binary")
        .arg(&config.app_server_binary)
        .arg("--endpoint-file")
        .arg(&endpoint_file)
        .arg("--")
        .args(std::env::args_os().skip(1))
        .env("CODEX_MP_ROUTER_ENDPOINT_FILE", &endpoint_file);
    let status = command
        .status()
        .await
        .with_context(|| format!("starting codex-mp manager: {}", manager_binary.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

async fn provider_command(path: &PathBuf, command: ProviderCommand) -> Result<()> {
    match command {
        ProviderCommand::Add(args) => {
            let secret = read_secret(args.api_key_stdin, args.api_key_env)?;
            let (mut registry, _lock) = ProviderRegistry::load_locked(path)?;
            let previous_registry = registry.clone();
            let mut provider = ProviderConfig::new(&args.name, &args.base_url)?;
            provider.protocol = args.protocol.into();
            provider.auth_strategy = auth_strategy(args.auth_strategy, args.auth_header)?;
            let reference = provider.credential_reference.clone();
            registry.add_provider(provider)?;
            save_provider_change(
                &registry,
                &previous_registry,
                &NativeCredentialStore::default(),
                &reference,
                secret.as_ref(),
            )
            .await?;
            println!(
                "added provider `{reference}` metadata at {}",
                path.display()
            );
            reload_running_router(path).await?;
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
            let (mut registry, _lock) = ProviderRegistry::load_locked(path)?;
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
            if let Some(auth_strategy_arg) = args.auth_strategy {
                provider.auth_strategy =
                    auth_strategy(auth_strategy_arg, args.auth_header.clone())?;
            } else if args.auth_header.is_some() {
                bail!("--auth-header requires --auth-strategy header");
            }
            if args.enable {
                provider.enabled = true;
            }
            if args.disable {
                provider.enabled = false;
            }
            let reference = provider.credential_reference.clone();
            // Everything above is routing state, and the credential may be
            // replaced below. A running Router caches resolved credentials per
            // `registry.generation()` and only clears that cache when the
            // generation *changes*, so without this bump an edited API key kept
            // resolving to the previous secret until the Router restarted.
            registry.bump_generation();
            save_provider_change(
                &registry,
                &previous_registry,
                &NativeCredentialStore::default(),
                &reference,
                secret.as_ref(),
            )
            .await?;
            println!("updated provider `{}`", args.id);
            reload_running_router(path).await?;
        }
        ProviderCommand::Remove(args) => {
            let (mut registry, _lock) = ProviderRegistry::load_locked(path)?;
            let previous_registry = registry.clone();
            let provider = registry.remove_provider(&args.id)?;
            let store = NativeCredentialStore::default();
            let previous_secret = if args.purge_credential {
                delete_credential_if_present(&store, &provider.credential_reference).await?
            } else {
                None
            };
            if let Err(error) = registry.save() {
                let registry_rollback = previous_registry.save();
                let credential_rollback: Result<()> = match previous_secret.as_ref() {
                    Some(secret) => codex_mp_credentials::set_blocking(
                        Arc::new(store.clone()),
                        provider.credential_reference.clone(),
                        secret.clone(),
                    )
                    .await
                    .map_err(anyhow::Error::new),
                    None => Ok(()),
                };
                return combine_rollback_errors(
                    error.into(),
                    registry_rollback,
                    credential_rollback,
                    "provider removal failed",
                );
            }
            println!("removed provider `{}`", args.id);
            reload_running_router(path).await?;
        }
        ProviderCommand::FetchModels(args) => fetch_models(path, args).await?,
    }
    Ok(())
}

async fn model_command(path: &PathBuf, command: ModelCommand) -> Result<()> {
    let (mut registry, _lock) = ProviderRegistry::load_locked(path)?;
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
            reload_running_router(path).await?;
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
            reload_running_router(path).await?;
        }
        ModelCommand::Remove { logical_model_id } => {
            registry.remove_model(&logical_model_id)?;
            registry.save()?;
            println!("removed model `{logical_model_id}`");
            reload_running_router(path).await?;
        }
        ModelCommand::Enable { logical_model_id } => {
            set_model_enabled(&mut registry, &logical_model_id, true)?;
            reload_running_router(path).await?;
        }
        ModelCommand::Disable { logical_model_id } => {
            set_model_enabled(&mut registry, &logical_model_id, false)?;
            reload_running_router(path).await?;
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
    // The enabled set is routing state. A running Router caches resolved
    // credentials per `registry.generation()` and only clears that cache when the
    // generation *changes*, so toggling a model must bump it (same gap as
    // `ProviderManager::set_model_enabled`).
    registry.bump_generation();
    registry.save()?;
    println!(
        "{} `{logical_model_id}`",
        if enabled { "enabled" } else { "disabled" }
    );
    Ok(())
}

fn list_models(path: &PathBuf, codex_bin: &Path) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let catalog_binary = catalog_binary_for(path, codex_bin)?;
    let official = discover_official_catalog(&catalog_binary)?;
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

fn sync(path: &PathBuf, codex_bin: &Path) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let paths = IntegrationPaths::for_registry(path);
    let catalog_binary = catalog_binary_for(path, codex_bin)?;
    let manifest = build_and_install(&paths, &registry, &catalog_binary)?;
    println!("merged catalog: {}", paths.catalog.display());
    println!(
        "managed Codex field: {} = {}",
        manifest.managed_field, manifest.applied_value
    );
    println!("Codex provider was not changed; OAuth credentials were not read.");
    Ok(())
}

fn repair_integration(path: &PathBuf, codex_bin: &Path) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let paths = IntegrationPaths::for_registry(path);
    let catalog_binary = catalog_binary_for(path, codex_bin)?;
    let manifest = repair(&paths, &registry, &catalog_binary)?;
    println!(
        "repaired catalog integration for {}",
        manifest.codex_version.as_deref().unwrap_or("unknown Codex")
    );
    Ok(())
}

fn restore_integration(registry_path: &Path) -> Result<()> {
    let paths = IntegrationPaths::for_registry(registry_path);
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
    println!(
        "catalog: {}",
        IntegrationPaths::for_registry(path).catalog.display()
    );
    let catalog_binary = catalog_binary_for(path, codex_bin)?;
    println!("Codex catalog source: {}", catalog_binary.display());
    // Report the real policy instead of a blanket reassurance: the router is
    // loopback-only, but the web panel can be bound to 0.0.0.0, and the
    // account manager does read auth.json.
    println!("router bind policy: 127.0.0.1 only");
    let registry = ProviderRegistry::load(path)?;
    let sec = registry.web_security();
    println!(
        "web panel bind policy: {}",
        if sec.allow_remote {
            "0.0.0.0 (remote access enabled; plain HTTP, password required)"
        } else {
            "127.0.0.1 only"
        }
    );
    println!("OAuth: the account manager reads auth.json to report the active account;");
    println!("       OAuth tokens are only rewritten when you explicitly switch accounts");
    println!("       from the web panel, and are never forwarded to a custom provider.");
    Ok(())
}

fn catalog_binary_for(registry_path: &Path, configured: &Path) -> Result<PathBuf> {
    if configured != Path::new("codex") {
        return Ok(configured.to_path_buf());
    }
    let Ok(paths) = DesktopPaths::discover() else {
        return Ok(configured.to_path_buf());
    };
    let manifest_path = DesktopPaths::manifest_path_for_registry(registry_path);
    if manifest_path.exists() {
        return Ok(paths.catalog_binary(&manifest_path)?);
    }
    Ok(paths.entrypoint)
}

fn desktop_command(path: &Path, command: DesktopCommand) -> Result<()> {
    match command {
        DesktopCommand::Status => desktop_status(path),
        DesktopCommand::Install(args) => desktop_install(path, args),
        DesktopCommand::Restore => desktop_restore(path),
    }
}

fn desktop_status(registry_path: &Path) -> Result<()> {
    let paths = DesktopPaths::discover().context("detecting ChatGPT Desktop")?;
    let manifest_path = DesktopPaths::manifest_path_for_registry(registry_path);
    let status = status_for(&paths, &manifest_path)?;
    print_desktop_status(&status);
    Ok(())
}

fn print_desktop_status(status: &DesktopStatus) {
    let state = match status.state {
        DesktopIntegrationState::Unmanaged => "unmanaged",
        DesktopIntegrationState::Managed => "managed",
        DesktopIntegrationState::Drifted => "drifted",
    };
    println!("Desktop integration: {state}");
    println!("platform: {:?}", status.platform);
    println!("Desktop release: {}", status.version);
    println!("release directory: {}", status.release_dir.display());
    println!("entrypoint: {}", status.entrypoint.display());
    if let Some(launcher_path) = &status.launcher_path {
        println!("managed launcher: {}", launcher_path.display());
    }
    println!("manifest: {}", status.manifest_path.display());
    println!("entrypoint sha256: {}", status.current_sha256);
    if let Some(launcher_sha256) = &status.launcher_sha256 {
        println!("managed launcher sha256: {launcher_sha256}");
    }
    if status.active_pids.is_empty() {
        println!("active Desktop processes: none detected");
    } else {
        println!("active Desktop process ids: {:?}", status.active_pids);
    }
}

fn desktop_install(registry_path: &Path, args: DesktopInstallArgs) -> Result<()> {
    let desktop_paths = DesktopPaths::discover().context("detecting ChatGPT Desktop")?;
    let manifest_path = DesktopPaths::manifest_path_for_registry(registry_path);
    let app_server_binary = find_launch_binary(
        args.app_server_binary,
        "CODEX_MP_APP_SERVER_BIN",
        &["codex-mp-app-server-bin"],
    )?;
    let build_metadata = args
        .build_metadata
        .or_else(|| std::env::var_os("CODEX_MP_BUILD_METADATA").map(PathBuf::from))
        .unwrap_or_else(|| {
            app_server_binary
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("codex-mp-build.json")
        });
    let codex_mp_binary = match args.codex_mp_binary {
        Some(path) => resolve_executable(&path),
        None => std::env::current_exe()?,
    };
    if !codex_mp_binary.is_file() {
        bail!(
            "codex-mp binary was not found: {}",
            codex_mp_binary.display()
        );
    }
    let endpoint_file = args.endpoint_file.unwrap_or_else(|| {
        IntegrationPaths::for_registry(registry_path)
            .config_dir
            .join("router-endpoint.json")
    });

    let result = codex_mp_desktop::install(&DesktopInstallOptions {
        manifest_path: manifest_path.clone(),
        app_server_binary,
        build_metadata,
        codex_mp_binary,
        endpoint_file,
    })
    .context("installing the reversible Desktop runtime adapter")?;

    if !args.skip_catalog {
        let registry = ProviderRegistry::load(registry_path)?;
        let integration_paths = IntegrationPaths::for_registry(registry_path);
        if let Err(error) = build_and_install(&integration_paths, &registry, &result.catalog_binary)
        {
            let rollback = codex_mp_desktop::restore(&desktop_paths, &manifest_path);
            if let Err(rollback_error) = rollback {
                bail!(
                    "catalog integration failed: {error}; Desktop rollback failed: {rollback_error}"
                );
            }
            return Err(error.into());
        }
        println!(
            "merged Desktop catalog: {}",
            integration_paths.catalog.display()
        );
    }

    println!(
        "installed Desktop runtime adapter for {}",
        result.manifest.release_dir.display()
    );
    println!("restart ChatGPT Desktop to load the managed stock app-server");
    println!("restore with: codex-mp desktop restore");
    Ok(())
}

fn desktop_restore(registry_path: &Path) -> Result<()> {
    let paths = DesktopPaths::discover().context("detecting ChatGPT Desktop")?;
    let manifest_path = DesktopPaths::manifest_path_for_registry(registry_path);
    if codex_mp_desktop::restore(&paths, &manifest_path)? {
        println!("restored the original Desktop entrypoint and removed the managed runtime");
    } else if codex_mp_desktop::restore_orphaned_launcher(&paths, &manifest_path)? {
        // No manifest, but the entrypoint is still one of our launchers (an
        // interrupted install, or a config directory the user cleaned). Saying
        // "no manifest was found" left the Desktop hijacked with no CLI way back.
        println!("restored the original Desktop entrypoint (no manifest was present)");
    } else {
        println!("no Desktop integration manifest was found");
    }
    Ok(())
}

async fn run_router(path: &PathBuf, args: RouterArgs) -> Result<()> {
    let registry = ProviderRegistry::load(path)?;
    let integration_paths = IntegrationPaths::for_registry(path);
    let endpoint_file = args
        .endpoint_file
        .unwrap_or_else(|| integration_paths.config_dir.join("router-endpoint.json"));
    let capability_path = integration_paths.capability_path();
    let capability =
        codex_mp_integration::ensure_capability(&capability_path).with_context(|| {
            format!(
                "ensuring managed Router capability: {}",
                capability_path.display()
            )
        })?;
    let capability = capability.trim();
    if capability.is_empty() {
        bail!(
            "managed Router capability is empty: {}",
            capability_path.display()
        );
    }
    let state = RouterState::with_capability_token(
        registry,
        Arc::new(NativeCredentialStore::default()),
        SecretString::from(capability.to_owned()),
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
    let endpoint_file = args.endpoint_file.unwrap_or_else(|| {
        IntegrationPaths::for_registry(path)
            .config_dir
            .join("router-endpoint.json")
    });
    let executable = std::env::current_exe()?;
    // Same reasoning as `launch`: stock Codex dials the `base_url` from
    // `config.toml`, so a long-lived Router must bind that port, not an ephemeral
    // one, or Codex cannot reach it.
    let manager_port = codex_mp_integration::router_port_for_registry(path);
    let mut supervisor = RouterSupervisor::new(executable, path.to_path_buf(), &endpoint_file)
        .with_port(manager_port);
    let endpoint = supervisor.start().await?;
    println!("router started on {}", endpoint.base_url);
    println!("manager is running; press Ctrl-C to stop the Router");
    // Waiting on Ctrl-C alone meant SIGTERM (systemd stop, `kill`, a container
    // stop) skipped `stop()` entirely and left the Router running as an orphan.
    wait_for_shutdown_signal().await;
    supervisor.stop().await?;
    println!("router stopped and endpoint file removed");
    Ok(())
}

/// Resolve when the process is asked to terminate by any supported mechanism.
async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let mut terminate = match signal(SignalKind::terminate()) {
            Ok(stream) => stream,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        let mut interrupt = match signal(SignalKind::interrupt()) {
            Ok(stream) => stream,
            Err(_) => {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
        };
        tokio::select! {
            _ = terminate.recv() => {}
            _ = interrupt.recv() => {}
            _ = tokio::signal::ctrl_c() => {}
        }
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
}

async fn launch(path: &Path, args: LaunchArgs) -> Result<ExitStatus> {
    let (requested_binary, env_name, default_names): (Option<PathBuf>, &str, &[&str]) =
        if let Some(binary) = args.app_server_binary {
            (
                Some(binary),
                "CODEX_MP_APP_SERVER_BIN",
                &["codex-mp-app-server-bin"],
            )
        } else if let Some(binary) = args.codex_binary {
            (
                Some(binary),
                "CODEX_MP_CODEX_BIN",
                &["codex-mp-codex-bin", "codex-mp-codex"],
            )
        } else {
            (
                None,
                "CODEX_MP_CODEX_BIN",
                &["codex-mp-codex-bin", "codex-mp-codex"],
            )
        };
    let codex_binary = find_launch_binary(requested_binary, env_name, default_names)?;
    let endpoint_file = args.endpoint_file.unwrap_or_else(|| {
        IntegrationPaths::for_registry(path)
            .config_dir
            .join("router-endpoint.json")
    });
    // Stock Codex dials the `base_url` recorded in `config.toml`; it never reads
    // the endpoint file. Starting the Router on an ephemeral port therefore sent
    // every request to a port nobody was listening on. Bind the same port the
    // config advertises so the managed config actually resolves.
    let launch_port = codex_mp_integration::router_port_for_registry(path);
    let mut supervisor =
        RouterSupervisor::new(std::env::current_exe()?, path.to_path_buf(), &endpoint_file)
            .with_port(launch_port);
    let endpoint = supervisor.start().await?;

    let mut command = tokio::process::Command::from(command_for_executable(&codex_binary));
    command
        .args(args.args)
        .env("CODEX_MP_ROUTER_ENDPOINT_FILE", &endpoint_file);
    let status = command.status().await;
    let stop_result = supervisor.stop().await;
    stop_result?;
    let status = status.with_context(|| {
        format!(
            "failed to start stock Codex binary '{}' (Router endpoint: {})",
            codex_binary.display(),
            endpoint.base_url
        )
    })?;
    Ok(status)
}

async fn web_command(registry_path: &Path, codex_bin: &Path, command: WebCommand) -> Result<()> {
    match command {
        WebCommand::Start(args) => {
            let integration_paths = IntegrationPaths::for_registry(registry_path);
            let endpoint_file = args
                .endpoint_file
                .unwrap_or_else(|| integration_paths.config_dir.join("router-endpoint.json"));
            let router_bin = std::env::current_exe()?;
            let codex_bin = catalog_binary_for(registry_path, codex_bin)?;
            let allow_remote = if args.allow_remote { Some(true) } else { None };

            let registry = ProviderRegistry::load(registry_path)?;
            let effective_port = args.port.unwrap_or(registry.web_security().port);
            let web_url = format!("http://localhost:{effective_port}");

            let _tray = if !args.headless {
                tray::spawn_tray(tray::TrayConfig {
                    web_url: web_url.clone(),
                    title: "Codex OmniBridge".into(),
                })
            } else {
                None
            };

            if args.open {
                let url_clone = web_url.clone();
                tokio::spawn(async move {
                    tokio::time::sleep(std::time::Duration::from_millis(600)).await;
                    let _ = open::that(&url_clone);
                });
            }

            codex_mp_web::run_web_server_with_local_token(
                registry_path.to_path_buf(),
                endpoint_file,
                router_bin,
                codex_bin,
                args.port,
                allow_remote,
                args.local_token,
            )
            .await
            .context("running Web control panel")?;
        }
        WebCommand::Password(args) => {
            let (mut registry, _lock) = ProviderRegistry::load_locked(registry_path)?;
            if args.clear {
                let sec = registry.web_security_mut();
                sec.password_hash = None;
                sec.web_enabled = false;
                sec.allow_remote = false;
                registry.save()?;
                println!("Web panel password cleared; web browser access disabled.");
                return Ok(());
            }

            let pwd = if let Some(p) = args.password {
                p
            } else if args.stdin {
                let mut buffer = String::new();
                io::stdin().read_to_string(&mut buffer)?;
                buffer.trim().to_string()
            } else {
                bail!("please provide a password or pass --stdin / --clear");
            };

            if pwd.is_empty() {
                bail!("password cannot be empty");
            }

            let hash = codex_mp_web::hash_password(&pwd);
            registry.web_security_mut().password_hash = Some(hash);
            registry.save()?;
            println!("Web panel access password updated successfully.");
        }
        WebCommand::Access(args) => {
            if args.enable && args.disable {
                bail!("--enable and --disable are mutually exclusive");
            }
            let (mut registry, _lock) = ProviderRegistry::load_locked(registry_path)?;
            if args.enable {
                if registry.web_security().password_hash.is_none() {
                    bail!(
                        "cannot enable web access: please set a password first using `codex-mp web password <PASSWORD>`"
                    );
                }
                registry.web_security_mut().web_enabled = true;
                registry.save()?;
                println!("Web browser access enabled.");
            } else if args.disable {
                let sec = registry.web_security_mut();
                sec.web_enabled = false;
                sec.allow_remote = false;
                registry.save()?;
                println!("Web browser access disabled (local desktop direct access only).");
            } else {
                // Exiting 0 here would make the command look successful to a
                // script that never actually changed anything.
                bail!("specify either --enable or --disable");
            }
        }
        WebCommand::Remote(args) => {
            if args.enable && args.disable {
                bail!("--enable and --disable are mutually exclusive");
            }
            let mut registry = ProviderRegistry::load(registry_path)?;
            if args.enable {
                if registry.web_security().password_hash.is_none() {
                    bail!(
                        "cannot enable remote access: please set a password first using `codex-mp web password <PASSWORD>`"
                    );
                }
                let sec = registry.web_security_mut();
                sec.web_enabled = true;
                sec.allow_remote = true;
                registry.save()?;
                println!("Web panel remote access enabled (bind 0.0.0.0).");
                println!();
                println!("SECURITY WARNING");
                println!(
                    "  Remote access is served over plain HTTP: the access password and every"
                );
                println!("  request travel the network unencrypted, and anyone who can read that");
                println!("  traffic can take over the panel.");
                println!(
                    "  Use this only on a trusted local network. For access over the internet,"
                );
                println!("  keep remote access disabled and tunnel it instead, e.g.");
                println!("      ssh -L 31828:127.0.0.1:31828 <host>");
                println!("  then open http://localhost:31828 locally.");
            } else if args.disable {
                registry.web_security_mut().allow_remote = false;
                registry.save()?;
                println!("Web panel remote access disabled (bind 127.0.0.1 only).");
            } else {
                bail!("specify either --enable or --disable");
            }
        }
        WebCommand::Status => {
            let registry = ProviderRegistry::load(registry_path)?;
            let sec = registry.web_security();
            println!("Web panel configuration:");
            println!("  configured port: {}", sec.port);
            println!(
                "  web browser access: {}",
                if sec.web_enabled {
                    "enabled"
                } else {
                    "disabled (local direct access only)"
                }
            );
            println!(
                "  password protected: {}",
                if sec.password_hash.is_some() {
                    "yes"
                } else {
                    "no"
                }
            );
            println!(
                "  remote access: {}",
                if sec.allow_remote {
                    "enabled (0.0.0.0)"
                } else {
                    "disabled (127.0.0.1)"
                }
            );
        }
    }
    Ok(())
}

/// Ask a running Router to pick up registry changes.
///
/// The web panel does this after every provider/model edit; the CLI did not, so
/// `codex-mp model add` while the panel's Router was serving left that Router
/// advertising the previous model list — the new model simply was not routable
/// until the Router was restarted. A Router that is not running is not an error
/// (the change is already saved), so this reports and returns.
async fn reload_running_router(registry_path: &Path) -> Result<()> {
    let endpoint_file = IntegrationPaths::for_registry(registry_path)
        .config_dir
        .join("router-endpoint.json");
    if !endpoint_file.exists() {
        return Ok(());
    }
    // Pin the configured port even though `reload()` does not bind one. A
    // supervisor that does not know which port `config.toml` advertises is a
    // latent hazard: any later `start()` on it would bind an ephemeral port that
    // stock Codex can never reach, which is exactly the N-17 failure mode.
    let router_port = codex_mp_integration::router_port_for_registry(registry_path);
    let supervisor = RouterSupervisor::new(
        std::env::current_exe()?,
        registry_path.to_path_buf(),
        &endpoint_file,
    )
    .with_port(router_port);
    match supervisor.reload().await {
        Ok(()) => Ok(()),
        Err(error) => {
            // Not fatal: the registry is saved and the next Router start picks it
            // up. Say so instead of failing an otherwise successful command.
            eprintln!(
                "codex-mp: saved the change, but the running router did not reload it ({error}); \
                 restart the router to apply it"
            );
            Ok(())
        }
    }
}

/// Whether something is listening at the Router's `/readyz`.
///
/// `resume` hands stock Codex `model_provider="omnibridge"`, so a Router must
/// already be running. This command does not start one (unlike `launch`); without
/// the check the session silently pointed at a port nobody owned and every model
/// request failed with no explanation.
async fn router_is_reachable(base_url: &str) -> bool {
    // `base_url` carries the provider path (`.../v1`); `/readyz` sits at the root.
    let root = base_url
        .trim_end_matches('/')
        .trim_end_matches("/v1")
        .trim_end_matches('/');
    let url = format!("{root}/readyz");
    match reqwest::Client::builder()
        // A redirect could point this probe at an unrelated host; refuse them,
        // consistent with every other client in this project.
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(3))
        .build()
    {
        Ok(client) => client.get(url).send().await.is_ok(),
        Err(_) => false,
    }
}

async fn resume(
    registry_path: &Path,
    configured_codex: &Path,
    args: ResumeArgs,
) -> Result<ExitStatus> {
    if !args.through_omnibridge {
        bail!(
            "refusing an implicit thread migration; rerun with --through-omnibridge (or use stock `codex resume` unchanged)"
        );
    }
    let paths = IntegrationPaths::for_registry(registry_path);
    let config = fs::read_to_string(&paths.codex_config).with_context(|| {
        format!(
            "reading the managed stock Codex config {}; run `codex-mp sync` first",
            paths.codex_config.display()
        )
    })?;
    let parsed: toml::Value = toml::from_str(&config).with_context(|| {
        format!(
            "parsing the managed stock Codex config {}; no files were changed",
            paths.codex_config.display()
        )
    })?;
    if parsed.get("model_provider").and_then(toml::Value::as_str) != Some("omnibridge") {
        bail!(
            "managed stock Codex config is not using model_provider=omnibridge; run `codex-mp sync` first"
        );
    }
    if !paths.manifest.exists() {
        bail!(
            "OmniBridge integration manifest is missing at {}; run `codex-mp sync` first",
            paths.manifest.display()
        );
    }

    // `resume` injects `model_provider="omnibridge"`, so a Router must already be
    // listening on the port `config.toml` advertises. This command does not start
    // one (unlike `launch`). Without this check the session silently pointed at a
    // port nobody owned: Codex started, and every model request failed.
    let router_base = paths.router_base_url();
    if !router_is_reachable(&router_base).await {
        bail!(
            "no OmniBridge Router is answering at {router_base}; start one first with \
             `codex-mp manager` (or `codex-mp launch`), or install the service with \
             `codex-mp-router-service-install`"
        );
    }

    let codex_binary = args
        .codex_binary
        .unwrap_or_else(|| configured_codex.to_path_buf());
    let mut command = tokio::process::Command::from(command_for_executable(&codex_binary));
    command
        .arg("resume")
        .arg("--config")
        .arg(r#"model_provider="omnibridge""#)
        .args(args.args);
    println!(
        "resuming through stock Codex with model_provider=omnibridge; no rollout/history files will be modified"
    );
    println!(
        "if the provider boundary cannot be preserved, start a new thread instead of editing the old history"
    );
    command.status().await.with_context(|| {
        format!(
            "failed to start stock Codex binary `{}`",
            codex_binary.display()
        )
    })
}

fn find_launch_binary(
    requested: Option<PathBuf>,
    env_name: &str,
    default_names: &[&str],
) -> Result<PathBuf> {
    if let Some(path) = requested.or_else(|| std::env::var_os(env_name).map(PathBuf::from)) {
        let resolved = resolve_executable(&path);
        if resolved.is_file() {
            return Ok(resolved);
        }
        bail!(
            "stock Codex binary '{}' was not found; pass a valid --codex-binary/--app-server-binary path or set {env_name}",
            path.display()
        );
    }

    let mut candidates = Vec::new();
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        for name in default_names {
            candidates.push(parent.join(name));
        }
    }
    if let Some(path) = std::env::var_os("PATH") {
        for directory in std::env::split_paths(&path) {
            for name in default_names {
                candidates.push(directory.join(name));
            }
        }
    }
    if let Some(path) = candidates
        .into_iter()
        .flat_map(executable_variants)
        .find(|path| path.is_file())
    {
        return Ok(path);
    }

    bail!(
        "no stock Codex binary was found; build or install '{}' and pass --codex-binary/--app-server-binary",
        default_names.join("' or '")
    )
}

async fn uninstall(path: &Path, args: UninstallArgs) -> Result<()> {
    let integration_paths = IntegrationPaths::for_registry(path);
    let endpoint_file = args
        .endpoint_file
        .unwrap_or_else(|| integration_paths.config_dir.join("router-endpoint.json"));
    if endpoint_file.exists() {
        let supervisor =
            RouterSupervisor::new(std::env::current_exe()?, path.to_path_buf(), &endpoint_file);
        // An unreachable Router is already stopped, which is the state uninstall
        // wants. The endpoint file can outlive its process (SIGKILL, power loss,
        // a container restart), and treating that as a hard error made uninstall
        // impossible: it aborted *before* restoring the Codex config, so the user
        // was left with a hijacked config and no working way to undo it.
        match supervisor.shutdown().await {
            Ok(()) => {
                for _ in 0..40 {
                    if !endpoint_file.exists() {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                if endpoint_file.exists() {
                    // The Router answered but kept its endpoint file. Clear it so
                    // the rest of the uninstall can proceed rather than dead-ending.
                    eprintln!(
                        "codex-mp: router acknowledged shutdown but left {}; removing it",
                        endpoint_file.display()
                    );
                    let _ = std::fs::remove_file(&endpoint_file);
                }
            }
            Err(error) => {
                eprintln!(
                    "codex-mp: no running router answered at {} ({error}); continuing",
                    endpoint_file.display()
                );
                // Remove the orphaned record so it cannot block a later run.
                let _ = std::fs::remove_file(&endpoint_file);
            }
        }
    }

    // Restore the Codex config FIRST. It is the change that actually hijacks the
    // user's editor, and it must be undone even if the Desktop adapter cannot be
    // cleaned up. Previously a Desktop failure aborted this function before the
    // config was restored, leaving `config.toml` pointing at a Router that was
    // about to be uninstalled — the worst possible end state.
    let restored = restore_if_present(&integration_paths)
        .context("Codex integration was not restored; no state was removed")?;

    // Desktop teardown is second, and a failure is reported without undoing the
    // config restore that already succeeded.
    let desktop_manifest = DesktopPaths::manifest_path_for_registry(path);
    let desktop_restored = match codex_mp_desktop::restore_if_present(&desktop_manifest) {
        Ok(done) => done,
        Err(error) => {
            eprintln!(
                "codex-mp: the Codex config was restored, but the Desktop adapter could not be \
                 removed ({error}); run `codex-mp desktop restore` to retry"
            );
            false
        }
    };
    // The registry and its credentials are one unit of user data: a registry whose
    // keys have been deleted lists providers that cannot work, and a keyring entry
    // whose provider is gone is an orphan. They are therefore removed together or
    // kept together.
    //
    // A *custom* `--registry` is never deleted: the user chose that location and
    // may manage it separately. Its credentials are preserved for the same reason.
    // Only the standard registry location, and only without `--keep-provider-data`,
    // is treated as "uninstall everything".
    let remove_registry = should_remove_provider_data(args.keep_provider_data, path);
    // Detect an unreadable registry up front. `purge_provider_data` tolerates one
    // (otherwise a hand-edit that broke the JSON made `uninstall` impossible), but
    // with no readable references it cannot know which credentials to delete, so
    // the user must be told that a key may survive rather than being left to
    // assume the purge was complete.
    let registry_unreadable = remove_registry && ProviderRegistry::load(path).is_err();
    let removed_credentials = if remove_registry {
        let manager = codex_mp_manager::ProviderManager::new(path.to_path_buf());
        manager.purge_provider_data(true).await.context(
            "Provider data cleanup failed; the executable can remain installed for retry",
        )?
    } else {
        0
    };
    if remove_registry {
        remove_empty_state_directory(path);
    }
    println!(
        "uninstalled integration (restored_config={restored}, restored_desktop={desktop_restored}, removed_credentials={removed_credentials}, removed_registry={remove_registry})"
    );
    if registry_unreadable {
        println!(
            "note: the provider registry could not be read, so stored credentials \
             could not be enumerated or removed; check your credential store and \
             delete any leftover entry manually"
        );
    }
    if !remove_registry {
        // Say plainly what was kept and why, so the user is not left wondering
        // whether their keys survived.
        println!(
            "provider registry and its credentials preserved at {}; \
             re-run with the default registry (or delete them manually) to remove them",
            path.display()
        );
    }
    Ok(())
}

/// Whether uninstall should delete the provider registry **and** its credentials.
///
/// The registry and its credentials are one unit of user data: a registry whose
/// keys were deleted lists providers that cannot work, and a keyring entry whose
/// provider is gone is an orphan. Anything that keeps one must keep the other.
/// A custom `--registry` is never removed — the user chose that location and may
/// manage it separately.
fn should_remove_provider_data(keep_provider_data: bool, registry_path: &Path) -> bool {
    !keep_provider_data && registry_path == default_registry_path()
}

#[cfg(test)]
mod router_reachability_tests {
    use super::*;

    /// The `/readyz` probe must be derived from the provider `base_url`, which
    /// carries a `/v1` path segment that has to be stripped.
    #[test]
    fn the_readiness_probe_strips_the_provider_path() {
        let build = |base: &str| {
            base.trim_end_matches('/')
                .trim_end_matches("/v1")
                .trim_end_matches('/')
                .to_owned()
                + "/readyz"
        };
        assert_eq!(
            build("http://127.0.0.1:8787/v1"),
            "http://127.0.0.1:8787/readyz"
        );
        assert_eq!(
            build("http://127.0.0.1:8787/v1/"),
            "http://127.0.0.1:8787/readyz"
        );
        assert_eq!(
            build("http://127.0.0.1:8787"),
            "http://127.0.0.1:8787/readyz"
        );
    }

    /// Regression: `resume` injected `model_provider="omnibridge"` without
    /// checking that a Router was listening. With none running, stock Codex
    /// started and every model request failed silently. A closed port must be
    /// reported as unreachable.
    #[tokio::test]
    async fn a_closed_port_is_reported_as_unreachable() {
        // Bind and immediately drop, so the port is almost certainly free.
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert!(
            !router_is_reachable(&format!("http://127.0.0.1:{port}/v1")).await,
            "a port with no listener must not be treated as a live Router"
        );
    }
}

#[cfg(test)]
mod model_discovery_tests {
    use super::*;

    /// The discovery response parser must accept the shapes real gateways use and
    /// skip unusable entries rather than failing the whole call.
    #[test]
    fn parse_discovered_models_accepts_the_common_shapes() {
        // OpenAI style: `{"data":[{"id":...}]}`.
        let openai = serde_json::json!({
            "data": [{"id": "gpt-oss-120b", "display_name": "GPT-OSS"}]
        });
        let parsed = parse_discovered_models(&openai).unwrap();
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].upstream_model_id, "gpt-oss-120b");
        assert_eq!(parsed[0].display_name.as_deref(), Some("GPT-OSS"));

        // A bare array of strings.
        let bare = serde_json::json!(["a", " b ", ""]);
        let parsed = parse_discovered_models(&bare).unwrap();
        assert_eq!(parsed.len(), 2, "blank ids must be skipped, not kept");
        assert_eq!(parsed[0].upstream_model_id, "a");
        assert_eq!(parsed[1].upstream_model_id, "b");

        // `{"models":[...]}` with alternate id keys.
        let alt = serde_json::json!({"models": [{"slug": "s-1"}, {"name": "n-1"}]});
        let parsed = parse_discovered_models(&alt).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].upstream_model_id, "s-1");
        assert_eq!(parsed[1].upstream_model_id, "n-1");
    }

    /// A response with no usable list must fail loudly rather than reporting zero.
    #[test]
    fn parse_discovered_models_rejects_an_unusable_response() {
        assert!(parse_discovered_models(&serde_json::json!({"unexpected": 1})).is_err());
        // A non-empty list with nothing usable is an error, not an empty success.
        assert!(parse_discovered_models(&serde_json::json!([{}, 42, ""])).is_err());
        // An empty list is a legitimate "no models".
        assert!(
            parse_discovered_models(&serde_json::json!([]))
                .unwrap()
                .is_empty()
        );
    }
}

#[cfg(test)]
mod provider_data_tests {
    use super::*;

    /// Regression: uninstall used to delete the stored credentials but keep the
    /// registry whenever `--registry` was non-default, leaving a provider list
    /// whose keys no longer existed. The decision now covers both together.
    #[test]
    fn provider_data_is_removed_only_for_the_default_registry() {
        let default_path = default_registry_path();

        assert!(
            should_remove_provider_data(false, &default_path),
            "the default registry and its credentials must be removed on a plain uninstall"
        );
        assert!(
            !should_remove_provider_data(true, &default_path),
            "--keep-provider-data must preserve both the registry and its credentials"
        );

        let custom = PathBuf::from("/tmp/some-custom-registry/providers.json");
        assert!(
            !should_remove_provider_data(false, &custom),
            "a custom registry must never be deleted, so its credentials must survive too"
        );
    }
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
    // Offloaded: the keyring backend bridges to a synchronous API (on Linux,
    // `zbus` calls `Runtime::block_on`), which panics with "Cannot start a runtime
    // from within a runtime" when invoked from inside this async function. Every
    // other keyring touch in this file already goes through `*_blocking`; this
    // call site was missed, so `provider fetch-models` panicked unconditionally.
    let key = codex_mp_credentials::get_blocking(
        Arc::new(NativeCredentialStore::default()),
        provider.credential_reference.clone(),
    )
    .await?;
    let url = format!("{}/models", provider.base_url.trim_end_matches('/'));
    // Redirects are refused: this request carries the provider's API key, and
    // reqwest's default policy would resend the header to whatever host a
    // 307/308 names (verified elsewhere in this project with a local redirector).
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .unwrap_or_else(|_| {
            eprintln!(
                "codex-mp: FATAL: no hardened model-discovery client available; \
                 provider redirects will be followed"
            );
            reqwest::Client::new()
        });
    let mut request = client.get(url);
    // Probe with the provider's *configured* auth strategy; always sending a
    // Bearer token broke discovery for api_key/header providers.
    if let Some((name, value)) = provider
        .auth_strategy
        .credential_header(secrecy::ExposeSecret::expose_secret(&key))
    {
        request = request.header(name, value);
    }
    let response = request.send().await?;
    let status = response.status();
    // Bound the read. `text()` buffers the entire body, so a hostile or broken
    // provider could exhaust memory with a multi-gigabyte "model list". The
    // router already caps upstream responses; discovery must too.
    const MAX_DISCOVERY_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
    if let Some(length) = response.content_length()
        && length > MAX_DISCOVERY_RESPONSE_BYTES as u64
    {
        bail!(
            "provider returned {length} bytes, which exceeds the \
             {MAX_DISCOVERY_RESPONSE_BYTES} byte limit for a model list"
        );
    }
    let body = response.text().await?;
    if body.len() > MAX_DISCOVERY_RESPONSE_BYTES {
        bail!(
            "provider returned {} bytes, which exceeds the \
             {MAX_DISCOVERY_RESPONSE_BYTES} byte limit for a model list",
            body.len()
        );
    }
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
        let (mut updated, _lock) = ProviderRegistry::load_locked(path)?;
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

/// Persist a provider change together with its credential.
///
/// Every keyring touch is offloaded to a blocking thread: the Linux keyring
/// backend reaches the Secret Service through `zbus`, which internally calls
/// `tokio::runtime::Runtime::block_on`. Calling it straight from this async
/// command panicked with "Cannot start a runtime from within a runtime" and
/// aborted the command before it wrote anything.
async fn save_provider_change(
    registry: &ProviderRegistry,
    previous_registry: &ProviderRegistry,
    store: &NativeCredentialStore,
    reference: &str,
    secret: Option<&SecretString>,
) -> Result<()> {
    let previous_secret = if secret.is_some() {
        match codex_mp_credentials::get_blocking(Arc::new(store.clone()), reference.to_owned())
            .await
        {
            Ok(secret) => Some(secret),
            // "No previous secret" and "the backend could not be read" must not
            // be conflated: treating a failed read as absent would let the
            // rollback below delete a credential that is still valid.
            Err(CredentialStoreError::NotFound(_)) => None,
            Err(error) => return Err(error.into()),
        }
    } else {
        None
    };
    if let Some(secret) = secret {
        codex_mp_credentials::set_blocking(
            Arc::new(store.clone()),
            reference.to_owned(),
            secret.clone(),
        )
        .await?;
    }
    if let Err(error) = registry.save() {
        let registry_rollback = previous_registry.save();
        let credential_rollback =
            restore_secret_async(store, reference.to_owned(), previous_secret).await;
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

async fn delete_credential_if_present(
    store: &NativeCredentialStore,
    reference: &str,
) -> Result<Option<SecretString>> {
    match codex_mp_credentials::get_blocking(Arc::new(store.clone()), reference.to_owned()).await {
        Ok(secret) => {
            codex_mp_credentials::delete_blocking(Arc::new(store.clone()), reference.to_owned())
                .await?;
            Ok(Some(secret))
        }
        Err(CredentialStoreError::NotFound(_)) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

/// Async counterpart of the credential rollback path; see
/// [`save_provider_change`] for why every keyring touch is offloaded.
async fn restore_secret_async(
    store: &NativeCredentialStore,
    reference: String,
    previous_secret: Option<SecretString>,
) -> Result<()> {
    match previous_secret {
        Some(secret) => {
            codex_mp_credentials::set_blocking(Arc::new(store.clone()), reference, secret).await?;
        }
        None => {
            match codex_mp_credentials::delete_blocking(Arc::new(store.clone()), reference).await {
                Ok(()) | Err(CredentialStoreError::NotFound(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
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
