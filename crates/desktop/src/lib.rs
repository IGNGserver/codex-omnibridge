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

use codex_mp_core::{FileLock, atomic_replace, default_registry_path, set_private_permissions};
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
        verify_pinned_runtime(&manifest)?;
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
    /// Two-phase-commit marker.
    ///
    /// Everything that changes the machine — the launcher, `CODEX_CLI_PATH`, and
    /// on Linux the real `codex` entrypoint — happens before the manifest is
    /// written. A crash in between left the override active with no manifest, so
    /// `install` refused ("CODEX_CLI_PATH is already set") and
    /// `restore_if_present` returned `false`: the tool could not undo its own
    /// change. Writing a `pending: true` manifest *before* touching anything makes
    /// that window recoverable.
    #[serde(default)]
    pub pending: bool,
    /// Platform the adapter was installed on.
    ///
    /// Recorded so teardown does not have to re-discover the Desktop install.
    /// `restore_if_present` called `DesktopPaths::discover()`, which fails once
    /// the Desktop app is uninstalled or moved, and that failure aborted the
    /// whole `codex-mp uninstall` sequence *before* the Codex config was restored
    /// — leaving `config.toml` hijacked with no working path back.
    #[serde(default = "default_platform")]
    pub platform: DesktopPlatform,
}

fn default_platform() -> DesktopPlatform {
    // Manifests written before this field existed: infer from the build target,
    // which is where they were written.
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
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        DesktopPlatform::Linux
    }
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

/// Read and structurally validate an integration manifest.
///
/// This deliberately does **not** check the pinned upstream commit. Teardown
/// must work for an adapter installed by *any* release, and `upstream_commit` is
/// bumped whenever the pinned Codex build moves. Rejecting it here meant an
/// adapter from an older release could never be removed: `restore`,
/// `restore_if_present`, `status` and `catalog_binary` all failed at load, so the
/// user could neither uninstall nor reinstall without hand-deleting the manifest.
/// Callers that are about to *use* the patched runtime call
/// [`verify_pinned_runtime`] instead.
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
    Ok(manifest)
}

/// Reject a manifest whose patched runtime was built from a different upstream
/// commit. Only meaningful when the patched runtime is actually going to be run.
fn verify_pinned_runtime(manifest: &DesktopManifest) -> Result<(), DesktopError> {
    if manifest.upstream_commit != PINNED_UPSTREAM_COMMIT {
        return Err(DesktopError::Manifest(format!(
            "patched runtime commit {} does not match {}",
            manifest.upstream_commit, PINNED_UPSTREAM_COMMIT
        )));
    }
    Ok(())
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
            // A manifest for a superseded Desktop release is reported as
            // `Unmanaged` rather than `Drifted`: `status` is how the user learns
            // what to do, and `Drifted` with an unusable manifest is a dead end
            // (`codex-mp desktop restore` will clear it).
            let stale_target = validate_manifest_target(&manifest, paths).is_err();
            let state = if stale_target {
                if is_native_entrypoint(&paths.entrypoint) {
                    DesktopIntegrationState::Unmanaged
                } else {
                    DesktopIntegrationState::Drifted
                }
            } else if verify_entrypoint_state(&manifest, paths).is_ok()
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
    install_into(&paths, options)
}

/// `install` against an explicit layout.
///
/// Split out so tests never touch the machine's real Desktop install: the public
/// entry point resolves `DesktopPaths::discover()`, which on a developer machine
/// points at a live ChatGPT Desktop, and a test that called it directly would be
/// mutating real user state.
pub(crate) fn install_into(
    paths: &DesktopPaths,
    options: &DesktopInstallOptions,
) -> Result<DesktopInstallResult, DesktopError> {
    let paths = paths.clone();
    // One exclusive lock covers the whole install transaction. Without it a
    // concurrent `restore` (or a second install from the panel) could interleave,
    // leaving the launcher/override applied while a different manifest revision
    // is on disk — the state the verification guards then reject for ever.
    let _lock = FileLock::acquire(&options.manifest_path)?;
    let app_server_binary = require_executable(&options.app_server_binary, "patched app-server")?;
    let build_metadata =
        validate_build_metadata(&options.build_metadata, Some(&app_server_binary))?;
    let codex_mp_binary = require_executable(&options.codex_mp_binary, "codex-mp")?;
    let host_binary = require_executable(&paths.code_mode_host, "Desktop code-mode host")?;
    let existing_manifest = if options.manifest_path.exists() {
        let manifest = load_manifest(&options.manifest_path)?;
        // An install reuses the existing runtime, so it must refuse a manifest
        // whose patched binary came from a different upstream build.
        verify_pinned_runtime(&manifest)?;
        Some(manifest)
    } else {
        None
    };
    // A manifest naming a Desktop release that no longer exists cannot be
    // installed on top of or verified — clear it first so a Desktop update does
    // not permanently block reinstalling the adapter.
    if recover_stale_manifest_if_present(&paths, &options.manifest_path)? {
        eprintln!(
            "codex-mp: recovered a Desktop adapter manifest left over from a previous \
             release (the Desktop app was updated); installing fresh"
        );
    }
    let existing_manifest = match existing_manifest {
        // Re-read only if we just cleared it.
        Some(_) if !options.manifest_path.exists() => None,
        other => other,
    };
    // An interrupted install leaves a `pending` manifest. Its hashes describe a
    // half-applied state, so `verify_*` cannot validate it and re-running install
    // would build on sand. Point the user at `restore`, which knows how to finish
    // or undo an interrupted install.
    if let Some(manifest) = existing_manifest.as_ref()
        && manifest.pending
    {
        return Err(DesktopError::Manifest(
            "a previous Desktop adapter install was interrupted; run \
             `codex-mp desktop restore` to return to the official runtime, then install again"
                .to_owned(),
        ));
    }
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
            // No manifest, but the entrypoint may still be *our* launcher from an
            // interrupted install or a hand-cleaned config directory. In that
            // state `install` refused (`EntrypointNotNative` — our launcher is a
            // script, not an ELF) and `restore` reported "no manifest", leaving no
            // CLI path back to a working Desktop at all.
            //
            // Our launcher is recognisable, and the displaced binary sits next to
            // it, so the orphan can be healed here instead of stranding the user.
            if restore_orphaned_launcher(&paths, &options.manifest_path)? {
                eprintln!(
                    "codex-mp: found an OmniBridge Desktop launcher at {} with no manifest; \
                     restored the original entrypoint",
                    paths.entrypoint.display()
                );
            }
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

    // Phase 1: write the intent record *before* anything on the machine changes.
    // Every hash the manifest records is computed from the content we are about
    // to write, so a crash at any later point still leaves a manifest that
    // describes a recoverable state.
    let (launcher_config_path, launcher_config_hash, launcher_bytes) = if native_launcher {
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
        let config_bytes = serde_json::to_vec_pretty(&config)?;
        let config_hash = sha256_bytes(&config_bytes);
        (Some((config_path, config_bytes)), Some(config_hash), None)
    } else {
        let launcher = launcher_script(
            &codex_mp_binary,
            &managed_app_server,
            &options.endpoint_file,
        );
        let hash = sha256_bytes(launcher.as_bytes());
        (None, None, Some((launcher.into_bytes(), hash)))
    };
    let launcher_hash = match (&launcher_bytes, &launcher_config_hash) {
        (Some((_, hash)), _) => hash.clone(),
        (None, Some(_)) => sha256_file(&codex_mp_binary)?,
        (None, None) => sha256_file(&launcher_path)?,
    };

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
        codex_mp_binary: codex_mp_binary.clone(),
        endpoint_file: options.endpoint_file.clone(),
        build_metadata: options.build_metadata.clone(),
        patch_sha256: build_metadata.patch_sha256,
        upstream_commit: build_metadata.upstream_commit,
        launcher_path: native_launcher.then_some(launcher_path.clone()),
        launcher_config: launcher_config_path.as_ref().map(|(path, _)| path.clone()),
        launcher_config_sha256: launcher_config_hash,
        previous_codex_cli_path,
        pending: true,
        platform: paths.platform,
    };
    save_manifest(&options.manifest_path, &manifest)?;

    // Phase 2: apply. Any error rolls the machine back to the official runtime
    // and removes the intent record, so a failed install never leaves the
    // override active without a manifest.
    let applied = (|| -> Result<(), DesktopError> {
        if native_launcher {
            if let Some((config_path, config_bytes)) = launcher_config_path.as_ref() {
                copy_file_atomic(&codex_mp_binary, &launcher_path, true)?;
                write_private_atomic(config_path, config_bytes)?;
                set_desktop_override(&launcher_path)?;
            }
        } else if let Some((bytes, _)) = launcher_bytes.as_ref() {
            write_executable_atomic(&launcher_path, bytes)?;
        }
        Ok(())
    })();

    if let Err(error) = applied {
        // Best-effort rollback, mirroring the old error path.
        if native_launcher {
            let _ = restore_desktop_override(&manifest, paths.platform);
            if let Some(path) = &manifest.launcher_config {
                let _ = fs::remove_file(path);
            }
            let _ = fs::remove_file(&launcher_path);
        } else {
            let _ = copy_file_atomic(&manifest.original_backup, &paths.entrypoint, true);
        }
        let _ = fs::remove_file(&options.manifest_path);
        return Err(error);
    }

    // Phase 3: confirm.
    let confirmed = DesktopManifest {
        pending: false,
        ..manifest
    };
    save_manifest(&options.manifest_path, &confirmed)?;
    Ok(DesktopInstallResult {
        manifest: confirmed,
        catalog_binary,
        changed_entrypoint: !native_launcher,
    })
}

/// What a path currently holds, relative to a manifest we own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntrypointState {
    /// Our managed launcher is installed there.
    Managed,
    /// The original untouched binary is there (already restored).
    Original,
    /// Neither: something changed it outside this tool.
    Foreign,
    /// The path does not exist.
    Absent,
}

fn entrypoint_state(manifest: &DesktopManifest, path: &Path) -> EntrypointState {
    if !path.exists() {
        return EntrypointState::Absent;
    }
    match sha256_file(path) {
        Ok(hash) if hash == manifest.launcher_sha256 => EntrypointState::Managed,
        Ok(hash) if hash == manifest.original_sha256 => EntrypointState::Original,
        _ => EntrypointState::Foreign,
    }
}

/// True when the Desktop override (if any) points at our launcher.
fn desktop_override_is_ours(
    manifest: &DesktopManifest,
    platform: DesktopPlatform,
) -> Result<bool, DesktopError> {
    let Some(current) = read_desktop_override()? else {
        return Ok(false);
    };
    Ok(paths_equal_for_platform(
        &current,
        launcher_path(manifest),
        platform,
    ))
}

fn remove_if_present(path: &Path) -> Result<(), DesktopError> {
    reject_symlink(path)?;
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

/// Undo a Desktop adapter install.
///
/// Written to be **re-runnable at any point**. The old implementation verified
/// the pre-restore state up front and deleted the manifest last, so a crash (or a
/// failure) after it had already restored the entrypoint or cleared the override
/// made every retry fail: on Windows/macOS with "CODEX_CLI_PATH does not point to
/// the managed Desktop launcher", on Linux with `ManagedEntrypointChanged` (and
/// once the backup was deleted but the manifest survived, with `BackupInvalid`).
/// Desktop was then stuck in `Drifted` for ever, and because the CLI restores
/// Desktop *before* the Codex config, that also blocked the config restore.
///
/// Each step now inspects the current state and acts only when there is work to
/// do, and the ordering puts the user-visible recovery (entrypoint/override)
/// first and destruction (backup, manifest) last.
pub fn restore(
    paths: &DesktopPaths,
    manifest_path: impl AsRef<Path>,
) -> Result<bool, DesktopError> {
    let manifest_path = manifest_path.as_ref();
    if !manifest_path.exists() {
        return Ok(false);
    }
    // Serialized against `install_into` for the same reason.
    let _lock = FileLock::acquire(manifest_path)?;
    let manifest = load_manifest(manifest_path)?;
    // A stale manifest (Desktop updated underneath us) is exactly the case a user
    // reaches for `restore` to fix, so clear it rather than erroring out.
    if validate_manifest_target(&manifest, paths).is_err() {
        recover_stale_manifest(&manifest, manifest_path, paths.platform)?;
        return Ok(true);
    }
    // Path-safety first: this validates containment without requiring the files
    // to still be present, so a partially-cleaned install remains recoverable.
    validate_backup_location(&manifest, manifest_path)?;
    let running = active_desktop_pids(paths, Some(&manifest));
    if !running.is_empty() {
        return Err(DesktopError::DesktopBusy(running));
    }

    let external_launcher = uses_external_launcher(&manifest, paths.platform);

    // Step 1 — put the user's own Desktop runtime back. This is the part that,
    // if left half-done, breaks the official app, so it happens first and is
    // idempotent.
    if external_launcher {
        if desktop_override_is_ours(&manifest, paths.platform)? {
            restore_desktop_override(&manifest, paths.platform)?;
        }
    } else {
        match entrypoint_state(&manifest, &manifest.entrypoint) {
            EntrypointState::Managed => {
                copy_file_atomic(&manifest.original_backup, &manifest.entrypoint, true)?
            }
            // Already restored by a previous attempt.
            EntrypointState::Original | EntrypointState::Absent => {}
            EntrypointState::Foreign => {
                return Err(DesktopError::ManagedEntrypointChanged(
                    manifest.entrypoint.clone(),
                ));
            }
        }
    }

    // Step 2 — remove our own runtime and launcher artifacts. All of these are
    // inside our state directory (validated below), and every removal tolerates
    // an already-deleted file.
    remove_owned_runtime(&manifest, manifest_path)?;
    if let Some(config_path) = &manifest.launcher_config {
        remove_if_present(config_path)?;
    }
    if external_launcher && let Some(launcher) = &manifest.launcher_path {
        remove_if_present(launcher)?;
    }

    // Step 3 — destruction last, so an interruption before this point is still
    // recoverable (the backup is what lets us put the entrypoint back).
    match fs::remove_file(&manifest.original_backup) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::remove_file(manifest_path)?;
    Ok(true)
}

pub fn restore_if_present(manifest_path: impl AsRef<Path>) -> Result<bool, DesktopError> {
    let manifest_path = manifest_path.as_ref();
    if !manifest_path.exists() {
        return Ok(false);
    }
    match DesktopPaths::discover() {
        Ok(paths) => restore(&paths, manifest_path),
        // The Desktop install is gone (the app was uninstalled, or a new release
        // moved the entrypoint) but our manifest remains. Re-discovering used to
        // fail hard, which aborted `codex-mp uninstall` *before* the Codex config
        // was restored and left the user's config hijacked with no CLI path back.
        // The manifest records everything needed to undo our own changes, so fall
        // back to tearing down from the record alone.
        Err(_) => restore_from_manifest_only(manifest_path),
    }
}

/// Tear down an adapter install using only what the manifest records.
///
/// Used when the Desktop install can no longer be discovered. Only our own state
/// is touched: the override is cleared when it points at our launcher, and the
/// runtime/launcher/backup files named by the manifest are removed. The live
/// Desktop entrypoint is *not* rewritten, because without a discoverable install
/// there is no verified original to put back — and the manifest-validated paths
/// keep the deletions inside our own state directory.
pub fn restore_from_manifest_only(manifest_path: impl AsRef<Path>) -> Result<bool, DesktopError> {
    let manifest_path = manifest_path.as_ref();
    if !manifest_path.exists() {
        return Ok(false);
    }
    let _lock = FileLock::acquire(manifest_path)?;
    let manifest = load_manifest(manifest_path)?;
    let platform = manifest.platform;

    if uses_external_launcher(&manifest, platform)
        && desktop_override_is_ours(&manifest, platform).unwrap_or(false)
    {
        restore_desktop_override(&manifest, platform)?;
    }
    remove_owned_runtime(&manifest, manifest_path)?;
    if let Some(config_path) = &manifest.launcher_config {
        remove_if_present(config_path)?;
    }
    if let Some(launcher) = &manifest.launcher_path {
        remove_if_present(launcher)?;
    }
    match fs::remove_file(&manifest.original_backup) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::remove_file(manifest_path)?;
    Ok(true)
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

/// Marker that identifies a launcher written by this project.
const LAUNCHER_MARKER: &str = "CODEX_MP_ROUTER_ENDPOINT_FILE";

/// Name the displaced original is moved to next to the entrypoint.
const ORPHAN_BACKUP_SUFFIX: &str = ".orig";

/// If `entrypoint` is one of our launchers, return a displaced original to restore.
///
/// This is the recovery source for an install whose manifest is gone. Two
/// layouts are checked, newest first:
///
/// 1. `<manifest dir>/desktop-backups/<version>/<entrypoint name>` — where
///    `install` puts the displaced original in current releases;
/// 2. `<entrypoint>.orig` — the sibling layout used by earlier releases.
///
/// Both are validated by requiring the candidate to be a native entrypoint:
/// restoring another script would only swap one non-native file for another.
fn detect_orphaned_launcher(
    entrypoint: &Path,
    manifest_path: &Path,
    version: &str,
) -> Option<PathBuf> {
    let contents = fs::read_to_string(entrypoint).ok()?;
    if !contents.contains(LAUNCHER_MARKER) {
        return None;
    }
    let name = entrypoint.file_name()?;

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(backup_root) = manifest_path.parent().map(|p| p.join("desktop-backups")) {
        // `install` records the displaced original under the Desktop release
        // version, which the caller already parsed (`DesktopPaths::version`).
        // Deriving it here by counting `.parent()` calls is easy to get wrong —
        // it is the release directory, not the `releases` directory above it.
        candidates.push(backup_root.join(version).join(name));
    }
    let mut sibling = entrypoint.as_os_str().to_os_string();
    sibling.push(ORPHAN_BACKUP_SUFFIX);
    candidates.push(PathBuf::from(sibling));

    candidates
        .into_iter()
        .find(|candidate| candidate.is_file() && is_native_entrypoint(candidate))
}

/// Heal an entrypoint replaced by this project whose manifest is gone.
///
/// Returns `true` when a launcher was found and the original entrypoint was
/// restored. `install` uses the same detection; exposing it lets
/// `codex-mp desktop restore` report and fix the orphaned state instead of
/// answering "no manifest was found" while the entrypoint stays hijacked.
pub fn restore_orphaned_launcher(
    paths: &DesktopPaths,
    manifest_path: impl AsRef<Path>,
) -> Result<bool, DesktopError> {
    let Some(backup) =
        detect_orphaned_launcher(&paths.entrypoint, manifest_path.as_ref(), &paths.version)
    else {
        return Ok(false);
    };
    fs::remove_file(&paths.entrypoint)?;
    copy_file_atomic(&backup, &paths.entrypoint, true)?;
    Ok(true)
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

/// Clear out an adapter install whose recorded Desktop release no longer exists.
///
/// `DesktopPaths::discover()` resolves `packages/standalone/current/bin/codex` to
/// whichever release is current, so **every ChatGPT Desktop update** changes
/// `release_dir`/`entrypoint` and invalidates the manifest. `validate_manifest_target`
/// is called by `status_for`, `install` and `restore`, so all of them then failed
/// with a bare "manifest targets X but current Desktop is Y" and nothing in the
/// CLI or web UI could clear it. The user could not uninstall the old adapter nor
/// install a new one without hand-deleting `desktop-integration.json`, and on
/// Linux the old release kept our launcher with an orphaned backup.
///
/// This removes exactly the artifacts the manifest names — all inside our own
/// state directory — restores the Desktop override if it still points at our
/// launcher, and drops the manifest. It never touches the *new* release.
fn recover_stale_manifest(
    manifest: &DesktopManifest,
    manifest_path: &Path,
    platform: DesktopPlatform,
) -> Result<(), DesktopError> {
    // Undo the override first: it is machine-global state that would otherwise
    // keep redirecting Desktop at a launcher we are about to delete.
    if uses_external_launcher(manifest, platform)
        && desktop_override_is_ours(manifest, platform).unwrap_or(false)
    {
        let _ = restore_desktop_override(manifest, platform);
    }
    // Only our own state directory is touched. The validator rejects a manifest
    // whose recorded paths escape it, which matters because the manifest is
    // user-writable input.
    validate_runtime_location(manifest, manifest_path)?;
    let _ = remove_owned_runtime(manifest, manifest_path);
    if let Some(config_path) = &manifest.launcher_config {
        let _ = remove_if_present(config_path);
    }
    if let Some(launcher) = &manifest.launcher_path {
        let _ = remove_if_present(launcher);
    }
    // The backup now belongs to a release that is gone; keep it only if it is
    // still inside our state directory and the caller asked for recovery.
    if !manifest.original_backup.is_symlink() && manifest.original_backup.is_file() {
        let _ = fs::remove_file(&manifest.original_backup);
    }
    fs::remove_file(manifest_path)?;
    Ok(())
}

/// Bring a stale manifest to a clean state, if one is present.
///
/// Returns `Ok(true)` when a stale manifest was recovered. Any error is reported
/// so the caller can surface it rather than silently proceeding on a half-cleaned
/// installation.
fn recover_stale_manifest_if_present(
    paths: &DesktopPaths,
    manifest_path: &Path,
) -> Result<bool, DesktopError> {
    if !manifest_path.exists() {
        return Ok(false);
    }
    let manifest = load_manifest(manifest_path)?;
    if validate_manifest_target(&manifest, paths).is_ok() {
        return Ok(false);
    }
    recover_stale_manifest(&manifest, manifest_path, paths.platform)?;
    Ok(true)
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

// `save_launcher_config` used to live here. Install now serialises the launcher
// config once, hashes those exact bytes for the manifest, and writes them in
// phase 2 — hashing the file after writing it would make the recorded hash
// unreliable across a crash.

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
        // Was `let _ =`, which silently discarded the very error this function
        // was changed to report. Without a loaded LaunchAgent the override does
        // not survive a logout, so the user is told the install failed.
        persist_macos_environment_override(Some(path))?;
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
        persist_macos_environment_override(None)?;
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

/// Escape a value for inclusion in plist XML.
///
/// A path is user-controlled (it comes from the manifest / registry directory)
/// and may legally contain `&`, `<` or `>`. Interpolating it raw produced
/// malformed XML, so `launchctl` silently refused to load the file.
#[cfg(target_os = "macos")]
fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
/// Persist (or clear) the macOS `CODEX_CLI_PATH` override as a LaunchAgent.
///
/// The plist is *bootstrapped* after writing. Previously it was only written —
/// never loaded — so nothing re-applied the variable after a logout even though
/// `verify_desktop_override` reported the override as healthy, and the write
/// error was discarded with `let _ =`.
fn persist_macos_environment_override(path: Option<&Path>) -> Result<(), DesktopError> {
    let Some(base_dirs) = BaseDirs::new() else {
        return Ok(());
    };
    let launch_agents = base_dirs.home_dir().join("Library/LaunchAgents");
    let plist_path = launch_agents.join("dev.codex-multiprovider.env.plist");
    let label = "dev.codex-multiprovider.env";

    // Always unload first so a reload picks up the new contents. `bootout`
    // fails harmlessly when nothing is loaded.
    let _ = std::process::Command::new("launchctl")
        .args(["bootout", &format!("gui/{}", unsafe { libc::getuid() })])
        .arg(&plist_path)
        .status();

    match path {
        Some(target) => {
            fs::create_dir_all(&launch_agents)?;
            let plist_content = format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{label}</string>
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
                xml_escape(&target.display().to_string())
            );
            codex_mp_core::write_private_atomic(&plist_path, plist_content.as_bytes())?;
            // Load it so the override survives a logout. A failure here is
            // reported rather than swallowed: without it the "persistence" is a
            // file nothing reads.
            let status = std::process::Command::new("launchctl")
                .args(["bootstrap", &format!("gui/{}", unsafe { libc::getuid() })])
                .arg(&plist_path)
                .status()?;
            if !status.success() {
                return Err(DesktopError::RuntimeInvalid(format!(
                    "wrote the macOS environment override but could not load it ({})",
                    plist_path.display()
                )));
            }
        }
        None => {
            if plist_path.exists() {
                fs::remove_file(&plist_path)?;
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
    // Delegates to core so the file is created with mode 0600 and never passes
    // through an umask-readable state. This one holds the launcher config, which
    // names the manager and app-server binaries the Desktop will execute.
    let mut bytes = bytes.to_vec();
    bytes.push(b'\n');
    codex_mp_core::write_private_atomic(path, &bytes)?;
    Ok(())
}

fn write_executable_atomic(path: &Path, bytes: &[u8]) -> Result<(), DesktopError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    reject_symlink(path)?;
    let temp = temp_path(path);
    // Created 0600 and only widened to 0755 once the content is complete. The
    // launcher holds no secret, but writing it via `fs::write` first left it
    // umask-readable (0664) and, worse, briefly *executable-by-mode* only after
    // the chmod — a partially written script is never runnable this way.
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let write_result = (|| -> Result<(), io::Error> {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        Ok(())
    })();
    if let Err(error) = write_result {
        let _ = fs::remove_file(&temp);
        return Err(error.into());
    }
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

fn sha256_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let mut digest = String::with_capacity(64);
    for byte in hasher.finalize() {
        FmtWrite::write_fmt(&mut digest, format_args!("{byte:02x}"))
            .expect("writing a SHA-256 digest to a String cannot fail");
    }
    digest
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

    /// Regression: an install whose manifest is gone left the Desktop hijacked
    /// **with no CLI path back**. `restore` answered "no manifest was found"
    /// (a no-op) and `install` refused with `EntrypointNotNative`, because our
    /// launcher is a script while the check requires an ELF/MZ header. The
    /// launcher is recognisable by its marker and the displaced binary sits next
    /// to it, so the orphan must be healed.
    #[test]
    fn an_orphaned_launcher_is_restored_from_its_sibling_backup() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".codex");
        let entrypoint = root.join("packages/standalone/releases/0.154.0-desktop/bin/codex");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        // `from_entrypoint` requires the entrypoint to exist.
        elf_stub(&entrypoint);
        let paths = DesktopPaths::from_entrypoint(&root, &entrypoint).unwrap();

        // The displaced original, next to the entrypoint.
        let mut backup_name = entrypoint.as_os_str().to_os_string();
        backup_name.push(ORPHAN_BACKUP_SUFFIX);
        elf_stub(&PathBuf::from(&backup_name));

        // Our launcher has replaced the entrypoint (a script with the marker).
        fs::write(
            &entrypoint,
            format!("#!/bin/sh\n{LAUNCHER_MARKER}=\"$HOME/.config/x\"\nexec real \"$@\"\n"),
        )
        .unwrap();
        assert!(
            !is_native_entrypoint(&entrypoint),
            "precondition: launcher is a script"
        );

        let manifest_path = directory.path().join(DESKTOP_MANIFEST_FILE);
        assert!(
            restore_orphaned_launcher(&paths, &manifest_path).unwrap(),
            "the orphaned launcher must be detected and healed"
        );
        assert!(
            is_native_entrypoint(&entrypoint),
            "the entrypoint must be the native binary again"
        );
    }

    /// A genuine stock entrypoint must not be mistaken for an orphan.
    #[test]
    fn a_native_entrypoint_is_not_treated_as_an_orphan() {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".codex");
        let entrypoint = root.join("packages/standalone/releases/0.154.0-desktop/bin/codex");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        elf_stub(&entrypoint);
        let paths = DesktopPaths::from_entrypoint(&root, &entrypoint).unwrap();

        let manifest_path = directory.path().join(DESKTOP_MANIFEST_FILE);
        assert!(
            !restore_orphaned_launcher(&paths, &manifest_path).unwrap(),
            "a stock entrypoint must be left alone"
        );
        assert!(is_native_entrypoint(&entrypoint));
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

    // ---- install/restore lifecycle ------------------------------------------

    struct Fixture {
        _directory: tempfile::TempDir,
        manifest_path: PathBuf,
        paths: DesktopPaths,
        options: DesktopInstallOptions,
    }

    /// A complete, installable Desktop layout. `version` selects the release
    /// directory, which is what a Desktop update changes.
    fn fixture(version: &str) -> Fixture {
        let directory = tempdir().unwrap();
        let root = directory.path().join(".codex");
        let release = root.join("packages/standalone/releases").join(version);
        let entrypoint = release.join("bin/codex");
        let code_mode_host = release.join("bin/codex-code-mode-host");
        fs::create_dir_all(entrypoint.parent().unwrap()).unwrap();
        elf_stub(&entrypoint);
        elf_stub(&code_mode_host);

        let paths = DesktopPaths::from_entrypoint(&root, &entrypoint).unwrap();
        // The registry lives beside the manifest, as in production.
        let manifest_path = directory.path().join(DESKTOP_MANIFEST_FILE);
        let app_server_binary = directory.path().join("codex-mp-app-server-bin");
        elf_stub(&app_server_binary);
        let metadata = directory.path().join("codex-mp-build.json");
        let app_server_hash = sha256_file(&app_server_binary).unwrap();
        fs::write(
            &metadata,
            serde_json::json!({
                "schema_version": 1,
                "upstream_commit": PINNED_UPSTREAM_COMMIT,
                "app_server_binary": "codex-mp-app-server-bin",
                "patch_sha256": "deadbeef",
                "official_codex_binary_untouched": true
            })
            .to_string(),
        )
        .unwrap();
        // `validate_build_metadata` checks the recorded mismatch itself; keep the
        // fixture honest by writing the real hash.
        let raw = fs::read_to_string(&metadata).unwrap().replace(
            "\"patch_sha256\": \"deadbeef\"",
            &format!("\"patch_sha256\": \"{app_server_hash}\""),
        );
        fs::write(&metadata, raw).unwrap();

        let options = DesktopInstallOptions {
            manifest_path: manifest_path.clone(),
            app_server_binary,
            build_metadata: metadata,
            codex_mp_binary: directory.path().join("codex-mp"),
            endpoint_file: directory.path().join("router-endpoint.json"),
        };
        elf_stub(&options.codex_mp_binary);

        Fixture {
            _directory: directory,
            manifest_path,
            paths,
            options,
        }
    }

    /// On Linux the adapter replaces the real `codex` entrypoint, so this is the
    /// path that must round-trip.
    #[cfg(target_os = "linux")]
    #[test]
    fn linux_install_then_restore_round_trips_the_entrypoint() {
        let fixture = fixture("0.154.0-desktop");
        let original = fs::read(&fixture.paths.entrypoint).unwrap();

        let result = install_into(&fixture.paths, &fixture.options).unwrap();
        assert!(result.changed_entrypoint);
        assert!(
            fs::read(&fixture.paths.entrypoint).unwrap() != original,
            "install did not replace the entrypoint"
        );
        assert!(fixture.manifest_path.exists());
        assert!(
            !result.manifest.pending,
            "install must confirm the manifest"
        );

        assert!(restore(&fixture.paths, &fixture.manifest_path).unwrap());
        assert_eq!(
            fs::read(&fixture.paths.entrypoint).unwrap(),
            original,
            "restore did not put the original entrypoint back"
        );
        assert!(!fixture.manifest_path.exists());
    }

    /// Regression: the end-to-end shape of N-74. Install, then lose the manifest
    /// (a crash before it was written, or a user cleaning the config directory).
    /// `restore` reported "no manifest" and did nothing, while `install` refused
    /// with `EntrypointNotNative` — so the Desktop stayed hijacked with no CLI way
    /// back. `restore_orphaned_launcher` must heal it from the sibling backup.
    #[test]
    fn linux_install_loses_manifest_then_recovers_the_entrypoint() {
        let fixture = fixture("0.154.0-desktop");
        let original = fs::read(&fixture.paths.entrypoint).unwrap();

        install_into(&fixture.paths, &fixture.options).unwrap();
        assert_ne!(
            fs::read(&fixture.paths.entrypoint).unwrap(),
            original,
            "precondition: install replaced the entrypoint"
        );

        // The manifest disappears.
        fs::remove_file(&fixture.manifest_path).unwrap();

        // Both entry points the user would try must be able to recover.
        assert!(
            restore_orphaned_launcher(&fixture.paths, &fixture.manifest_path).unwrap(),
            "the orphaned launcher must be detected"
        );
        assert_eq!(
            fs::read(&fixture.paths.entrypoint).unwrap(),
            original,
            "the original entrypoint must be back after recovering the orphan"
        );
        // Idempotent: nothing left to heal.
        assert!(!restore_orphaned_launcher(&fixture.paths, &fixture.manifest_path).unwrap());
    }

    /// After an install loses its manifest, `install` must be able to run again
    /// (the user's other escape route) rather than refusing `EntrypointNotNative`.
    #[test]
    fn install_recovers_from_a_lost_manifest() {
        let fixture = fixture("0.154.0-desktop");
        install_into(&fixture.paths, &fixture.options).unwrap();
        fs::remove_file(&fixture.manifest_path).unwrap();

        // Re-installing heals the orphan first, then installs cleanly.
        let result = install_into(&fixture.paths, &fixture.options);
        assert!(
            result.is_ok(),
            "install must not refuse an orphaned launcher it wrote itself: {:?}",
            result.err()
        );
    }

    /// Regression: restore verified the *pre*-restore state up front and deleted
    /// the manifest last, so a crash after it had restored the entrypoint made
    /// every retry fail with `ManagedEntrypointChanged`. Desktop was then stuck
    /// in `Drifted` for ever.
    #[cfg(target_os = "linux")]
    #[test]
    fn restore_is_re_runnable_after_a_partial_restore() {
        let fixture = fixture("0.154.0-desktop");
        let original = fs::read(&fixture.paths.entrypoint).unwrap();
        let result = install_into(&fixture.paths, &fixture.options).unwrap();

        // Simulate a crash after the entrypoint was restored but before cleanup:
        // the entrypoint is the original again, the manifest and backup remain.
        fs::write(&fixture.paths.entrypoint, &original).unwrap();
        fs::set_permissions(
            &fixture.paths.entrypoint,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        assert!(fixture.manifest_path.exists());
        assert!(result.manifest.original_backup.exists());

        // The retry must finish, not dead-end.
        restore(&fixture.paths, &fixture.manifest_path)
            .expect("a partially restored adapter must still be restorable");
        assert_eq!(fs::read(&fixture.paths.entrypoint).unwrap(), original);
        assert!(!fixture.manifest_path.exists());
        assert!(!result.manifest.original_backup.exists());
    }

    /// Regression: a manifest naming a Desktop release that no longer exists
    /// blocked `install`, `restore` and `status` with no way out, because a
    /// Desktop update changes `release_dir`/`entrypoint`.
    #[cfg(target_os = "linux")]
    #[test]
    fn stale_manifest_after_a_desktop_update_is_recoverable() {
        let fixture = fixture("0.154.0-desktop");
        install_into(&fixture.paths, &fixture.options).unwrap();
        assert!(fixture.manifest_path.exists());

        // The Desktop app updates: a new release directory becomes current.
        let old_manifest = load_manifest(&fixture.manifest_path).unwrap();
        let new_release = fixture
            .paths
            .codex_home
            .join("packages/standalone/releases/0.155.0-desktop");
        fs::create_dir_all(new_release.join("bin")).unwrap();
        let new_entrypoint = new_release.join("bin/codex");
        elf_stub(&new_entrypoint);
        elf_stub(&new_release.join("bin/codex-code-mode-host"));
        let new_paths =
            DesktopPaths::from_entrypoint(&fixture.paths.codex_home, &new_entrypoint).unwrap();
        assert_ne!(new_paths.entrypoint, old_manifest.entrypoint);

        // `install` must clear the stale record rather than refusing for ever.
        let mut options = fixture.options.clone();
        options.manifest_path = fixture.manifest_path.clone();
        let reinstalled =
            install_into(&new_paths, &options).expect("a Desktop update must not block reinstall");
        assert_eq!(reinstalled.manifest.entrypoint, new_paths.entrypoint);
        assert!(!reinstalled.manifest.pending);

        // And `restore` against the new paths also works.
        restore(&new_paths, &fixture.manifest_path).unwrap();
        assert!(!fixture.manifest_path.exists());
        assert_eq!(
            fs::read(&new_paths.entrypoint).unwrap(),
            fs::read(new_release.join("bin/codex")).unwrap()
        );
        // The old release is gone from disk in this fixture; its backup must not
        // have been left behind.
        assert!(!old_manifest.original_backup.exists());
    }

    /// Regression: `load_manifest` rejected any manifest whose `upstream_commit`
    /// did not match the currently pinned one. An adapter installed by an older
    /// release therefore could not be removed at all — `restore`,
    /// `restore_if_present` and `status` all failed at load, so the user could
    /// neither uninstall nor reinstall without hand-deleting the manifest.
    #[cfg(target_os = "linux")]
    #[test]
    fn an_older_release_adapter_can_still_be_removed() {
        let fixture = fixture("0.154.0-desktop");
        let result = install_into(&fixture.paths, &fixture.options).unwrap();

        // Rewrite the manifest as an older release would have left it.
        let mut stale = load_manifest(&fixture.manifest_path).unwrap();
        stale.upstream_commit = "an-older-pinned-commit".into();
        save_manifest(&fixture.manifest_path, &stale).unwrap();

        // Structural load must succeed so teardown is possible.
        let loaded = load_manifest(&fixture.manifest_path)
            .expect("an old-release manifest must still be readable for teardown");

        // `status` must not be a dead end either.
        let status = status_for(&fixture.paths, &fixture.manifest_path).unwrap();
        assert!(matches!(
            status.state,
            DesktopIntegrationState::Managed | DesktopIntegrationState::Drifted
        ));

        // And teardown must actually complete.
        assert!(
            restore(&fixture.paths, &fixture.manifest_path).unwrap(),
            "teardown of an old-release adapter must succeed"
        );
        assert!(!fixture.manifest_path.exists());
        assert!(!loaded.original_backup.exists());
        assert!(!result.manifest.original_backup.exists());
    }

    /// Regression: `restore_if_present` re-discovered the Desktop install, which
    /// fails once the app is uninstalled or moves. That error aborted
    /// `codex-mp uninstall` *before* the Codex config was restored, leaving the
    /// user's config hijacked with no CLI path back. Tearing down from the
    /// manifest record must still work.
    #[cfg(target_os = "linux")]
    #[test]
    fn restore_from_manifest_only_succeeds_when_desktop_discovery_fails() {
        let fixture = fixture("0.154.0-desktop");
        let result = install_into(&fixture.paths, &fixture.options).unwrap();
        assert!(fixture.manifest_path.exists());

        // Simulate the Desktop app being uninstalled: the entrypoint is gone.
        std::fs::remove_file(&fixture.paths.entrypoint).unwrap();

        let restored = restore_from_manifest_only(&fixture.manifest_path)
            .expect("teardown must not require a discoverable Desktop install");
        assert!(restored);
        assert!(!fixture.manifest_path.exists(), "manifest must be removed");
        assert!(
            !result.manifest.original_backup.exists(),
            "backup must be removed"
        );
        assert!(
            !codex_mp_core::lock_file_path(&fixture.manifest_path).exists(),
            "lock must be released"
        );
    }

    /// Regression: the macOS LaunchAgent plist interpolated the launcher path
    /// into XML without escaping. A path containing `&`, `<` or `>` (legal in
    /// macOS home directories) produced malformed XML, so `launchctl` refused to
    /// load the file and the "persistent" override silently vanished after a
    /// logout — while `verify_desktop_override` still reported it as healthy.
    #[cfg(target_os = "macos")]
    #[test]
    fn plist_paths_are_xml_escaped() {
        assert_eq!(xml_escape("/Users/a&b/codex"), "/Users/a&amp;b/codex");
        assert_eq!(xml_escape("/x/<y>/z"), "/x/&lt;y&gt;/z");
        assert_eq!(xml_escape("a\"b'c"), "a&quot;b&apos;c");
        // A plain path must pass through unchanged.
        assert_eq!(
            xml_escape("/Users/lvziw/.local/bin/codex"),
            "/Users/lvziw/.local/bin/codex"
        );
        // The escaped form must be well-formed when embedded in a plist string.
        let escaped = xml_escape("/Users/a&b/codex");
        assert!(!escaped.contains('&') || escaped.contains("&amp;"));
    }

    /// The manifest must record the platform so teardown does not have to guess.
    #[test]
    fn manifest_records_the_install_platform() {
        let fixture = fixture("0.154.0-desktop");
        let result = install_into(&fixture.paths, &fixture.options).unwrap();
        assert_eq!(result.manifest.platform, fixture.paths.platform);
        let reloaded = load_manifest(&fixture.manifest_path).unwrap();
        assert_eq!(reloaded.platform, fixture.paths.platform);
    }

    /// Two concurrent installs must not interleave: the loser must fail on a
    /// coherent state (or succeed cleanly), never leave a manifest that disagrees
    /// with the launcher actually installed.
    #[cfg(target_os = "linux")]
    #[test]
    fn concurrent_installs_leave_a_coherent_manifest() {
        let fixture = fixture("0.154.0-desktop");
        let manifest_path = fixture.manifest_path.clone();
        let paths = fixture.paths.clone();
        let options = fixture.options.clone();
        let other_paths = paths.clone();
        let other_options = options.clone();

        let first = std::thread::spawn(move || install_into(&paths, &options).map(|_| ()));
        let second =
            std::thread::spawn(move || install_into(&other_paths, &other_options).map(|_| ()));
        let results = [first.join().unwrap(), second.join().unwrap()];
        assert!(
            results.iter().any(Result::is_ok),
            "at least one install must succeed: {results:?}"
        );

        // Whatever happened, the surviving manifest must match the launcher on
        // disk, and the entrypoint must be either the launcher or the original.
        if manifest_path.exists() {
            let manifest = load_manifest(&manifest_path).unwrap();
            let current = sha256_file(&fixture.paths.entrypoint).unwrap();
            assert!(
                current == manifest.launcher_sha256 || current == manifest.original_sha256,
                "entrypoint matches neither the managed launcher nor the original"
            );
        }
        // No lock may be left behind.
        assert!(
            !codex_mp_core::lock_file_path(&manifest_path).exists(),
            "a lock file was left behind"
        );
    }

    /// An interrupted install must be recoverable by `restore`, and a re-run of
    /// `install` must refuse rather than build on a half-applied state.
    #[cfg(target_os = "linux")]
    #[test]
    fn pending_manifest_from_an_interrupted_install_is_recoverable() {
        let fixture = fixture("0.154.0-desktop");
        let original = fs::read(&fixture.paths.entrypoint).unwrap();
        install_into(&fixture.paths, &fixture.options).unwrap();

        // Simulate a crash after the entrypoint was replaced but before the
        // manifest was confirmed.
        let mut manifest = load_manifest(&fixture.manifest_path).unwrap();
        manifest.pending = true;
        save_manifest(&fixture.manifest_path, &manifest).unwrap();

        // Re-running install must not silently continue on top of this.
        assert!(matches!(
            install_into(&fixture.paths, &fixture.options),
            Err(DesktopError::Manifest(_))
        ));

        // `restore` is the documented way out and must put the entrypoint back.
        restore(&fixture.paths, &fixture.manifest_path).unwrap();
        assert_eq!(fs::read(&fixture.paths.entrypoint).unwrap(), original);
        assert!(!fixture.manifest_path.exists());

        // And now a fresh install works again.
        install_into(&fixture.paths, &fixture.options).expect("install must work after recovery");
    }
}
