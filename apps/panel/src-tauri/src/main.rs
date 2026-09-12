use std::path::{Path, PathBuf};

use codex_mp_core::{
    ModelCapabilities, ModelEdit, ProviderProtocol, ProviderRegistry, default_registry_path,
};
use codex_mp_integration::{IntegrationPaths, build_and_install};
use codex_mp_manager::{
    DiscoveredModel, ProviderManager, ProviderSummary, RouterStatus, RouterSupervisor,
};
use codex_mp_router::default_router_endpoint_path;
use secrecy::SecretString;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tokio::sync::Mutex;

struct AppState {
    providers: ProviderManager,
    supervisor: Mutex<RouterSupervisor>,
    codex_binary: PathBuf,
}

fn error_message(error: impl std::fmt::Display) -> String {
    error.to_string()
}

fn registry_path() -> PathBuf {
    std::env::var_os("CODEX_MP_REGISTRY")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_registry_path)
}

fn router_endpoint_path() -> PathBuf {
    std::env::var_os("CODEX_MP_ROUTER_ENDPOINT_FILE")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(default_router_endpoint_path)
}

#[tauri::command]
fn list_providers(state: State<'_, AppState>) -> Result<Vec<ProviderSummary>, String> {
    state.providers.list_providers().map_err(error_message)
}

#[tauri::command]
async fn discover_models(
    provider_id: String,
    state: State<'_, AppState>,
) -> Result<Vec<DiscoveredModel>, String> {
    state
        .providers
        .discover_models(&provider_id)
        .await
        .map_err(error_message)
}

#[tauri::command]
fn add_provider(
    name: String,
    base_url: String,
    protocol: ProviderProtocol,
    api_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<ProviderSummary, String> {
    state
        .providers
        .add_provider(&name, &base_url, protocol, api_key.map(SecretString::from))
        .map_err(error_message)
}

#[tauri::command]
fn edit_provider(
    id: String,
    name: Option<String>,
    base_url: Option<String>,
    protocol: Option<ProviderProtocol>,
    enabled: Option<bool>,
    api_key: Option<String>,
    state: State<'_, AppState>,
) -> Result<ProviderSummary, String> {
    state
        .providers
        .edit_provider(
            &id,
            name,
            base_url,
            protocol,
            enabled,
            api_key.map(SecretString::from),
        )
        .map_err(error_message)
}

#[tauri::command]
fn remove_provider(
    id: String,
    purge_credential: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    state
        .providers
        .remove_provider(&id, purge_credential)
        .map_err(error_message)
}

#[tauri::command]
fn import_models(
    provider_id: String,
    discovered: Vec<DiscoveredModel>,
    selected_ids: Vec<String>,
    state: State<'_, AppState>,
) -> Result<Vec<codex_mp_manager::ModelSummary>, String> {
    state
        .providers
        .import_models(&provider_id, &discovered, &selected_ids)
        .map_err(error_message)
}

#[tauri::command]
fn add_model(
    provider_id: String,
    upstream_model_id: String,
    display_name: String,
    context_window: Option<u64>,
    images: bool,
    tools: bool,
    state: State<'_, AppState>,
) -> Result<codex_mp_manager::ModelSummary, String> {
    let mut capabilities = ModelCapabilities::default();
    capabilities.images = images;
    capabilities.tools = tools;
    state
        .providers
        .add_model(
            &provider_id,
            &upstream_model_id,
            &display_name,
            context_window,
            capabilities,
        )
        .map_err(error_message)
}

#[tauri::command]
fn edit_model(
    logical_model_id: String,
    display_name: Option<String>,
    context_window: Option<u64>,
    clear_context_window: bool,
    state: State<'_, AppState>,
) -> Result<codex_mp_manager::ModelSummary, String> {
    if display_name.is_none() && context_window.is_none() && !clear_context_window {
        return Err("model edit requires a field".into());
    }
    state
        .providers
        .edit_model(
            &logical_model_id,
            ModelEdit {
                display_name,
                context_window: if clear_context_window {
                    Some(None)
                } else {
                    context_window.map(Some)
                },
            },
        )
        .map_err(error_message)
}

#[tauri::command]
fn set_model_enabled(
    logical_model_id: String,
    enabled: bool,
    state: State<'_, AppState>,
) -> Result<codex_mp_manager::ModelSummary, String> {
    state
        .providers
        .set_model_enabled(&logical_model_id, enabled)
        .map_err(error_message)
}

#[tauri::command]
fn remove_model(logical_model_id: String, state: State<'_, AppState>) -> Result<(), String> {
    state
        .providers
        .remove_model(&logical_model_id)
        .map_err(error_message)
}

#[tauri::command]
async fn router_status(state: State<'_, AppState>) -> Result<RouterStatus, String> {
    state
        .supervisor
        .lock()
        .await
        .status()
        .await
        .map_err(error_message)
}

#[tauri::command]
async fn router_reload(state: State<'_, AppState>) -> Result<(), String> {
    state
        .supervisor
        .lock()
        .await
        .reload()
        .await
        .map_err(error_message)
}

#[tauri::command]
fn sync_catalog(state: State<'_, AppState>) -> Result<String, String> {
    let registry =
        ProviderRegistry::load(state.providers.registry_path()).map_err(error_message)?;
    let paths = IntegrationPaths::default();
    build_and_install(&paths, &registry, &state.codex_binary).map_err(error_message)?;
    Ok(paths.catalog.display().to_string())
}

async fn stop_router(app: &AppHandle) {
    let state = app.state::<AppState>();
    let _ = state.supervisor.lock().await.stop().await;
}

fn reveal_panel(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "打开面板", true, None::<&str>)?;
    let reload = MenuItem::with_id(app, "reload", "刷新 Router", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "退出并停止功能", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &reload, &quit])?;
    TrayIconBuilder::with_id("main")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_tray_icon_event(|app, event| {
            if matches!(event, TrayIconEvent::DoubleClick { .. }) {
                reveal_panel(app.app_handle());
            }
        })
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => reveal_panel(app),
            "reload" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let state = app.state::<AppState>();
                    let _ = state.supervisor.lock().await.reload().await;
                });
            }
            "quit" => {
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    stop_router(&app).await;
                    app.exit(0);
                });
            }
            _ => {}
        })
        .build(app)?;
    Ok(())
}

fn router_executable(resource_dir: Option<&Path>) -> PathBuf {
    if let Some(path) = std::env::var_os("CODEX_MP_ROUTER_BIN") {
        return PathBuf::from(path);
    }

    let mut candidates = Vec::new();
    if let Some(resource_dir) = resource_dir {
        candidates.push(resource_dir.join("codex-mp"));
    }
    if let Ok(current_exe) = std::env::current_exe()
        && let Some(parent) = current_exe.parent()
    {
        candidates.extend([
            parent.join("codex-mp"),
            parent.join("resources/codex-mp"),
            parent.join("../lib/codex-multiprovider/codex-mp"),
            parent.join("../lib/codex-multiprovider/resources/codex-mp"),
            parent.join("../share/codex-multiprovider/codex-mp"),
            parent.join("../share/codex-multiprovider/resources/codex-mp"),
        ]);
    }
    if let Some(path) = std::env::var_os("PATH") {
        candidates.extend(std::env::split_paths(&path).map(|directory| directory.join("codex-mp")));
    }
    candidates
        .into_iter()
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("codex-mp"))
}

fn codex_executable() -> PathBuf {
    std::env::var_os("CODEX_BIN")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("codex"))
}

fn main() {
    let registry_path = registry_path();
    let endpoint_file = router_endpoint_path();
    let providers = ProviderManager::new(registry_path.clone());
    let supervisor = RouterSupervisor::new(
        router_executable(None),
        registry_path,
        endpoint_file,
    );
    let state = AppState {
        providers,
        supervisor: Mutex::new(supervisor),
        codex_binary: codex_executable(),
    };

    tauri::Builder::default()
        .manage(state)
        .setup(|app| {
            build_tray(app.handle())?;
            if let Ok(resource_dir) = app.path().resource_dir()
                && let Ok(mut supervisor) = app.state::<AppState>().supervisor.try_lock()
            {
                supervisor.set_executable(router_executable(Some(&resource_dir)));
            }
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                let state = app_handle.state::<AppState>();
                let _ = state.supervisor.lock().await.start().await;
            });
            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
            }
        })
        .invoke_handler(tauri::generate_handler![
            list_providers,
            discover_models,
            add_provider,
            edit_provider,
            remove_provider,
            import_models,
            add_model,
            edit_model,
            set_model_enabled,
            remove_model,
            router_status,
            router_reload,
            sync_catalog,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Codex MultiProvider panel");
}
