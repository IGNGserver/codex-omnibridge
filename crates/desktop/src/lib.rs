//! Cross-platform ChatGPT Desktop runtime integration.
//!
//! ChatGPT Desktop starts its own Codex binary from the user-level
//! `~/.codex/packages/standalone` release. A shell environment inherited by a
//! terminal cannot affect that already managed process, so Desktop support is
//! an explicit, reversible legacy/experimental runtime adapter. Remote Control
//! uses the stock local Desktop path instead. This crate owns only the adapter
//! manifest, a private runtime copy, and the Desktop entrypoint launcher. It
//! never edits the system ChatGPT package or Codex auth/history.

use std::fmt::Write as FmtWrite;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use codex_mp_core::{atomic_replace, default_registry_path, set_private_permissions};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const DESKTOP_MANIFEST_SCHEMA_VERSION: u32 = 4;
pub const PINNED_UPSTREAM_COMMIT: &str = "73a1148c9c775c2a4616ce5096291740a00ed68a";
pub const DESKTOP_LAUNCHER_CONFIG_FILE: &str = "codex-mp-desktop-launcher.json";

const DESKTOP_MANIFEST_FILE: &str = "desktop-integration.json";
const RUNTIME_DIR: &str = "desktop-runtime";
const CODE_MODE_HOST: &str = "codex-code-mode-host";

#[derive(Debug, Error)]
pub enum DesktopError {
    #[error("Codex Desktop integration is only implemented for Linux, Windows, and macOS")]
    UnsupportedPlatform,
    #[error("Codex Desktop standalone release was not found at `{0}`")]
    NotFound(PathBuf),
    #[error("Desktop path is invalid: {0}")]
    InvalidPath(String),
    #[error("Desktop integration manifest error: {0}")]
    Manifest(String),
    #[error("Desktop integration IO error: {0}")]
    Io(#[from] io::Error),
    #[error("Desktop integration JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("patched app-server binary is missing or not executable: {0}")]
    MissingPatchedBinary(PathBuf),
    #[error("codex-mp binary is missing or not executable: {0}")]
    MissingManagerBinary(PathBuf),
    #[error("Desktop is running and must be closed before changing its runtime (pids: {0:?})")]
    DesktopBusy(Vec<u32>),
    #[error(
        "Desktop entrypoint `{0}` is not an untouched native binary; refusing to adopt it without a manifest"
    )]
    EntrypointNotNative(PathBuf),
    #[error("Desktop entrypoint was changed outside the managed manifest: {0}")]
    ManagedEntrypointChanged(PathBuf),
    #[error("managed Desktop backup is missing or changed: {0}")]
    BackupInvalid(PathBuf),
    #[error("managed Desktop runtime is invalid: {0}")]
    RuntimeInvalid(String),
    #[error("patched Codex build metadata is invalid: {0}")]
    BuildMetadata(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopPaths {
    pub platform: DesktopPlatform,
    pub codex_home: PathBuf,
    pub release_dir: PathBuf,
    pub bin_dir: PathBuf,
    pub entrypoint: PathBuf,
    pub code_mode_host: PathBuf,
    pub version: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum DesktopPlatform {
    Linux,
    Windows,
    Macos,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopLauncherConfig {
    pub schema_version: u32,
    pub manager_binary: PathBuf,
    pub app_server_binary: PathBuf,
    pub endpoint_file: PathBuf,
}

impl DesktopPaths {
    /// Discover the current user-level ChatGPT Desktop runtime.
    pub fn discover() -> Result<Self, DesktopError> {
        ensure_supported_platform()?;
        let codex_home = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex")))
            .ok_or_else(|| {
                DesktopError::InvalidPath("unable to determine the home directory".into())
            })?;
        if let Some(entrypoint) = std::env::var_os("CODEX_MP_DESKTOP_CODEX_BIN") {
            return Self::from_entrypoint(codex_home, entrypoint);
        }

        #[cfg(windows)]
        {
            return discover_windows(codex_home);
        }

        #[cfg(not(windows))]
        {
            #[allow(unused_mut)]
            let mut candidates = vec![
                codex_home
                    .join("packages")
                    .join("standalone")
                    .join("current")
                    .join("bin")
                    .join("codex"),
            ];
            #[cfg(target_os = "macos")]
            candidates.extend(macos_entrypoint_candidates());
            candidates
                .into_iter()
                .find(|candidate| candidate.exists())
                .map_or_else(
                    || Err(DesktopError::NotFound(candidates_display(&codex_home))),
                    |entrypoint| Self::from_entrypoint(codex_home.clone(), entrypoint),
                )
        }
    }

    /// Build a layout from a concrete entrypoint. This is also useful for
    /// tests and for a caller that has already resolved the `current` link.
    pub fn from_entrypoint(
        codex_home: impl Into<PathBuf>,
        entrypoint: impl Into<PathBuf>,
    ) -> Result<Self, DesktopError> {
        let codex_home = codex_home.into();
        let requested_entrypoint = entrypoint.into();
        let entrypoint = fs::canonicalize(&requested_entrypoint)
            .map_err(|_| DesktopError::NotFound(requested_entrypoint.clone()))?;
        let bin_dir = entrypoint
            .parent()
            .ok_or_else(|| DesktopError::InvalidPath(entrypoint.display().to_string()))?
            .to_path_buf();
        let release_dir = release_dir_for_entrypoint(&bin_dir)?;
        if !entrypoint.is_file() {
            return Err(DesktopError::NotFound(entrypoint));
        }
        let version = version_for_release(&release_dir)?;
        Ok(Self {
            platform: current_platform(),
            codex_home,
            release_dir,
            bin_dir: bin_dir.clone(),
            entrypoint,
            code_mode_host: code_mode_host_path(&bin_dir),
            version,
        })
    }

    pub fn launcher_config_path(&self) -> PathBuf {
        self.bin_dir.join(DESKTOP_LAUNCHER_CONFIG_FILE)
    }

    pub fn manifest_path_for_registry(registry_path: impl AsRef<Path>) -> PathBuf {
        registry_path
            .as_ref()
            .parent()
            .map(|path| path.join(DESKTOP_MANIFEST_FILE))
            .unwrap_or_else(|| PathBuf::from(DESKTOP_MANIFEST_FILE))
    }

    pub fn default_manifest_path() -> PathBuf {
        Self::manifest_path_for_registry(default_registry_path())
    }

    pub fn runtime_root_for_manifest(manifest_path: impl AsRef<Path>) -> PathBuf {
        manifest_path
            .as_ref()
            .parent()
            .map(|path| path.join(RUNTIME_DIR))
            .unwrap_or_else(|| PathBuf::from(RUNTIME_DIR))
    }

    /// Return the untouched binary that should be used to build the catalog.
    /// A managed install uses its verified backup; an unmanaged install uses
    /// the currently discovered Desktop binary.
    pub fn catalog_binary(&self, manifest_path: impl AsRef<Path>) -> Result<PathBuf, DesktopError> {
        let manifest_path = manifest_path.as_ref();
        if !manifest_path.exists() {
            ensure_native_entrypoint(&self.entrypoint)?;
            return Ok(self.entrypoint.clone());
        }
        let manifest = load_manifest(manifest_path)?;
        validate_manifest_target(&manifest, self)?;
        validate_backup_location(&manifest, manifest_path)?;
        verify_entrypoint_state(&manifest, self)?;
        verify_backup(&manifest)?;
        verify_launcher_config(&manifest)?;
        verify_launcher_binary(&manifest)?;
        verify_desktop_override(&manifest, self.platform)?;
        Ok(manifest.original_backup)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopManifest {
    pub schema_version: u32,
    pub release_dir: PathBuf,
    pub entrypoint: PathBuf,
    pub original_backup: PathBuf,
    pub original_sha256: String,
    pub launcher_sha256: String,
    pub runtime_dir: PathBuf,
    pub patched_app_server: PathBuf,
    pub patched_app_server_sha256: String,
    pub code_mode_host: PathBuf,
    pub code_mode_host_sha256: String,
    pub codex_mp_binary: PathBuf,
    pub endpoint_file: PathBuf,
    pub build_metadata: PathBuf,
    pub patch_sha256: String,
    pub upstream_commit: String,
    #[serde(default)]
    pub launcher_path: Option<PathBuf>,
    #[serde(default)]
    pub launcher_config: Option<PathBuf>,
    #[serde(default)]
    pub launcher_config_sha256: Option<String>,
    #[serde(default)]
    pub previous_codex_cli_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DesktopIntegrationState {
    Unmanaged,
    Managed,
    Drifted,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct DesktopStatus {
    pub platform: DesktopPlatform,
    pub state: DesktopIntegrationState,
    pub version: String,
    pub release_dir: PathBuf,
    pub entrypoint: PathBuf,
    pub manifest_path: PathBuf,
    pub current_sha256: String,
    pub launcher_sha256: Option<String>,
    pub launcher_path: Option<PathBuf>,
    pub active_pids: Vec<u32>,
}

#[derive(Debug, Clone)]
pub struct DesktopInstallOptions {
    pub manifest_path: PathBuf,
    pub app_server_binary: PathBuf,
    pub build_metadata: PathBuf,
    pub codex_mp_binary: PathBuf,
    pub endpoint_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DesktopInstallResult {
    pub manifest: DesktopManifest,
    pub catalog_binary: PathBuf,
    pub changed_entrypoint: bool,
}

pub fn load_manifest(path: impl AsRef<Path>) -> Result<DesktopManifest, DesktopError> {
    let path = path.as_ref();
    reject_symlink(path)?;
    let manifest: DesktopManifest = serde_json::from_str(&fs::read_to_string(path)?)?;
    if !matches!(
        manifest.schema_version,
        2 | 3 | DESKTOP_MANIFEST_SCHEMA_VERSION
    ) {
        return Err(DesktopError::Manifest(format!(
            "unsupported schema version {}",
            manifest.schema_version
        )));
    }
    if manifest.upstream_commit != PINNED_UPSTREAM_COMMIT {
        return Err(DesktopError::Manifest(format!(
            "patched runtime commit {} does not match {}",
            manifest.upstream_commit, PINNED_UPSTREAM_COMMIT
        )));
    }
    Ok(manifest)
}

pub fn status_for(
    paths: &DesktopPaths,
    manifest_path: impl AsRef<Path>,
) -> Result<DesktopStatus, DesktopError> {
    let manifest_path = manifest_path.as_ref().to_path_buf();
    let current_sha256 = sha256_file(&paths.entrypoint)?;
    let manifest = if manifest_path.exists() {
        Some(load_manifest(&manifest_path)?)
    } else {
        None
    };
    let active_pids = active_desktop_pids(paths, manifest.as_ref());
    let launcher_path = manifest
        .as_ref()
        .and_then(|manifest| manifest.launcher_path.clone());
    let (state, launcher_sha256) = match manifest {
        Some(manifest) => {
            let state = if validate_manifest_target(&manifest, paths).is_ok()
                && verify_entrypoint_state(&manifest, paths).is_ok()
                && verify_backup(&manifest).is_ok()
                && verify_launcher_config(&manifest).is_ok()
                && verify_launcher_binary(&manifest).is_ok()
                && verify_desktop_override(&manifest, paths.platform).is_ok()
                && verify_runtime(&manifest, &manifest_path).is_ok()
            {
                DesktopIntegrationState::Managed
            } else {
                DesktopIntegrationState::Drifted
            };
            (state, Some(manifest.launcher_sha256))
        }
        None => (
            if is_native_entrypoint(&paths.entrypoint) {
                DesktopIntegrationState::Unmanaged
            } else {
                DesktopIntegrationState::Drifted
            },
            None,
        ),
    };
    Ok(DesktopStatus {
        platform: paths.platform,
        state,
        version: paths.version.clone(),
        release_dir: paths.release_dir.clone(),
        entrypoint: paths.entrypoint.clone(),
        manifest_path,
        current_sha256,
        launcher_sha256,
        launcher_path,
        active_pids,
    })
}

pub fn install(options: &DesktopInstallOptions) -> Result<DesktopInstallResult, DesktopError> {
    let paths = DesktopPaths::discover()?;
    let app_server_binary = require_executable(&options.app_server_binary, "patched app-server")?;
    let build_metadata =
        validate_build_metadata(&options.build_metadata, Some(&app_server_binary))?;
    let codex_mp_binary = require_executable(&options.codex_mp_binary, "codex-mp")?;
    let host_binary = require_executable(&paths.code_mode_host, "Desktop code-mode host")?;
    let existing_manifest = if options.manifest_path.exists() {
        Some(load_manifest(&options.manifest_path)?)
    } else {
        None
    };
    let native_launcher = !matches!(paths.platform, DesktopPlatform::Linux);
    let previous_codex_cli_path = if native_launcher {
        let current_override = read_desktop_override()?;
        if existing_manifest.is_none() && current_override.is_some() {
            return Err(DesktopError::RuntimeInvalid(
                "CODEX_CLI_PATH is already set; remove it or restore the existing Desktop integration before installing"
                    .to_owned(),
            ));
        }
        match existing_manifest.as_ref() {
            Some(manifest) => manifest.previous_codex_cli_path.clone(),
            None => current_override,
        }
    } else {
        None
    };
    let running = active_desktop_pids(&paths, existing_manifest.as_ref());
    if !running.is_empty() {
        return Err(DesktopError::DesktopBusy(running));
    }
    let (original_backup, original_sha256, catalog_binary) = match existing_manifest.as_ref() {
        Some(manifest) => {
            validate_manifest_target(manifest, &paths)?;
            validate_backup_location(manifest, &options.manifest_path)?;
            verify_entrypoint_state(manifest, &paths)?;
            verify_backup(manifest)?;
            verify_launcher_config(manifest)?;
            verify_launcher_binary(manifest)?;
            verify_desktop_override(manifest, paths.platform)?;
            (
                manifest.original_backup.clone(),
                manifest.original_sha256.clone(),
                manifest.original_backup.clone(),
            )
        }
        None => {
            ensure_native_entrypoint(&paths.entrypoint)?;
            let original_sha256 = sha256_file(&paths.entrypoint)?;
            let entrypoint_name = paths
                .entrypoint
                .file_name()
                .ok_or_else(|| DesktopError::InvalidPath(paths.entrypoint.display().to_string()))?;
            let backup = options
                .manifest_path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join("desktop-backups")
                .join(&paths.version)
                .join(entrypoint_name);
            copy_file_atomic(&paths.entrypoint, &backup, true)?;
            (backup, original_sha256, paths.entrypoint.clone())
        }
    };

    let patched_hash = sha256_file(&app_server_binary)?;
    let runtime_dir = DesktopPaths::runtime_root_for_manifest(&options.manifest_path)
        .join(format!("{}-{}", paths.version, &patched_hash[..12]));
    let runtime_bin_dir = runtime_dir.join("bin");
    let managed_app_server = runtime_bin_dir.join(
        paths
            .entrypoint
            .file_name()
            .ok_or_else(|| DesktopError::InvalidPath(paths.entrypoint.display().to_string()))?,
    );
    let managed_host =
        runtime_bin_dir.join(paths.code_mode_host.file_name().ok_or_else(|| {
            DesktopError::InvalidPath(paths.code_mode_host.display().to_string())
        })?);
    copy_file_atomic(&app_server_binary, &managed_app_server, true)?;
    copy_file_atomic(&host_binary, &managed_host, true)?;
    let host_hash = sha256_file(&managed_host)?;

    let launcher_path = if native_launcher {
        managed_launcher_path(&options.manifest_path, &paths)?
    } else {
        paths.entrypoint.clone()
    };
    let launcher_config = if native_launcher {
        let config_path = launcher_path
            .parent()
            .map(|parent| parent.join(DESKTOP_LAUNCHER_CONFIG_FILE))
            .ok_or_else(|| DesktopError::InvalidPath(launcher_path.display().to_string()))?;
        let config = DesktopLauncherConfig {
            schema_version: 1,
            manager_binary: codex_mp_binary.clone(),
            app_server_binary: managed_app_server.clone(),
            endpoint_file: options.endpoint_file.clone(),
        };
        copy_file_atomic(&codex_mp_binary, &launcher_path, true)?;
        save_launcher_config(&config_path, &config)?;
        set_desktop_override(&launcher_path)?;
        let config_hash = sha256_file(&config_path)?;
        Some((config_path, config_hash))
    } else {
        let launcher = launcher_script(
            &codex_mp_binary,
            &managed_app_server,
            &options.endpoint_file,
        );
        write_executable_atomic(&launcher_path, launcher.as_bytes())?;
        None
    };
    let launcher_hash = sha256_file(&launcher_path)?;
    let manifest = DesktopManifest {
        schema_version: DESKTOP_MANIFEST_SCHEMA_VERSION,
        release_dir: paths.release_dir.clone(),
        entrypoint: paths.entrypoint.clone(),
        original_backup,
        original_sha256,
        launcher_sha256: launcher_hash,
        runtime_dir,
        patched_app_server: managed_app_server,
        patched_app_server_sha256: patched_hash,
        code_mode_host: managed_host,
        code_mode_host_sha256: host_hash,
        codex_mp_binary,
        endpoint_file: options.endpoint_file.clone(),
        build_metadata: options.build_metadata.clone(),
        patch_sha256: build_metadata.patch_sha256,
        upstream_commit: build_metadata.upstream_commit,
        launcher_path: native_launcher.then_some(launcher_path),
        launcher_config: launcher_config.as_ref().map(|(path, _)| path.clone()),
        launcher_config_sha256: launcher_config.map(|(_, hash)| hash),
        previous_codex_cli_path,
    };
    if let Err(error) = save_manifest(&options.manifest_path, &manifest) {
        // The launcher must never remain active without a manifest.
        if native_launcher {
            let _ = restore_desktop_override(&manifest, paths.platform);
            if let Some(path) = &manifest.launcher_config {
                let _ = fs::remove_file(path);
            }
            if let Some(path) = &manifest.launcher_path {
                let _ = fs::remove_file(path);
            }
        } else {
            copy_file_atomic(&manifest.original_backup, &paths.entrypoint, true).map_err(
                |rollback| DesktopError::Manifest(format!("{error}; rollback failed: {rollback}")),
            )?;
        }
        return Err(error);
    }
    Ok(DesktopInstallResult {
        manifest,
        catalog_binary,
        changed_entrypoint: !native_launcher,
    })
}

pub fn restore(
    paths: &DesktopPaths,
    manifest_path: impl AsRef<Path>,
) -> Result<bool, DesktopError> {
    let manifest_path = manifest_path.as_ref();
    if !manifest_path.exists() {
        return Ok(false);
    }
    let manifest = load_manifest(manifest_path)?;
    validate_manifest_target(&manifest, paths)?;
    validate_backup_location(&manifest, manifest_path)?;
    let running = active_desktop_pids(paths, Some(&manifest));
    if !running.is_empty() {
        return Err(DesktopError::DesktopBusy(running));
    }
    verify_entrypoint_state(&manifest, paths)?;
    verify_backup(&manifest)?;
    verify_launcher_config(&manifest)?;
    verify_launcher_binary(&manifest)?;
    verify_desktop_override(&manifest, paths.platform)?;
    let external_launcher = uses_external_launcher(&manifest, paths.platform);
    if external_launcher {
        restore_desktop_override(&manifest, paths.platform)?;
    }
    // Remove only files under our state directory before replacing the live
    // entrypoint. If this fails, keep the managed launcher active so the
    // manifest/runtime pair remains coherent for a later retry.
    if let Err(error) = remove_owned_runtime(&manifest, manifest_path) {
        if external_launcher {
            let _ = set_desktop_override(launcher_path(&manifest));
        }
        return Err(error);
    }
    if !external_launcher {
        copy_file_atomic(&manifest.original_backup, &paths.entrypoint, true)?;
    }
    if let Some(config_path) = &manifest.launcher_config {
        reject_symlink(config_path)?;
        if config_path.exists() {
            fs::remove_file(config_path)?;
        }
    }
    if let Some(launcher_path) = &manifest.launcher_path
        && external_launcher
    {
        reject_symlink(launcher_path)?;
        if launcher_path.exists() {
            fs::remove_file(launcher_path)?;
        }
    }
    fs::remove_file(&manifest.original_backup)?;
    fs::remove_file(manifest_path)?;
    Ok(true)
}

pub fn restore_if_present(manifest_path: impl AsRef<Path>) -> Result<bool, DesktopError> {
    let manifest_path = manifest_path.as_ref();
    if !manifest_path.exists() {
        return Ok(false);
    }
    let paths = DesktopPaths::discover()?;
    restore(&paths, manifest_path)
}

fn ensure_supported_platform() -> Result<(), DesktopError> {
    if cfg!(any(
        target_os = "linux",
        target_os = "windows",
        target_os = "macos"
    )) {
        Ok(())
    } else {
        Err(DesktopError::UnsupportedPlatform)
    }
}

fn current_platform() -> DesktopPlatform {
    #[cfg(target_os = "linux")]
    {
        DesktopPlatform::Linux
    }
    #[cfg(target_os = "windows")]
    {
        DesktopPlatform::Windows
    }
    #[cfg(target_os = "macos")]
    {
        DesktopPlatform::Macos
    }
}

#[cfg(not(windows))]
fn candidates_display(codex_home: &Path) -> PathBuf {
    codex_home
        .join("packages")
        .join("standalone")
        .join("current")
        .join("bin")
        .join("codex")
}

fn release_dir_for_entrypoint(bin_dir: &Path) -> Result<PathBuf, DesktopError> {
    #[cfg(target_os = "windows")]
    {
        if bin_dir.file_name().and_then(|name| name.to_str()) == Some("bin") {
            return bin_dir
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| DesktopError::InvalidPath(bin_dir.display().to_string()));
        }
        return Ok(bin_dir.to_path_buf());
    }
    #[cfg(target_os = "macos")]
    {
        let contents = bin_dir
            .parent()
            .ok_or_else(|| DesktopError::InvalidPath(bin_dir.display().to_string()))?;
        if contents.file_name().and_then(|name| name.to_str()) != Some("Contents") {
            return Ok(contents.to_path_buf());
        }
        return contents
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| DesktopError::InvalidPath(contents.display().to_string()));
    }
    #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
    {
        bin_dir
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| DesktopError::InvalidPath(bin_dir.display().to_string()))
    }
}

fn version_for_release(release_dir: &Path) -> Result<String, DesktopError> {
    let name = release_dir
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.trim().is_empty())
        .ok_or_else(|| DesktopError::InvalidPath(release_dir.display().to_string()))?;
    #[cfg(target_os = "macos")]
    {
        return Ok(name.strip_suffix(".app").unwrap_or(name).to_owned());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Ok(name.to_owned())
    }
}

fn code_mode_host_path(bin_dir: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        let with_extension = bin_dir.join(format!("{CODE_MODE_HOST}.exe"));
        if with_extension.exists() {
            return with_extension;
        }
    }
    bin_dir.join(CODE_MODE_HOST)
}

#[cfg(target_os = "macos")]
fn macos_entrypoint_candidates() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Some(home) = BaseDirs::new().map(|dirs| dirs.home_dir().to_path_buf()) {
        for app in ["ChatGPT.app", "Codex.app"] {
            candidates.push(
                home.join("Applications")
                    .join(app)
                    .join("Contents/Resources/codex"),
            );
        }
    }
    for app in ["ChatGPT.app", "Codex.app"] {
        candidates.push(
            PathBuf::from("/Applications")
                .join(app)
                .join("Contents/Resources/codex"),
        );
    }
    candidates
}

#[cfg(windows)]
fn discover_windows(codex_home: PathBuf) -> Result<DesktopPaths, DesktopError> {
    let local_app_data = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .ok_or_else(|| DesktopError::InvalidPath("LOCALAPPDATA is not set".into()))?;
    let root = local_app_data.join("OpenAI/Codex/bin");
    let mut candidates = fs::read_dir(&root)
        .map_err(|_| DesktopError::NotFound(root.clone()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_dir() && path.join("codex.exe").is_file() && code_mode_host_path(path).is_file()
        })
        .collect::<Vec<_>>();
    candidates.sort_by_key(|path| {
        fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH)
    });
    let entrypoint = candidates
        .pop()
        .map(|path| path.join("codex.exe"))
        .ok_or(DesktopError::NotFound(root))?;
    DesktopPaths::from_entrypoint(codex_home, entrypoint)
}

fn ensure_native_entrypoint(path: &Path) -> Result<(), DesktopError> {
    reject_symlink(path)?;
    if !is_native_entrypoint(path) {
        return Err(DesktopError::EntrypointNotNative(path.to_path_buf()));
    }
    Ok(())
}

fn is_native_entrypoint(path: &Path) -> bool {
    let Ok(mut file) = File::open(path) else {
        return false;
    };
    let mut magic = [0_u8; 4];
    if file.read_exact(&mut magic).is_err() {
        return false;
    }
    #[cfg(target_os = "windows")]
    {
        magic[..2] == *b"MZ"
    }
    #[cfg(target_os = "macos")]
    {
        matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
    }
    #[cfg(target_os = "linux")]
    {
        magic == *b"\x7fELF"
    }
}

fn require_executable(path: &Path, label: &str) -> Result<PathBuf, DesktopError> {
    let path = path.canonicalize().map_err(|_| match label {
        "codex-mp" => DesktopError::MissingManagerBinary(path.to_path_buf()),
        _ => DesktopError::MissingPatchedBinary(path.to_path_buf()),
    })?;
    if !path.is_file() {
        return Err(if label == "codex-mp" {
            DesktopError::MissingManagerBinary(path)
        } else {
            DesktopError::MissingPatchedBinary(path)
        });
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if path.metadata()?.permissions().mode() & 0o111 == 0 {
            return Err(if label == "codex-mp" {
                DesktopError::MissingManagerBinary(path)
            } else {
                DesktopError::MissingPatchedBinary(path)
            });
        }
    }
    Ok(path)
}

fn reject_symlink(path: &Path) -> Result<(), DesktopError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(DesktopError::InvalidPath(
            format!("`{}` must not be a symlink", path.display()),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn validate_manifest_target(
    manifest: &DesktopManifest,
    paths: &DesktopPaths,
) -> Result<(), DesktopError> {
    if manifest.release_dir != paths.release_dir || manifest.entrypoint != paths.entrypoint {
        return Err(DesktopError::Manifest(format!(
            "manifest targets `{}` but current Desktop is `{}`",
            manifest.entrypoint.display(),
            paths.entrypoint.display()
        )));
    }
    Ok(())
}

fn verify_backup(manifest: &DesktopManifest) -> Result<(), DesktopError> {
    if manifest.original_backup.is_symlink()
        || !manifest.original_backup.is_file()
        || sha256_file(&manifest.original_backup).ok().as_deref()
            != Some(manifest.original_sha256.as_str())
    {
        return Err(DesktopError::BackupInvalid(
            manifest.original_backup.clone(),
        ));
    }
    Ok(())
}

fn validate_backup_location(
    manifest: &DesktopManifest,
    manifest_path: &Path,
) -> Result<(), DesktopError> {
    let backup_root = manifest_path
        .parent()
        .map(|path| path.join("desktop-backups"))
        .unwrap_or_else(|| PathBuf::from("desktop-backups"));
    let invalid_location = !manifest.original_backup.starts_with(&backup_root)
        || manifest
            .original_backup
            .components()
            .any(|component| component == Component::ParentDir)
        || manifest
            .original_backup
            .file_name()
            .and_then(|name| name.to_str())
            != manifest
                .entrypoint
                .file_name()
                .and_then(|name| name.to_str());
    let within_canonical_root = fs::canonicalize(&backup_root)
        .ok()
        .zip(fs::canonicalize(&manifest.original_backup).ok())
        .is_some_and(|(root, backup)| backup.starts_with(root));
    if invalid_location || manifest.original_backup.is_symlink() || !within_canonical_root {
        return Err(DesktopError::BackupInvalid(
            manifest.original_backup.clone(),
        ));
    }
    Ok(())
}

fn save_launcher_config(path: &Path, config: &DesktopLauncherConfig) -> Result<(), DesktopError> {
    let bytes = serde_json::to_vec_pretty(config)?;
    write_private_atomic(path, &bytes)
}

fn verify_launcher_config(manifest: &DesktopManifest) -> Result<(), DesktopError> {
    match (&manifest.launcher_config, &manifest.launcher_config_sha256) {
        (None, None) => Ok(()),
        (Some(path), Some(expected_hash)) => {
            let launcher_path = manifest
                .launcher_path
                .as_ref()
                .unwrap_or(&manifest.entrypoint);
            let expected_path = manifest
                .launcher_path
                .as_ref()
                .unwrap_or(&manifest.entrypoint)
                .parent()
                .map(|parent| parent.join(DESKTOP_LAUNCHER_CONFIG_FILE))
                .ok_or_else(|| {
                    DesktopError::RuntimeInvalid(
                        "Desktop entrypoint has no parent directory".to_owned(),
                    )
                })?;
            if path != &expected_path
                || launcher_path.is_symlink()
                || !launcher_path.is_file()
                || path.is_symlink()
                || !path.is_file()
                || sha256_file(path).ok().as_deref() != Some(expected_hash.as_str())
            {
                return Err(DesktopError::RuntimeInvalid(path.display().to_string()));
            }
            let config: DesktopLauncherConfig = serde_json::from_str(&fs::read_to_string(path)?)?;
            if config.schema_version != 1
                || config.manager_binary != manifest.codex_mp_binary
                || config.app_server_binary != manifest.patched_app_server
                || config.endpoint_file != manifest.endpoint_file
                || config.manager_binary.is_symlink()
                || !config.manager_binary.is_file()
            {
                return Err(DesktopError::RuntimeInvalid(format!(
                    "invalid launcher configuration: {}",
                    path.display()
                )));
            }
            Ok(())
        }
        _ => Err(DesktopError::RuntimeInvalid(
            "launcher configuration path and hash must be present together".to_owned(),
        )),
    }
}

fn managed_launcher_path(
    manifest_path: &Path,
    paths: &DesktopPaths,
) -> Result<PathBuf, DesktopError> {
    let entrypoint_name = paths
        .entrypoint
        .file_name()
        .ok_or_else(|| DesktopError::InvalidPath(paths.entrypoint.display().to_string()))?;
    let root = manifest_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("desktop-launcher")
        .join(&paths.version);
    Ok(root.join(entrypoint_name))
}

fn uses_external_launcher(manifest: &DesktopManifest, platform: DesktopPlatform) -> bool {
    !matches!(platform, DesktopPlatform::Linux) && manifest.launcher_path.is_some()
}

fn launcher_path(manifest: &DesktopManifest) -> &Path {
    manifest
        .launcher_path
        .as_deref()
        .unwrap_or(&manifest.entrypoint)
}

fn verify_launcher_binary(manifest: &DesktopManifest) -> Result<(), DesktopError> {
    let launcher = launcher_path(manifest);
    if launcher.is_symlink()
        || !launcher.is_file()
        || sha256_file(launcher).ok().as_deref() != Some(manifest.launcher_sha256.as_str())
    {
        return Err(DesktopError::RuntimeInvalid(launcher.display().to_string()));
    }
    Ok(())
}

fn verify_entrypoint_state(
    manifest: &DesktopManifest,
    paths: &DesktopPaths,
) -> Result<(), DesktopError> {
    let current_hash = sha256_file(&paths.entrypoint)?;
    let expected_hash = if uses_external_launcher(manifest, paths.platform) {
        &manifest.original_sha256
    } else {
        &manifest.launcher_sha256
    };
    if current_hash != *expected_hash {
        return Err(DesktopError::ManagedEntrypointChanged(
            paths.entrypoint.clone(),
        ));
    }
    Ok(())
}

fn read_desktop_override() -> Result<Option<PathBuf>, DesktopError> {
    if let Some(value) = std::env::var_os("CODEX_CLI_PATH") {
        let value = PathBuf::from(value);
        if !value.as_os_str().is_empty() {
            return Ok(Some(value));
        }
    }
    #[cfg(windows)]
    {
        let output = std::process::Command::new("reg.exe")
            .args(["QUERY", r"HKCU\Environment", "/v", "CODEX_CLI_PATH"])
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&output.stdout);
        return Ok(text.lines().find_map(|line| {
            let mut fields = line
                .trim()
                .splitn(3, char::is_whitespace)
                .map(str::trim)
                .filter(|field| !field.is_empty());
            if fields.next()? != "CODEX_CLI_PATH" {
                return None;
            }
            fields.next()?;
            Some(PathBuf::from(fields.next()?))
        }));
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("launchctl")
            .args(["getenv", "CODEX_CLI_PATH"])
            .output()?;
        if !output.status.success() {
            return Ok(None);
        }
        let value = String::from_utf8_lossy(&output.stdout).trim().to_owned();
        return Ok((!value.is_empty()).then(|| PathBuf::from(value)));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    {
        Ok(None)
    }
}

fn set_desktop_override(path: &Path) -> Result<(), DesktopError> {
    #[cfg(windows)]
    {
        let status = std::process::Command::new("reg.exe")
            .args([
                "ADD",
                r"HKCU\Environment",
                "/v",
                "CODEX_CLI_PATH",
                "/t",
                "REG_SZ",
                "/d",
            ])
            .arg(path)
            .args(["/f"])
            .status()?;
        if !status.success() {
            return Err(DesktopError::RuntimeInvalid(
                "unable to set the Windows user CODEX_CLI_PATH override".to_owned(),
            ));
        }
        let _ = broadcast_windows_environment_change();
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("launchctl")
            .args(["setenv", "CODEX_CLI_PATH"])
            .arg(path)
            .status()?;
        if !status.success() {
            return Err(DesktopError::RuntimeInvalid(
                "unable to set the macOS launchctl CODEX_CLI_PATH override".to_owned(),
            ));
        }
        let _ = persist_macos_environment_override(Some(path));
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    let _ = path;
    Ok(())
}

fn verify_desktop_override(
    manifest: &DesktopManifest,
    platform: DesktopPlatform,
) -> Result<(), DesktopError> {
    if !uses_external_launcher(manifest, platform) {
        return Ok(());
    }
    let actual = read_desktop_override()?;
    if actual
        .as_deref()
        .is_none_or(|path| !paths_equal_for_platform(path, launcher_path(manifest), platform))
    {
        return Err(DesktopError::RuntimeInvalid(
            "CODEX_CLI_PATH does not point to the managed Desktop launcher".to_owned(),
        ));
    }
    Ok(())
}

fn restore_desktop_override(
    manifest: &DesktopManifest,
    platform: DesktopPlatform,
) -> Result<(), DesktopError> {
    if !uses_external_launcher(manifest, platform) {
        return Ok(());
    }
    if let Some(previous) = &manifest.previous_codex_cli_path {
        return set_desktop_override(previous);
    }
    #[cfg(windows)]
    {
        let status = std::process::Command::new("reg.exe")
            .args(["DELETE", r"HKCU\Environment", "/v", "CODEX_CLI_PATH", "/f"])
            .status()?;
        if !status.success() {
            return Err(DesktopError::RuntimeInvalid(
                "unable to remove the Windows user CODEX_CLI_PATH override".to_owned(),
            ));
        }
        let _ = broadcast_windows_environment_change();
    }
    #[cfg(target_os = "macos")]
    {
        let status = std::process::Command::new("launchctl")
            .args(["unsetenv", "CODEX_CLI_PATH"])
            .status()?;
        if !status.success() {
            return Err(DesktopError::RuntimeInvalid(
                "unable to remove the macOS launchctl CODEX_CLI_PATH override".to_owned(),
            ));
        }
        let _ = persist_macos_environment_override(None);
    }
    Ok(())
}

#[cfg(windows)]
fn broadcast_windows_environment_change() -> Result<(), DesktopError> {
    // Explorer is normally already running when a user installs the adapter.
    // Broadcast the standard notification so a subsequent Start-menu launch
    // receives the new user-level CODEX_CLI_PATH without requiring a logoff
    // or an Explorer restart.
    let script = r#"
$signature = @'
using System;
using System.Runtime.InteropServices;
public static class CodexMpEnvironmentBroadcast {
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    public static extern IntPtr SendMessageTimeout(
        IntPtr hWnd, uint msg, IntPtr wParam, string lParam,
        uint flags, uint timeout, out IntPtr result);
}
'@
Add-Type -TypeDefinition $signature
$result = [IntPtr]::Zero
$sent = [CodexMpEnvironmentBroadcast]::SendMessageTimeout(
    [IntPtr]0xffff, 0x1a, [IntPtr]::Zero, "Environment", 0x2, 5000,
    [ref]$result)
if ($sent -eq [IntPtr]::Zero) { exit 1 }
"#;
    let status = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .status()?;
    if !status.success() {
        return Err(DesktopError::RuntimeInvalid(
            "unable to broadcast the Windows environment change".to_owned(),
        ));
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn persist_macos_environment_override(path: Option<&Path>) -> Result<(), DesktopError> {
    let Some(base_dirs) = BaseDirs::new() else {
        return Ok(());
    };
    let launch_agents = base_dirs.home_dir().join("Library/LaunchAgents");
    let plist_path = launch_agents.join("dev.codex-multiprovider.env.plist");
    match path {
        Some(target) => {
            let _ = fs::create_dir_all(&launch_agents);
            let plist_content = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>dev.codex-multiprovider.env</string>
    <key>ProgramArguments</key>
    <array>
        <string>launchctl</string>
        <string>setenv</string>
        <string>CODEX_CLI_PATH</string>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
                target.display()
            );
            let _ = fs::write(&plist_path, plist_content);
        }
        None => {
            if plist_path.exists() {
                let _ = fs::remove_file(&plist_path);
            }
        }
    }
    Ok(())
}

fn paths_equal_for_platform(left: &Path, right: &Path, platform: DesktopPlatform) -> bool {
    let left = fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    match platform {
        DesktopPlatform::Windows => left
            .to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy()),
        DesktopPlatform::Linux | DesktopPlatform::Macos => left == right,
    }
}

fn verify_runtime(manifest: &DesktopManifest, manifest_path: &Path) -> Result<(), DesktopError> {
    validate_runtime_location(manifest, manifest_path)?;
    if !manifest.runtime_dir.is_dir()
        || !manifest.patched_app_server.is_file()
        || !manifest.code_mode_host.is_file()
        || manifest.patched_app_server.is_symlink()
        || manifest.code_mode_host.is_symlink()
        || sha256_file(&manifest.patched_app_server).ok().as_deref()
            != Some(manifest.patched_app_server_sha256.as_str())
        || sha256_file(&manifest.code_mode_host).ok().as_deref()
            != Some(manifest.code_mode_host_sha256.as_str())
        || validate_build_metadata(&manifest.build_metadata, None)
            .map(|metadata| metadata.patch_sha256 != manifest.patch_sha256)
            .unwrap_or(true)
    {
        return Err(DesktopError::RuntimeInvalid(
            manifest.runtime_dir.display().to_string(),
        ));
    }
    Ok(())
}

fn validate_runtime_location(
    manifest: &DesktopManifest,
    manifest_path: &Path,
) -> Result<(), DesktopError> {
    let runtime_root = DesktopPaths::runtime_root_for_manifest(manifest_path);
    let invalid_location = manifest.runtime_dir == runtime_root
        || manifest.runtime_dir.is_symlink()
        || !manifest.runtime_dir.starts_with(&runtime_root)
        || manifest
            .runtime_dir
            .components()
            .any(|component| component == Component::ParentDir)
        || !manifest
            .patched_app_server
            .starts_with(&manifest.runtime_dir)
        || !manifest.code_mode_host.starts_with(&manifest.runtime_dir)
        || manifest
            .patched_app_server
            .components()
            .any(|component| component == Component::ParentDir)
        || manifest
            .code_mode_host
            .components()
            .any(|component| component == Component::ParentDir);
    if invalid_location {
        return Err(DesktopError::RuntimeInvalid(
            manifest.runtime_dir.display().to_string(),
        ));
    }
    if manifest.runtime_dir.exists() {
        let within_canonical_root = fs::canonicalize(&runtime_root)
            .ok()
            .zip(fs::canonicalize(&manifest.runtime_dir).ok())
            .is_some_and(|(root, runtime)| runtime.starts_with(root));
        let files_within_runtime = fs::canonicalize(&manifest.runtime_dir)
            .ok()
            .zip(fs::canonicalize(&manifest.patched_app_server).ok())
            .zip(fs::canonicalize(&manifest.code_mode_host).ok())
            .is_some_and(|((runtime, app_server), host)| {
                app_server.starts_with(&runtime) && host.starts_with(&runtime)
            });
        if !within_canonical_root || !files_within_runtime {
            return Err(DesktopError::RuntimeInvalid(
                manifest.runtime_dir.display().to_string(),
            ));
        }
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
struct BuildMetadata {
    schema_version: u32,
    upstream_commit: String,
    app_server_binary: String,
    patch_sha256: String,
    official_codex_binary_untouched: bool,
}

fn validate_build_metadata(
    path: &Path,
    app_server_binary: Option<&Path>,
) -> Result<BuildMetadata, DesktopError> {
    reject_symlink(path)?;
    let metadata: BuildMetadata = serde_json::from_str(&fs::read_to_string(path)?)
        .map_err(|error| DesktopError::BuildMetadata(format!("{}: {error}", path.display())))?;
    let binary_name = app_server_binary
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str());
    if metadata.schema_version != 1
        || metadata.upstream_commit != PINNED_UPSTREAM_COMMIT
        || binary_name.is_some_and(|name| metadata.app_server_binary != name)
        || metadata.patch_sha256.trim().is_empty()
        || !metadata.official_codex_binary_untouched
    {
        return Err(DesktopError::BuildMetadata(format!(
            "{} does not describe the pinned app-server binary{}",
            path.display(),
            binary_name
                .map(|name| format!(" `{name}`"))
                .unwrap_or_default()
        )));
    }
    Ok(metadata)
}

fn launcher_script(
    codex_mp_binary: &Path,
    app_server_binary: &Path,
    endpoint_file: &Path,
) -> String {
    format!(
        "#!/bin/sh\nset -eu\n\nCODEX_MP_BIN={}\nPATCHED_APP_SERVER={}\nDEFAULT_ENDPOINT={}\nENDPOINT_FILE=\"${{CODEX_MP_ROUTER_ENDPOINT_FILE:-$DEFAULT_ENDPOINT}}\"\n\nexec \"$CODEX_MP_BIN\" launch --app-server-binary \"$PATCHED_APP_SERVER\" --endpoint-file \"$ENDPOINT_FILE\" -- \"$@\"\n",
        shell_quote(codex_mp_binary),
        shell_quote(app_server_binary),
        shell_quote(endpoint_file),
    )
}

fn shell_quote(path: &Path) -> String {
    let value = path.to_string_lossy();
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn save_manifest(path: &Path, manifest: &DesktopManifest) -> Result<(), DesktopError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let bytes = serde_json::to_vec_pretty(manifest)?;
    write_private_atomic(path, &bytes)
}

fn write_private_atomic(path: &Path, bytes: &[u8]) -> Result<(), DesktopError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)?;
    file.write_all(bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    set_private_permissions(&temp)?;
    atomic_replace(&temp, path)?;
    set_private_permissions(path)?;
    Ok(())
}

fn write_executable_atomic(path: &Path, bytes: &[u8]) -> Result<(), DesktopError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    reject_symlink(path)?;
    let temp = temp_path(path);
    fs::write(&temp, bytes)?;
    set_executable_permissions(&temp)?;
    atomic_replace(&temp, path)?;
    set_executable_permissions(path)?;
    Ok(())
}

fn copy_file_atomic(
    source: &Path,
    destination: &Path,
    executable: bool,
) -> Result<(), DesktopError> {
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    reject_symlink(destination)?;
    let temp = temp_path(destination);
    fs::copy(source, &temp)?;
    if executable {
        set_executable_permissions(&temp)?;
    } else {
        set_private_permissions(&temp)?;
    }
    atomic_replace(&temp, destination)?;
    if executable {
        set_executable_permissions(destination)?;
    } else {
        set_private_permissions(destination)?;
    }
    Ok(())
}

fn set_executable_permissions(path: &Path) -> Result<(), io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn temp_path(path: &Path) -> PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file"),
        std::process::id(),
        stamp
    ))
}

fn sha256_file(path: &Path) -> Result<String, DesktopError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 1024 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        FmtWrite::write_fmt(&mut digest, format_args!("{byte:02x}"))
            .expect("writing a SHA-256 digest to a String cannot fail");
    }
    Ok(digest)
}

fn remove_owned_runtime(
    manifest: &DesktopManifest,
    manifest_path: &Path,
) -> Result<(), DesktopError> {
    validate_runtime_location(manifest, manifest_path)?;
    if manifest.runtime_dir.exists() {
        fs::remove_dir_all(&manifest.runtime_dir)?;
    }
    Ok(())
}

fn active_desktop_pids(paths: &DesktopPaths, manifest: Option<&DesktopManifest>) -> Vec<u32> {
    let mut watched = vec![paths.entrypoint.as_path(), paths.code_mode_host.as_path()];
    if let Some(manifest) = manifest {
        watched.push(manifest.patched_app_server.as_path());
        if let Some(launcher) = manifest.launcher_path.as_deref() {
            watched.push(launcher);
        }
    }
    active_pids(&watched)
}

#[cfg(target_os = "linux")]
fn active_pids(paths: &[&Path]) -> Vec<u32> {
    let targets = paths
        .iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Vec::new();
    }
    let mut pids = fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| entry.file_name().to_str()?.parse::<u32>().ok())
        .filter(|pid| {
            fs::canonicalize(format!("/proc/{pid}/exe"))
                .is_ok_and(|executable| targets.iter().any(|target| target == &executable))
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids
}

#[cfg(target_os = "windows")]
fn active_pids(paths: &[&Path]) -> Vec<u32> {
    let targets = paths
        .iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Vec::new();
    }

    // Fast path: use tasklist.exe with CSV format and /NH (no header)
    // to quickly identify candidate PIDs by image name without PowerShell CIM overhead.
    let tasklist_output = std::process::Command::new("tasklist.exe")
        .args(["/FO", "CSV", "/NH"])
        .output();

    if let Ok(output) = tasklist_output
        && output.status.success()
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let target_filenames = targets
            .iter()
            .filter_map(|t| t.file_name().and_then(|f| f.to_str()))
            .collect::<Vec<_>>();

        let mut candidate_pids = Vec::new();
        for line in stdout.lines() {
            let fields = line
                .split(',')
                .map(|f| f.trim().trim_matches('"'))
                .collect::<Vec<_>>();
            if fields.len() >= 2 {
                let image_name = fields[0];
                if target_filenames
                    .iter()
                    .any(|target_name| image_name.eq_ignore_ascii_case(target_name))
                    && let Ok(pid) = fields[1].parse::<u32>()
                {
                    candidate_pids.push(pid);
                }
            }
        }

        if candidate_pids.is_empty() {
            return Vec::new();
        }

        // If candidates found, query exact ExecutablePath for only these candidate PIDs
        let pid_filter = candidate_pids
            .iter()
            .map(|pid| format!("ProcessId = {pid}"))
            .collect::<Vec<_>>()
            .join(" or ");
        let script = format!(
            "$ProgressPreference='SilentlyContinue'; Get-CimInstance Win32_Process -Filter \"{pid_filter}\" | Select-Object ProcessId,ExecutablePath | ConvertTo-Json -Compress"
        );
        if let Ok(output) = std::process::Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command", &script])
            .output()
            && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout)
        {
            let processes = value.as_array().cloned().unwrap_or_else(|| vec![value]);
            let mut pids = processes
                .into_iter()
                .filter_map(|process| {
                    let executable = process.get("ExecutablePath")?.as_str()?;
                    let executable =
                        fs::canonicalize(executable).unwrap_or_else(|_| PathBuf::from(executable));
                    if !targets
                        .iter()
                        .any(|target| paths_equal(target, &executable))
                    {
                        return None;
                    }
                    process
                        .get("ProcessId")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|pid| u32::try_from(pid).ok())
                })
                .collect::<Vec<_>>();
            pids.sort_unstable();
            pids.dedup();
            return pids;
        }
    }

    // Fallback: full CIM scan
    let output = std::process::Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            "$ProgressPreference='SilentlyContinue'; Get-CimInstance Win32_Process | Select-Object ProcessId,ExecutablePath | ConvertTo-Json -Compress",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&output.stdout) else {
        return Vec::new();
    };
    let processes = value.as_array().cloned().unwrap_or_else(|| vec![value]);
    let mut pids = processes
        .into_iter()
        .filter_map(|process| {
            let executable = process.get("ExecutablePath")?.as_str()?;
            let executable =
                fs::canonicalize(executable).unwrap_or_else(|_| PathBuf::from(executable));
            if !targets
                .iter()
                .any(|target| paths_equal(target, &executable))
            {
                return None;
            }
            process
                .get("ProcessId")
                .and_then(serde_json::Value::as_u64)
                .and_then(|pid| u32::try_from(pid).ok())
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(target_os = "macos")]
fn active_pids(paths: &[&Path]) -> Vec<u32> {
    let targets = paths
        .iter()
        .filter_map(|path| fs::canonicalize(path).ok())
        .collect::<Vec<_>>();
    if targets.is_empty() {
        return Vec::new();
    }
    let Ok(output) = std::process::Command::new("ps")
        .args(["-axo", "pid=,command="])
        .output()
    else {
        return Vec::new();
    };
    let mut pids = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse::<u32>().ok()?;
            let executable = PathBuf::from(fields.next()?);
            let executable = fs::canonicalize(&executable).unwrap_or(executable);
            targets
                .iter()
                .any(|target| paths_equal(target, &executable))
                .then_some(pid)
        })
        .collect::<Vec<_>>();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(any(target_os = "windows", target_os = "macos"))]
fn paths_equal(left: &Path, right: &Path) -> bool {
    #[cfg(target_os = "windows")]
    {
        left.to_string_lossy()
            .eq_ignore_ascii_case(&right.to_string_lossy())
    }
    #[cfg(target_os = "macos")]
    {
        left == right
    }
}

#[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
fn active_pids(_paths: &[&Path]) -> Vec<u32> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn elf_stub(path: &Path) {
        let mut bytes = vec![0_u8; 64];
        #[cfg(target_os = "windows")]
        bytes[..2].copy_from_slice(b"MZ");
        #[cfg(target_os = "macos")]
        bytes[..4].copy_from_slice(&[0xcf, 0xfa, 0xed, 0xfe]);
        #[cfg(target_os = "linux")]
        bytes[..4].copy_from_slice(b"\x7fELF");
        fs::write(path, bytes).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
        }
    }

    #[test]
    fn discovers_standalone_layout_and_manifest_path() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".codex");
        let entrypoint = root.join("packages/standalone/releases/0.154.0-desktop/bin/codex");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        elf_stub(&entrypoint);

        let paths = DesktopPaths::from_entrypoint(&root, &entrypoint).unwrap();
        assert_eq!(paths.version, "0.154.0-desktop");
        assert_eq!(
            DesktopPaths::manifest_path_for_registry(directory.path().join("providers.json")),
            directory.path().join("desktop-integration.json")
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolves_the_current_release_link_before_recording_the_manifest() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".codex");
        let release_entrypoint =
            root.join("packages/standalone/releases/0.154.0-desktop/bin/codex");
        let current_entrypoint = root.join("packages/standalone/current/bin/codex");
        fs::create_dir_all(release_entrypoint.parent().unwrap()).unwrap();
        elf_stub(&release_entrypoint);
        std::os::unix::fs::symlink(
            root.join("packages/standalone/releases/0.154.0-desktop"),
            root.join("packages/standalone/current"),
        )
        .unwrap();

        let paths = DesktopPaths::from_entrypoint(&root, &current_entrypoint).unwrap();
        assert_eq!(paths.version, "0.154.0-desktop");
        assert_eq!(
            paths.entrypoint,
            fs::canonicalize(release_entrypoint).unwrap()
        );
    }

    #[test]
    fn launcher_quotes_paths_and_preserves_arguments() {
        let script = launcher_script(
            Path::new("/tmp/Codex MP/codex-mp"),
            Path::new("/tmp/runtime/codex"),
            Path::new("/tmp/router endpoint.json"),
        );
        assert!(script.contains("'/tmp/Codex MP/codex-mp'"));
        assert!(script.contains("\"$@\""));
    }

    #[test]
    fn unmanaged_status_rejects_a_shell_wrapper() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".codex");
        let entrypoint = root.join("packages/standalone/releases/v/bin/codex");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        fs::write(&entrypoint, b"#!/bin/sh\nexec codex\n").unwrap();
        let paths = DesktopPaths::from_entrypoint(&root, &entrypoint).unwrap();
        let status = status_for(&paths, directory.path().join("manifest.json")).unwrap();
        assert_eq!(status.state, DesktopIntegrationState::Drifted);
    }

    #[test]
    fn build_metadata_must_match_the_pinned_app_server() {
        let directory = tempdir().unwrap();
        let metadata = directory.path().join("codex-mp-build.json");
        fs::write(
            &metadata,
            serde_json::json!({
                "schema_version": 1,
                "upstream_commit": "wrong",
                "app_server_binary": "codex-mp-app-server-bin",
                "patch_sha256": "abc",
                "official_codex_binary_untouched": true
            })
            .to_string(),
        )
        .unwrap();
        let binary = directory.path().join("codex-mp-app-server-bin");
        elf_stub(&binary);
        assert!(matches!(
            validate_build_metadata(&metadata, Some(&binary)),
            Err(DesktopError::BuildMetadata(_))
        ));
    }
}
