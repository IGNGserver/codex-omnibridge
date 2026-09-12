//! Credential storage isolated from the provider registry.
//!
//! The native implementation delegates to the operating system keyring. The
//! registry only stores the opaque `credential_reference` and never receives a
//! secret. Tests can use `MemoryCredentialStore` without touching the host
//! keyring.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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

        // Try file fallback first if present
        let config_dir = directories::ProjectDirs::from("dev", "codex", "codexmultiprovider")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let fallback_file = config_dir.join(".credentials");
        if let Ok(s) = std::fs::read_to_string(&fallback_file)
            && let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&s)
            && let Some(val) = map.get(reference)
        {
            return Ok(SecretString::from(val.clone()));
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
            Err(e) => {
                // In headless/container environments where Secret Service prompt is dismissed,
                // store in a secure 0600 file in user config as graceful fallback.
                let config_dir =
                    directories::ProjectDirs::from("dev", "codex", "codexmultiprovider")
                        .map(|d| d.config_dir().to_path_buf())
                        .unwrap_or_else(|| std::path::PathBuf::from("."));
                let _ = std::fs::create_dir_all(&config_dir);
                let fallback_file = config_dir.join(".credentials");
                let mut map: HashMap<String, String> = std::fs::read_to_string(&fallback_file)
                    .ok()
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_default();
                map.insert(reference.to_owned(), value.expose_secret().to_owned());
                if let Ok(json_str) = serde_json::to_string(&map) {
                    let write_result = {
                        #[cfg(unix)]
                        use std::os::unix::fs::OpenOptionsExt;
                        #[cfg(unix)]
                        let result = std::fs::OpenOptions::new()
                            .write(true)
                            .create(true)
                            .truncate(true)
                            .mode(0o600)
                            .open(&fallback_file)
                            .and_then(|mut f| {
                                std::io::Write::write_all(&mut f, json_str.as_bytes())
                            });
                        #[cfg(not(unix))]
                        let result = std::fs::write(&fallback_file, json_str);
                        result
                    };
                    if let Err(error) = write_result {
                        return Err(CredentialStoreError::Backend(error.to_string()));
                    }
                    if let Err(error) = set_private_file_permissions(&fallback_file) {
                        return Err(CredentialStoreError::Backend(error.to_string()));
                    }
                    return Ok(());
                }
                Err(CredentialStoreError::Backend(e.to_string()))
            }
        }
    }

    fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
        let _ = self.entry(reference)?.delete_credential();
        let config_dir = directories::ProjectDirs::from("dev", "codex", "codexmultiprovider")
            .map(|d| d.config_dir().to_path_buf())
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let fallback_file = config_dir.join(".credentials");
        if let Ok(s) = std::fs::read_to_string(&fallback_file)
            && let Ok(mut map) = serde_json::from_str::<HashMap<String, String>>(&s)
        {
            map.remove(reference);
            if let Ok(json_str) = serde_json::to_string(&map) {
                let _ = std::fs::write(&fallback_file, json_str);
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
}
