use std::sync::Arc;

use async_trait::async_trait;

use crate::http::HttpClient;
use crate::llm::post_chat_completion;
use crate::provider::{CredentialSource, ProviderConfigSource};
use crate::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse};

pub struct OpenAiCompatibleLlmEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

#[async_trait]
impl LlmEngine for OpenAiCompatibleLlmEngine {
    fn id(&self) -> &str {
        "openai-compatible"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["llama3".into()],
        }
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        let api_key = self
            .credentials
            .api_key(&self.provider_ref)
            .await
            .map_err(|e| EngineError::Auth(format!("keychain access failed: {e}")))?
            .ok_or_else(|| EngineError::Auth("missing api key".into()))?;
        let cfg = self
            .configs
            .config(&self.provider_ref)
            .await
            .ok_or_else(|| {
                EngineError::Config(format!(
                    "missing provider config for {}",
                    self.provider_ref
                ))
            })?;
        let model = req.model.as_deref().unwrap_or(&cfg.default_model);
        post_chat_completion(
            self.http.as_ref(),
            &cfg.base_url,
            model,
            &api_key,
            &req.prompt,
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestHttpClient;
    use crate::provider::ProviderConfig;
    use async_trait::async_trait;
    use std::collections::HashMap;
    use std::sync::Mutex;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    struct FakeCredentials {
        keys: Mutex<HashMap<String, String>>,
    }

    impl FakeCredentials {
        fn with_key(provider_ref: &str, key: &str) -> Arc<Self> {
            let mut keys = HashMap::new();
            keys.insert(provider_ref.to_string(), key.to_string());
            Arc::new(Self {
                keys: Mutex::new(keys),
            })
        }
    }

    #[async_trait]
    impl CredentialSource for FakeCredentials {
        async fn api_key(&self, provider_ref: &str) -> Result<Option<String>, String> {
            Ok(self.keys.lock().unwrap().get(provider_ref).cloned())
        }
    }

    struct FakeConfigs {
        entries: Mutex<HashMap<String, ProviderConfig>>,
    }

    impl FakeConfigs {
        fn with_config(provider_ref: &str, cfg: ProviderConfig) -> Arc<Self> {
            let mut entries = HashMap::new();
            entries.insert(provider_ref.to_string(), cfg);
            Arc::new(Self {
                entries: Mutex::new(entries),
            })
        }
    }

    #[async_trait]
    impl ProviderConfigSource for FakeConfigs {
        async fn config(&self, provider_ref: &str) -> Option<ProviderConfig> {
            self.entries.lock().unwrap().get(provider_ref).cloned()
        }
    }

    #[tokio::test]
    async fn hits_custom_base_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"choices":[{"message":{"content":"compatible"}}]}"#,
            ))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "local-llm",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "llama3".into(),
            },
        );
        let creds = FakeCredentials::with_key("local-llm", "local-key");

        let engine = OpenAiCompatibleLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "local-llm".into(),
        };
        let out = engine
            .complete(LlmRequest {
                prompt: "rewrite me".into(),
                model: None,
            })
            .await
            .unwrap();
        assert_eq!(out.text, "compatible");
    }

    #[tokio::test]
    async fn requires_provider_config() {
        let creds = FakeCredentials::with_key("local-llm", "local-key");
        let configs = Arc::new(FakeConfigs {
            entries: Mutex::new(HashMap::new()),
        });

        let engine = OpenAiCompatibleLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "local-llm".into(),
        };
        let err = engine
            .complete(LlmRequest {
                prompt: "rewrite me".into(),
                model: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing provider config"));
    }
}
