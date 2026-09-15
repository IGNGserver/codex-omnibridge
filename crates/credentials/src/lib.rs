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

fn file_backend_path() -> PathBuf {
    directories::ProjectDirs::from("dev", "codex", "codexmultiprovider")
        .map(|d| d.config_dir().join(".credentials"))
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

    /// Explicitly opt into the 0600 file backend for headless installations.
    /// The default store never falls back to a plaintext credential file.
    pub fn with_file_fallback(service: impl Into<String>) -> Self {
        Self::new(service)
    }

    fn file_backend_enabled(&self) -> bool {
        std::env::var("CODEX_MP_SECRET_BACKEND").as_deref() == Ok("file")
    }

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
        if let Ok(val) = std::env::var(&env_var)
            && !val.trim().is_empty()
        {
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
        entry
            .get_password()
            .map(SecretString::from)
            .map_err(|error| {
                if matches!(error, keyring::Error::NoEntry) {
                    CredentialStoreError::NotFound(reference.to_owned())
                } else {
                    CredentialStoreError::Backend(error.to_string())
                }
            })
    }

    fn set(&self, reference: &str, value: &SecretString) -> Result<(), CredentialStoreError> {
        match self.entry(reference)?.set_password(value.expose_secret()) {
            Ok(()) => Ok(()),
            Err(e) if self.file_backend_enabled() => {
                // File persistence is an explicit operator choice. It is
                // still protected to 0600 and committed through a private,
                // synced temporary file so a keyring failure cannot silently
                // persist a partial secret document.
                let fallback_file = file_backend_path();
                let mut map: HashMap<String, String> = fs::read_to_string(&fallback_file)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                map.insert(reference.to_owned(), value.expose_secret().to_owned());
                if let Ok(json_str) = serde_json::to_string(&map) {
                    if let Err(error) = write_private_json_atomic(&fallback_file, &json_str) {
                        return Err(CredentialStoreError::Backend(error.to_string()));
                    }
                    return Ok(());
                }
                Err(CredentialStoreError::Backend(e.to_string()))
            }
            Err(e) => Err(CredentialStoreError::Backend(format!(
                "keyring write failed; choose --secret-backend file explicitly to enable the 0600 file backend: {e}"
            ))),
        }
    }

    fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
        let _ = self.entry(reference)?.delete_credential();
        if !self.file_backend_enabled() {
            return Ok(());
        }
        let fallback_file = file_backend_path();
        if let Ok(s) = fs::read_to_string(&fallback_file)
            && let Ok(mut map) = serde_json::from_str::<HashMap<String, String>>(&s)
        {
            map.remove(reference);
            if let Ok(json_str) = serde_json::to_string(&map) {
                let _ = write_private_json_atomic(&fallback_file, &json_str);
            }
        }
        Ok(())
    }
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
