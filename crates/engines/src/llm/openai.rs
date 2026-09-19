use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{Auth, HttpClient};
use crate::llm::post_chat_completion;
use crate::provider::{self, CredentialSource, Defaults, ProviderConfigSource, OPENAI_BASE_URL};
use crate::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse};

const DEFAULT_MODEL: &str = "gpt-4o-mini";

pub struct OpenAiLlmEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

#[async_trait]
impl LlmEngine for OpenAiLlmEngine {
    fn id(&self) -> &str {
        "openai"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["gpt-4o-mini".into(), "gpt-4o".into()],
        }
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        // Same seam as the compatible engine: the resolved binding names the
        // provider, `self.provider_ref` is only the fallback for a binding
        // that carries none (an auto-resolved slot).
        let provider = provider::resolve(
            self.credentials.as_ref(),
            self.configs.as_ref(),
            req.provider_ref.as_deref(),
            &self.provider_ref,
            Some(Defaults {
                base_url: OPENAI_BASE_URL,
                model: DEFAULT_MODEL,
            }),
        )
        .await?;
        // api.openai.com cannot serve an unauthenticated call, so say so
        // before the round-trip rather than relaying its 401.
        let api_key = provider.require_key()?;
        let model = req.model.as_deref().unwrap_or(&provider.default_model);
        post_chat_completion(
            self.http.as_ref(),
            &provider.base_url,
            model,
            Auth::Bearer(api_key),
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
    use wiremock::matchers::{body_string_contains, method, path};
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
    async fn completes_against_mock_openai() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"rewritten"}}]}"#),
            )
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "gpt-4o-mini".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-test");

        let engine = OpenAiLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let out = engine
            .complete(LlmRequest {
                prompt: "fix this".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();
        assert_eq!(out.text, "rewritten");
    }

    #[tokio::test]
    async fn maps_non_2xx_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthorized"}"#))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "gpt-4o-mini".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-bad");

        let engine = OpenAiLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let err = engine
            .complete(LlmRequest {
                prompt: "fix this".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("401"));
    }

    #[tokio::test]
    async fn http_error_carries_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(503).set_body_string("service unavailable"))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "gpt-4o-mini".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-test");

        let engine = OpenAiLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let err = engine
            .complete(LlmRequest {
                prompt: "test".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap_err();
        match &err {
            EngineError::Retryable { status, body } => {
                assert_eq!(*status, 503);
                assert!(body.contains("service unavailable"));
            }
            _ => panic!("expected EngineError::Retryable, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn bound_model_appears_in_request_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .and(body_string_contains("\"gpt-4o\""))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"bound"}}]}"#),
            )
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "gpt-4o-mini".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-test");

        let engine = OpenAiLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let out = engine
            .complete(LlmRequest {
                prompt: "test".into(),
                model: Some("gpt-4o".into()),
                provider_ref: None,
            })
            .await
            .unwrap();
        assert_eq!(out.text, "bound");
    }

    /// Same seam as the compatible engine: a binding may point this engine at
    /// a provider other than the built-in "openai" (an OpenAI-shaped gateway,
    /// say). Reading `self.provider_ref` regardless would take the wrong key.
    #[tokio::test]
    async fn request_provider_ref_beats_the_registered_default() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"gateway"}}]}"#),
            )
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "omni",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "omni-large".into(),
            },
        );
        let creds = FakeCredentials::with_key("omni", "omni-key");

        let engine = OpenAiLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let out = engine
            .complete(LlmRequest {
                prompt: "fix this".into(),
                model: None,
                provider_ref: Some("omni".into()),
            })
            .await
            .unwrap();
        assert_eq!(out.text, "gateway");
    }
}
