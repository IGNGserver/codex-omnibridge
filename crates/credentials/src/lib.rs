//! Credential storage isolated from the provider registry.
//!
//! The native implementation delegates to the operating system keyring. The
//! registry only stores the opaque `credential_reference` and never receives a
//! secret. Tests can use `MemoryCredentialStore` without touching the host
//! keyring.

use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use keyring::Entry;
use secrecy::{ExposeSecret, SecretString};
use thiserror::Error;

const DEFAULT_SERVICE: &str = "dev.codex-multiprovider";

fn set_private_file_permissions(path: &std::path::Path) -> Result<(), std::io::Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(windows)]
    {
        let system_dir = std::env::var_os("SystemRoot")
            .map(std::path::PathBuf::from)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "SystemRoot is not set; cannot secure the credential fallback",
                )
            })?
            .join("System32");
        let principal = std::process::Command::new(system_dir.join("whoami.exe"))
            .output()?
            .stdout;
        let principal = String::from_utf8_lossy(&principal).trim().to_owned();
        if principal.is_empty() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "whoami.exe returned no Windows principal",
            ));
        }
        let output = std::process::Command::new(system_dir.join("icacls.exe"))
            .arg(path)
            .args(["/inheritance:r", "/grant:r"])
            .arg(format!("{principal}:F"))
            .output()?;
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    let _ = path;
    Ok(())
}

/// References already reported as served by an environment override.
///
/// Compares against nothing but its own contents, so it is only used to keep the
/// one-line notice from repeating for the same reference.
static OVERRIDE_WARNED: std::sync::Mutex<Option<std::collections::HashSet<String>>> =
    std::sync::Mutex::new(None);

fn file_backend_path() -> PathBuf {
    // Shares `codex_mp_core`'s application identity so the credential file always
    // sits beside the registry it belongs to. Using a second `ProjectDirs` triple
    // here resolved to the same directory on Linux by coincidence, but to a
    // *different* one on macOS/Windows.
    codex_mp_core::app_config_dir()
        .map(|dir| dir.join(".credentials"))
        .unwrap_or_else(|| PathBuf::from(".credentials"))
}

fn write_private_json_atomic(path: &Path, contents: &str) -> Result<(), std::io::Error> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    for attempt in 0..16u32 {
        let temporary = parent.join(format!(
            ".{file_name}.{}.{}.{}.tmp",
            std::process::id(),
            timestamp,
            attempt
        ));
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&temporary) {
            Ok(mut file) => {
                if let Err(error) = file
                    .write_all(contents.as_bytes())
                    .and_then(|_| file.sync_all())
                {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
                drop(file);
                if let Err(error) = fs::rename(&temporary, path) {
                    let _ = fs::remove_file(&temporary);
                    return Err(error);
                }
                set_private_file_permissions(path)?;
                #[cfg(unix)]
                fs::File::open(parent)?.sync_all()?;
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }

    Err(std::io::Error::new(
        std::io::ErrorKind::AlreadyExists,
        "could not allocate a private credentials temporary file",
    ))
}

#[derive(Debug, Error)]
pub enum CredentialStoreError {
    #[error("credential backend error: {0}")]
    Backend(String),
    #[error("credential `{0}` was not found")]
    NotFound(String),
    #[error("credential reference must not be empty")]
    EmptyReference,
}

pub trait CredentialStore: Send + Sync {
    fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError>;
    fn set(&self, reference: &str, value: &SecretString) -> Result<(), CredentialStoreError>;
    fn delete(&self, reference: &str) -> Result<(), CredentialStoreError>;
}

#[derive(Debug, Clone)]
pub struct NativeCredentialStore {
    service: String,
}

impl Default for NativeCredentialStore {
    fn default() -> Self {
        Self::new(DEFAULT_SERVICE)
    }
}

impl NativeCredentialStore {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    /// Whether the opt-in 0600 file backend is selected.
    ///
    /// Enabled only by `--secret-backend file`, which sets this variable. The
    /// default store never falls back to a plaintext credential file.
    fn file_backend_enabled(&self) -> bool {
        std::env::var("CODEX_MP_SECRET_BACKEND").as_deref() == Ok("file")
    }

    /// Persist a secret in the 0600 file store.
    ///
    /// The document is read-modify-written under the cross-process file lock so
    /// two concurrent writers cannot drop each other's entries, and committed
    /// through a private synced temporary file.
    fn set_in_file(
        &self,
        reference: &str,
        value: &SecretString,
    ) -> Result<(), CredentialStoreError> {
        set_in_file_at(&file_backend_path(), reference, value)
    }
}

/// Persist a secret in a 0600 credential file, read-modify-written under the
/// cross-process lock so concurrent writers cannot drop each other's entries.
fn set_in_file_at(
    path: &std::path::Path,
    reference: &str,
    value: &SecretString,
) -> Result<(), CredentialStoreError> {
    let _guard = codex_mp_core::FileLock::acquire(path)
        .map_err(|error| CredentialStoreError::Backend(error.to_string()))?;
    let mut map: HashMap<String, String> = fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default();
    map.insert(reference.to_owned(), value.expose_secret().to_owned());
    let json_str = serde_json::to_string(&map)
        .map_err(|error| CredentialStoreError::Backend(error.to_string()))?;
    write_private_json_atomic(path, &json_str)
        .map_err(|error| CredentialStoreError::Backend(error.to_string()))
}

impl NativeCredentialStore {
    fn entry(&self, reference: &str) -> Result<Entry, CredentialStoreError> {
        if reference.trim().is_empty() {
            return Err(CredentialStoreError::EmptyReference);
        }
        Entry::new(&self.service, reference)
            .map_err(|error| CredentialStoreError::Backend(error.to_string()))
    }
}

impl CredentialStore for NativeCredentialStore {
    fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
        let env_var = format!(
            "CODEX_MP_KEY_{}",
            reference
                .chars()
                .map(|c| if c.is_ascii_alphanumeric() {
                    c.to_ascii_uppercase()
                } else {
                    '_'
                })
                .collect::<String>()
        );
        // An explicit environment override takes precedence over every stored
        // backend. That is a deliberate operator escape hatch (12-factor style),
        // but it used to be entirely silent: a stray variable from a shell
        // profile, container or CI job would replace a provider's upstream
        // credential with nothing in the UI or logs to say so.
        //
        // Warn once per reference so the behaviour is discoverable without
        // spamming (the Router resolves each reference once per generation).
        if let Ok(val) = std::env::var(&env_var)
            && !val.trim().is_empty()
        {
            let first_time = OVERRIDE_WARNED
                .lock()
                .map(|mut warned| {
                    warned
                        .get_or_insert_with(std::collections::HashSet::new)
                        .insert(env_var.clone())
                })
                .unwrap_or(true);
            if first_time {
                eprintln!(
                    "codex-mp: using the `{env_var}` environment override for `{reference}`; \
                     this takes precedence over any stored credential"
                );
            }
            return Ok(SecretString::from(val));
        }

        if self.file_backend_enabled() {
            let fallback_file = file_backend_path();
            if let Ok(s) = std::fs::read_to_string(&fallback_file)
                && let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&s)
                && let Some(val) = map.get(reference)
            {
                return Ok(SecretString::from(val.clone()));
            }
        }

        let entry = self.entry(reference)?;
        match entry.get_password() {
            Ok(pwd) => Ok(SecretString::from(pwd)),
            Err(error) => {
                // If the key was stored in the file store (e.g. automatic fallback for large secrets),
                // check the file store before giving up.
                let fallback_file = file_backend_path();
                if let Ok(s) = std::fs::read_to_string(&fallback_file)
                    && let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&s)
                    && let Some(val) = map.get(reference)
                {
                    return Ok(SecretString::from(val.clone()));
                }
                if matches!(error, keyring::Error::NoEntry) {
                    Err(CredentialStoreError::NotFound(reference.to_owned()))
                } else {
                    Err(CredentialStoreError::Backend(error.to_string()))
                }
            }
        }
    }

    fn set(&self, reference: &str, value: &SecretString) -> Result<(), CredentialStoreError> {
        // `--secret-backend file` selects the 0600 file as the store, so it must
        // be written **first** and be authoritative.
        if self.file_backend_enabled() {
            return self.set_in_file(reference, value);
        }

        // On Windows (or when a secret exceeds Windows Credential Manager's limit),
        // large secret material (e.g. OAuth token bundles > 2560 UTF-16 chars)
        // cannot be stored in the native keyring. Automatically fall back to the 0600 file store.
        let secret_str = value.expose_secret();
        if secret_str.encode_utf16().count() >= 2560 {
            return self.set_in_file(reference, value);
        }

        let entry = self.entry(reference)?;
        match entry.set_password(secret_str) {
            Ok(()) => Ok(()),
            Err(e) => {
                let err_msg = e.to_string();
                if err_msg.contains("2560 chars")
                    || err_msg.contains("longer than the platform limit")
                {
                    return self.set_in_file(reference, value);
                }
                Err(CredentialStoreError::Backend(format!(
                    "keyring write failed; choose --secret-backend file explicitly to enable the 0600 file backend: {e}"
                )))
            }
        }
    }

    fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
        // A failure here used to be discarded and `Ok(())` returned, so a caller
        // that removed a provider believed the key was gone while it stayed live
        // in the keyring. `NoEntry` is the one benign case: already absent.
        let mut had_keyring_error = false;
        let mut keyring_err_msg = String::new();
        if let Err(error) = self.entry(reference)?.delete_credential()
            && !matches!(error, keyring::Error::NoEntry)
        {
            had_keyring_error = true;
            keyring_err_msg = error.to_string();
        }

        // Always clean up any entry in the file store (handles file backend or fallback writes)
        let fallback_file = file_backend_path();
        let mut removed_from_file = false;
        if fallback_file.exists()
            && let Ok(_guard) = codex_mp_core::FileLock::acquire(&fallback_file)
            && let Ok(s) = fs::read_to_string(&fallback_file)
            && let Ok(mut map) = serde_json::from_str::<HashMap<String, String>>(&s)
        {
            if map.remove(reference).is_some() {
                removed_from_file = true;
                if let Ok(json_str) = serde_json::to_string(&map) {
                    let _ = write_private_json_atomic(&fallback_file, &json_str);
                }
            }
        }

        if had_keyring_error && !removed_from_file && !self.file_backend_enabled() {
            return Err(CredentialStoreError::Backend(format!(
                "keyring delete failed for `{reference}`: {keyring_err_msg}"
            )));
        }

        Ok(())
    }
}

/// Run a synchronous credential-store operation without blocking the async
/// runtime that is driving the caller.
///
/// This is not a nicety. On Linux the `keyring` crate reaches the Secret Service
/// over D-Bus via `zbus`, and `zbus` bridges back to a *synchronous* API by
/// calling `tokio::runtime::Runtime::block_on` internally. Invoking a store
/// operation directly from inside a `#[tokio::main]` async context therefore
/// panics with "Cannot start a runtime from within a runtime" — which made
/// `codex-mp provider add` and `provider edit` abort before writing anything.
/// Moving the call to a blocking thread both fixes that panic and keeps a slow
/// or wedged keyring daemon from stalling the runtime's worker threads.
///
/// When called outside a runtime the closure simply runs inline.
pub async fn run_blocking<T, F>(operation: F) -> Result<T, CredentialStoreError>
where
    F: FnOnce() -> Result<T, CredentialStoreError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle
            .spawn_blocking(operation)
            .await
            .map_err(|error| CredentialStoreError::Backend(error.to_string()))?,
        Err(_) => operation(),
    }
}

/// Resolve a credential from an async context.
pub async fn get_blocking(
    store: Arc<dyn CredentialStore>,
    reference: String,
) -> Result<SecretString, CredentialStoreError> {
    run_blocking(move || store.get(&reference)).await
}

/// Store a credential from an async context.
pub async fn set_blocking(
    store: Arc<dyn CredentialStore>,
    reference: String,
    value: SecretString,
) -> Result<(), CredentialStoreError> {
    run_blocking(move || store.set(&reference, &value)).await
}

/// Delete a credential from an async context.
pub async fn delete_blocking(
    store: Arc<dyn CredentialStore>,
    reference: String,
) -> Result<(), CredentialStoreError> {
    run_blocking(move || store.delete(&reference)).await
}

#[derive(Debug, Clone, Default)]
pub struct MemoryCredentialStore {
    values: Arc<Mutex<HashMap<String, SecretString>>>,
}

impl CredentialStore for MemoryCredentialStore {
    fn get(&self, reference: &str) -> Result<SecretString, CredentialStoreError> {
        self.values
            .lock()
            .map_err(|_| CredentialStoreError::Backend("memory lock poisoned".into()))?
            .get(reference)
            .cloned()
            .ok_or_else(|| CredentialStoreError::NotFound(reference.to_owned()))
    }

    fn set(&self, reference: &str, value: &SecretString) -> Result<(), CredentialStoreError> {
        if reference.trim().is_empty() {
            return Err(CredentialStoreError::EmptyReference);
        }
        self.values
            .lock()
            .map_err(|_| CredentialStoreError::Backend("memory lock poisoned".into()))?
            .insert(reference.to_owned(), value.clone());
        Ok(())
    }

    fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
        self.values
            .lock()
            .map_err(|_| CredentialStoreError::Backend("memory lock poisoned".into()))?
            .remove(reference);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_store_round_trip_does_not_expose_secret_in_debug() {
        let store = MemoryCredentialStore::default();
        let secret = SecretString::from("super-secret");
        store.set("provider:test", &secret).unwrap();
        assert_eq!(
            store.get("provider:test").unwrap().expose_secret(),
            "super-secret"
        );
        assert!(!format!("{store:?}").contains("super-secret"));
        store.delete("provider:test").unwrap();
        assert!(matches!(
            store.get("provider:test"),
            Err(CredentialStoreError::NotFound(_))
        ));
    }

    /// Regression: `--secret-backend file` is documented as selecting *file*
    /// storage, but `set` wrote the keyring first and only fell back to the file
    /// when the keyring failed. On any machine with a working keyring the flag
    /// silently stored secrets in the keyring instead.
    ///
    /// Worse, `get` reads the file first: with both stores populated, a rotated
    /// key was written to the keyring while reads kept returning the file's stale
    /// value. Verified end-to-end before the fix — after
    /// `provider edit --api-key-stdin` the upstream still received the old key.
    ///
    /// Drives `set_in_file_at` (the function `set_in_file` delegates to) against a
    /// temporary path, so the test owns its file and never touches the operator's
    /// real config directory.
    #[test]
    fn the_file_backend_accumulates_entries_and_keeps_them_private() {
        let directory = std::env::temp_dir().join(format!(
            "codex-mp-credentials-filemode-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(".credentials");

        set_in_file_at(&path, "provider:a", &SecretString::from("first".to_owned())).unwrap();
        // A second write must not drop the first: the document is read-modify-written.
        set_in_file_at(
            &path,
            "provider:b",
            &SecretString::from("second".to_owned()),
        )
        .unwrap();
        // Replacing an existing entry must overwrite it, not duplicate it.
        set_in_file_at(
            &path,
            "provider:a",
            &SecretString::from("rotated".to_owned()),
        )
        .unwrap();

        let map: HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(
            map.len(),
            2,
            "entries must accumulate, not overwrite the map"
        );
        assert_eq!(
            map.get("provider:a").map(String::as_str),
            Some("rotated"),
            "an update must replace the previous value"
        );
        assert_eq!(map.get("provider:b").map(String::as_str), Some("second"));

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600,
                "the credential file must stay private"
            );
        }
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// The `CODEX_MP_KEY_<REFERENCE>` environment override is a deliberate
    /// operator escape hatch, but it was undocumented, untested, and completely
    /// silent — a stray variable from a shell profile, container or CI job would
    /// replace a provider's upstream credential with nothing anywhere to say so.
    ///
    /// This pins the documented behaviour: the override wins over stored values.
    #[test]
    fn the_environment_override_takes_precedence() {
        let reference = "provider:env-override-test";
        let var = "CODEX_MP_KEY_PROVIDER_ENV_OVERRIDE_TEST";
        // SAFETY: this test owns the variable.
        let previous = std::env::var_os(var);
        unsafe { std::env::set_var(var, "from-env") };

        let store = NativeCredentialStore::default();
        let resolved = store
            .get(reference)
            .map(|s| s.expose_secret().to_owned())
            .map_err(|e| e.to_string());

        match previous {
            Some(value) => unsafe { std::env::set_var(var, value) },
            None => unsafe { std::env::remove_var(var) },
        }

        assert_eq!(
            resolved.ok().as_deref(),
            Some("from-env"),
            "the environment override must win over any stored backend"
        );
    }

    /// A blank override must be ignored rather than stored as an empty secret.
    #[test]
    fn a_blank_environment_override_is_ignored() {
        let var = "CODEX_MP_KEY_PROVIDER_BLANK_TEST";
        let previous = std::env::var_os(var);
        unsafe { std::env::set_var(var, "   ") };

        let store = NativeCredentialStore::default();
        let resolved = store.get("provider:blank-test");

        match previous {
            Some(value) => unsafe { std::env::set_var(var, value) },
            None => unsafe { std::env::remove_var(var) },
        }

        assert!(
            resolved.is_err(),
            "a whitespace-only override must not be treated as a credential"
        );
    }

    #[test]
    fn private_json_writer_commits_a_private_file() {
        let directory = std::env::temp_dir().join(format!(
            "codex-mp-credentials-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join(".credentials");
        write_private_json_atomic(&path, r#"{"provider:test":"secret"}"#).unwrap();
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            r#"{"provider:test":"secret"}"#
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert!(!directory.read_dir().unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
        fs::remove_dir_all(directory).unwrap();
    }
}
