use async_trait::async_trait;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub base_url: String,
    pub default_model: String,
}

#[async_trait]
pub trait CredentialSource: Send + Sync {
    /// Returns the API key/secret for a provider_ref.
    ///
    /// `Ok(None)` means the key has never been set.
    /// `Err(...)` means the credential store (e.g. OS keyring) failed.
    async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String>;
}

#[async_trait]
pub trait ProviderConfigSource: Send + Sync {
    /// Returns the base_url + default_model for a provider_ref, if configured.
    async fn config(&self, provider_ref: &str) -> Option<ProviderConfig>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_config_constructs() {
        let cfg = ProviderConfig {
            base_url: "https://api.openai.com/v1".into(),
            default_model: "gpt-4o-mini".into(),
        };
        assert_eq!(cfg.base_url, "https://api.openai.com/v1");
        assert_eq!(cfg.default_model, "gpt-4o-mini");
    }
}
