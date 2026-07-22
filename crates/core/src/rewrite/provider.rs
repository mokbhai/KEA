use async_trait::async_trait;
use kea_engines::{CredentialSource, ProviderConfig, ProviderConfigSource};
use std::sync::Arc;

use crate::error::KeaError;
use crate::secrets::CredentialStore;
use crate::store::settings::SettingsRepo;

pub struct ProviderConfigRepo {
    settings: SettingsRepo,
}

impl ProviderConfigRepo {
    pub fn new(settings: SettingsRepo) -> Self {
        Self { settings }
    }

    fn key(provider_ref: &str) -> String {
        format!("provider.{provider_ref}")
    }

    pub async fn get(&self, provider_ref: &str) -> Result<Option<ProviderConfig>, KeaError> {
        self.settings.get(&Self::key(provider_ref)).await
    }

    pub async fn set(&self, provider_ref: &str, cfg: &ProviderConfig) -> Result<(), KeaError> {
        self.settings.set(&Self::key(provider_ref), cfg).await
    }
}

#[async_trait]
impl ProviderConfigSource for ProviderConfigRepo {
    async fn config(&self, provider_ref: &str) -> Option<ProviderConfig> {
        self.get(provider_ref).await.ok().flatten()
    }
}

/// Adapts core's `CredentialStore` to the engines `CredentialSource` seam.
pub struct CredentialSourceAdapter {
    store: Arc<dyn CredentialStore>,
}

impl CredentialSourceAdapter {
    pub fn new(store: Arc<dyn CredentialStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl CredentialSource for CredentialSourceAdapter {
    async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String> {
        match self.store.get(provider_ref).await {
            Ok(val) => Ok(val),
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    provider_ref = %provider_ref,
                    "keychain access failed while reading credential"
                );
                Err(format!("keychain access failed: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::InMemoryCredentialStore;
    use crate::store::db::{open_pool, run_config_migrations};

    #[tokio::test]
    async fn roundtrips_provider_config() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = ProviderConfigRepo::new(SettingsRepo::new(pool));
        let cfg = ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
        };
        repo.set("openai", &cfg).await.unwrap();
        assert_eq!(repo.get("openai").await.unwrap(), Some(cfg));
    }

    #[tokio::test]
    async fn implements_provider_config_source() {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&pool).await.unwrap();
        let repo = ProviderConfigRepo::new(SettingsRepo::new(pool));
        let cfg = ProviderConfig {
            base_url: "https://api.example.com/v1".into(),
            default_model: "test-model".into(),
        };
        repo.set("local-llm", &cfg).await.unwrap();
        assert_eq!(
            ProviderConfigSource::config(&repo, "local-llm").await,
            Some(cfg)
        );
    }

    #[tokio::test]
    async fn credential_source_adapter_forwards_store() {
        let store = Arc::new(InMemoryCredentialStore::default());
        store.set("openai", "sk-test").await.unwrap();
        let adapter = CredentialSourceAdapter::new(store);
        assert_eq!(
            CredentialSource::api_key(&adapter, "openai").await.unwrap(),
            Some("sk-test".into())
        );
        assert_eq!(
            CredentialSource::api_key(&adapter, "missing").await.unwrap(),
            None
        );
    }
}
