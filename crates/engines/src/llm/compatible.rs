use std::sync::Arc;

use async_trait::async_trait;

use crate::http::HttpClient;
use crate::llm::post_chat_completion;
use crate::provider::{self, CredentialSource, ProviderConfigSource};
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
        // One registered instance serves every OpenAI-compatible provider the
        // user added, so the request's provider_ref (from the resolved
        // binding) decides whose key and base URL to use. `self.provider_ref`
        // is only the fallback for a binding that names no provider. There is
        // no "the" compatible endpoint, so no defaults: an unconfigured
        // provider fails closed.
        let provider = provider::resolve(
            self.credentials.as_ref(),
            self.configs.as_ref(),
            req.provider_ref.as_deref(),
            &self.provider_ref,
            None,
        )
        .await?;
        let model = req.model.as_deref().unwrap_or(&provider.default_model);
        // No key is a supported setup here: Ollama, LM Studio and llama.cpp
        // all serve unauthenticated, and the provider UI says "No key needed".
        post_chat_completion(
            self.http.as_ref(),
            &provider.base_url,
            model,
            provider.auth(),
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

        fn empty() -> Arc<Self> {
            Arc::new(Self {
                keys: Mutex::new(HashMap::new()),
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
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"compatible"}}]}"#),
            )
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
                provider_ref: None,
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
                provider_ref: None,
            })
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing provider config"));
    }

    /// The regression this file exists to prevent: a user-added provider.
    ///
    /// `register_phase1_engines` registers exactly one compatible engine, with
    /// `provider_ref: "local-llm"`, because the registry is keyed by engine
    /// id. Every custom provider the user adds ("omni" here) therefore shares
    /// it and can only be identified by the provider_ref the resolved binding
    /// carries. When that was dropped the engine read local-llm's credential —
    /// which does not exist — and the user saw "missing api key" for a key
    /// they had just saved and successfully tested.
    #[tokio::test]
    async fn request_provider_ref_beats_the_registered_default() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"omni"}}]}"#),
            )
            .mount(&server)
            .await;

        // Only "omni" is configured and keyed — exactly the state after the
        // user adds a custom provider and saves its base URL and key.
        let configs = FakeConfigs::with_config(
            "omni",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "omni-large".into(),
            },
        );
        let creds = FakeCredentials::with_key("omni", "omni-key");

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
                provider_ref: Some("omni".into()),
            })
            .await
            .unwrap();
        assert_eq!(out.text, "omni");
    }

    /// A local server needs no key, and the app's provider UI says as much
    /// ("No key needed" for local-llm). Demanding a bearer here made a user
    /// who had finished local-server onboarding hit "missing api key" for a
    /// key they were told not to set.
    #[tokio::test]
    async fn a_keyless_local_server_works() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(r#"{"choices":[{"message":{"content":"local"}}]}"#),
            )
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "local-llm",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "llama3".into(),
            },
        );

        let engine = OpenAiCompatibleLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: FakeCredentials::empty(),
            configs,
            provider_ref: "local-llm".into(),
        };
        let out = engine
            .complete(LlmRequest {
                prompt: "rewrite me".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();
        assert_eq!(out.text, "local");
    }

    /// A wrong key is the server's 401, and it must reach the user as the
    /// "check the API key" message rather than a bare status line.
    #[tokio::test]
    async fn server_rejection_becomes_an_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/chat/completions"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"bad key"}"#))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "local-llm",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "llama3".into(),
            },
        );

        let engine = OpenAiCompatibleLlmEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: FakeCredentials::with_key("local-llm", "wrong"),
            configs,
            provider_ref: "local-llm".into(),
        };
        let err = engine
            .complete(LlmRequest {
                prompt: "rewrite me".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap_err();
        match &err {
            EngineError::Auth(msg) => assert!(msg.contains("401")),
            _ => panic!("expected EngineError::Auth, got {:?}", err),
        }
        assert!(err
            .to_string()
            .contains("check the API key in provider settings"));
    }
}
