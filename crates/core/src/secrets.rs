use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Mutex;
use crate::error::KeaError;

#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn get(&self, provider_ref: &str) -> Result<Option<String>, KeaError>;
    async fn set(&self, provider_ref: &str, secret: &str) -> Result<(), KeaError>;
    async fn delete(&self, provider_ref: &str) -> Result<(), KeaError>;
}

#[derive(Default)]
pub struct InMemoryCredentialStore { map: Mutex<HashMap<String, String>> }

#[async_trait]
impl CredentialStore for InMemoryCredentialStore {
    async fn get(&self, p: &str) -> Result<Option<String>, KeaError> {
        Ok(self.map.lock().unwrap().get(p).cloned())
    }
    async fn set(&self, p: &str, s: &str) -> Result<(), KeaError> {
        self.map.lock().unwrap().insert(p.into(), s.into()); Ok(())
    }
    async fn delete(&self, p: &str) -> Result<(), KeaError> {
        self.map.lock().unwrap().remove(p); Ok(())
    }
}

pub struct KeyringCredentialStore { service: String }

impl KeyringCredentialStore {
    pub fn new(service: impl Into<String>) -> Self { Self { service: service.into() } }
    fn entry(&self, p: &str) -> Result<keyring::Entry, KeaError> {
        keyring::Entry::new(&self.service, p).map_err(|e| KeaError::Other(e.to_string()))
    }
}

#[async_trait]
impl CredentialStore for KeyringCredentialStore {
    async fn get(&self, p: &str) -> Result<Option<String>, KeaError> {
        match self.entry(p)?.get_password() {
            Ok(s) => Ok(Some(s)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(KeaError::Other(e.to_string())),
        }
    }
    async fn set(&self, p: &str, s: &str) -> Result<(), KeaError> {
        self.entry(p)?.set_password(s).map_err(|e| KeaError::Other(e.to_string()))
    }
    async fn delete(&self, p: &str) -> Result<(), KeaError> {
        match self.entry(p)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(KeaError::Other(e.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn in_memory_roundtrips() {
        let store = InMemoryCredentialStore::default();
        assert_eq!(store.get("openai").await.unwrap(), None);
        store.set("openai", "sk-test").await.unwrap();
        assert_eq!(store.get("openai").await.unwrap(), Some("sk-test".into()));
        store.delete("openai").await.unwrap();
        assert_eq!(store.get("openai").await.unwrap(), None);
    }
}

#[cfg(test)]
mod keyring_backing_tests {
    use super::*;

    /// The real store must actually persist. Without a platform feature the
    /// keyring crate falls back to a no-op backend where writes vanish.
    #[tokio::test]
    async fn keyring_store_roundtrips() {
        let store = KeyringCredentialStore::new("ai.kea.desktop.selftest");
        let _ = store.delete("probe").await;
        store.set("probe", "sk-roundtrip").await.unwrap();
        let got = store.get("probe").await.unwrap();
        let _ = store.delete("probe").await;
        assert_eq!(got, Some("sk-roundtrip".into()), "keyring did not persist the secret");
    }
}
