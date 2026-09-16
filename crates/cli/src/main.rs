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
use codex_mp_credentials::{CredentialStore, CredentialStoreError, NativeCredentialStore};
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
    Ok(match value {
        AuthStrategyArg::Bearer => AuthStrategy::Bearer,
        AuthStrategyArg::ApiKey => AuthStrategy::ApiKey,
        AuthStrategyArg::None => AuthStrategy::None,
        AuthStrategyArg::Header => AuthStrategy::Header {
            name: header_name.context("--auth-header is required with --auth-strategy header")?,
        },
    })
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
        Command::Model { command } => model_command(&registry_path, command),
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
            let mut registry = ProviderRegistry::load(path)?;
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
    println!("router bind policy: 127.0.0.1 only");
    println!("OAuth: not accessed by codex-mp");
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
    let mut supervisor = RouterSupervisor::new(executable, path.to_path_buf(), &endpoint_file);
    let endpoint = supervisor.start().await?;
    println!("router started on {}", endpoint.base_url);
    println!("manager is running; press Ctrl-C to stop the Router");
    tokio::signal::ctrl_c().await?;
    supervisor.stop().await?;
    println!("router stopped and endpoint file removed");
    Ok(())
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
    let mut supervisor =
        RouterSupervisor::new(std::env::current_exe()?, path.to_path_buf(), &endpoint_file);
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
            let mut registry = ProviderRegistry::load(registry_path)?;
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
            let mut registry = ProviderRegistry::load(registry_path)?;
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
                println!("specify either --enable or --disable");
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
            } else if args.disable {
                registry.web_security_mut().allow_remote = false;
                registry.save()?;
                println!("Web panel remote access disabled (bind 127.0.0.1 only).");
            } else {
                println!("specify either --enable or --disable");
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

    let desktop_manifest = DesktopPaths::manifest_path_for_registry(path);
    let desktop_restored = codex_mp_desktop::restore_if_present(&desktop_manifest)
        .context("Codex Desktop was not restored; no state was removed")?;

    let restored = restore_if_present(&integration_paths)
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
        "uninstalled integration (restored_config={restored}, restored_desktop={desktop_restored}, removed_credentials={removed_credentials}, removed_registry={remove_registry})"
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
