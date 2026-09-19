use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{Auth, HttpClient};
use crate::provider::{self, CredentialSource, Defaults, ProviderConfigSource, OPENAI_BASE_URL};
use crate::traits::{AudioPcm, EngineCaps, EngineError, TtsEngine, TtsOpts};
use crate::tts::audio::bytes_to_pcm_wav;

const DEFAULT_MODEL: &str = "tts-1";

pub struct OpenAiTtsEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

#[async_trait]
impl TtsEngine for OpenAiTtsEngine {
    fn id(&self) -> &str {
        "openai-tts"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["tts-1".into(), "tts-1-hd".into(), "gpt-4o-mini-tts".into()],
        }
    }

    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError> {
        let provider = provider::resolve(
            self.credentials.as_ref(),
            self.configs.as_ref(),
            opts.provider_ref.as_deref(),
            &self.provider_ref,
            Some(Defaults {
                base_url: OPENAI_BASE_URL,
                model: DEFAULT_MODEL,
            }),
        )
        .await?;
        // The hosted speech endpoint cannot serve an unauthenticated call.
        let api_key = provider.require_key()?;
        let model = opts.model.as_deref().unwrap_or(&provider.default_model);
        let voice = opts.voice.as_deref().unwrap_or("alloy");
        let format = opts.format.as_deref().unwrap_or("wav");
        let url = format!("{}/audio/speech", provider.base_url.trim_end_matches('/'));
        let body = serde_json::json!({
            "model": model,
            "input": text,
            "voice": voice,
            "response_format": format,
        });
        let bytes = self
            .http
            .post_binary(&url, Auth::Bearer(api_key), body)
            .await?;
        if format == "wav" {
            bytes_to_pcm_wav(&bytes)
        } else {
            Err(EngineError::Config(format!(
                "unsupported response format: {format}"
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestHttpClient;
    use crate::provider::ProviderConfig;
    use crate::stt::audio::pcm_to_wav_bytes;
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

    fn minimal_wav_fixture() -> Vec<u8> {
        pcm_to_wav_bytes(&AudioPcm {
            samples: vec![0.1, -0.1, 0.2],
            sample_rate_hz: 24_000,
        })
        .unwrap()
    }

    #[tokio::test]
    async fn openai_tts_synthesizes_wav() {
        let wav = minimal_wav_fixture();
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(wav))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "tts-1".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-test");

        let engine = OpenAiTtsEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let pcm = engine
            .synthesize(
                "hello",
                TtsOpts {
                    model: None,
                    voice: None,
                    format: Some("wav".into()),
                    provider_ref: Some("openai".into()),
                },
            )
            .await
            .unwrap();
        assert!(pcm.sample_rate_hz > 0);
        assert!(!pcm.samples.is_empty());
    }

    #[tokio::test]
    async fn server_rejection_becomes_an_auth_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/speech"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthorized"}"#))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "tts-1".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-bad");

        let engine = OpenAiTtsEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let err = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap_err();
        match &err {
            EngineError::Auth(msg) => assert!(msg.contains("401")),
            _ => panic!("expected EngineError::Auth, got {:?}", err),
        }
    }

    #[tokio::test]
    async fn errors_on_missing_credentials() {
        let engine = OpenAiTtsEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: FakeCredentials::empty(),
            configs: Arc::new(FakeConfigs {
                entries: Mutex::new(HashMap::new()),
            }),
            provider_ref: "openai".into(),
        };
        let err = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("missing api key"));
    }
}
