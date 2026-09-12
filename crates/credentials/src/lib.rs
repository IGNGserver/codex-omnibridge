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
        self.entry(reference)?
            .set_password(value.expose_secret())
            .map_err(|error| CredentialStoreError::Backend(error.to_string()))
    }

    fn delete(&self, reference: &str) -> Result<(), CredentialStoreError> {
        self.entry(reference)?
            .delete_credential()
            .map_err(|error| CredentialStoreError::Backend(error.to_string()))
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
