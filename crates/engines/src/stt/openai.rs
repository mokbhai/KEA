use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{HttpClient, MultipartPart};
use crate::provider::{CredentialSource, ProviderConfig, ProviderConfigSource};
use crate::stt::audio::pcm_to_wav_bytes;
use crate::traits::{AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, Transcript};

pub struct OpenAiSttEngine {
    pub http: Arc<dyn HttpClient>,
    pub credentials: Arc<dyn CredentialSource>,
    pub configs: Arc<dyn ProviderConfigSource>,
    pub provider_ref: String,
}

#[async_trait]
impl SttEngine for OpenAiSttEngine {
    fn id(&self) -> &str {
        "openai-stt"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["whisper-1".into(), "gpt-4o-mini-transcribe".into()],
        }
    }

    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let provider_ref = opts
            .provider_ref
            .as_deref()
            .unwrap_or(&self.provider_ref);
        let api_key = self
            .credentials
            .api_key(provider_ref)
            .await
            .map_err(|e| EngineError::Auth(format!("keychain access failed: {e}")))?
            .ok_or_else(|| EngineError::Auth("missing api key".into()))?;
        let cfg = self
            .configs
            .config(provider_ref)
            .await
            .unwrap_or(ProviderConfig {
                base_url: "https://api.openai.com/v1".into(),
                default_model: "whisper-1".into(),
            });
        let model = opts.model.as_deref().unwrap_or(&cfg.default_model);
        let wav = pcm_to_wav_bytes(&audio)?;
        let url = format!(
            "{}/audio/transcriptions",
            cfg.base_url.trim_end_matches('/')
        );
        let parts = vec![
            MultipartPart {
                name: "file".into(),
                filename: Some("audio.wav".into()),
                content_type: Some("audio/wav".into()),
                data: wav,
            },
            MultipartPart {
                name: "model".into(),
                filename: None,
                content_type: None,
                data: model.as_bytes().to_vec(),
            },
        ];
        let (status, text) = self.http.post_multipart(&url, &api_key, parts).await?;
        if status != 200 {
            return Err(EngineError::http(status, text));
        }
        let parsed: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| EngineError::Other(e.to_string()))?;
        let content = parsed["text"]
            .as_str()
            .ok_or_else(|| EngineError::Other("missing text field".into()))?;
        Ok(Transcript {
            text: content.to_string(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::ReqwestHttpClient;
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
    async fn transcribes_against_mock_openai_stt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"text":"dictated text"}"#),
            )
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "whisper-1".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-test");

        let engine = OpenAiSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let out = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: None,
                    language: None,
                    provider_ref: Some("openai".into()),
                },
            )
            .await
            .unwrap();
        assert_eq!(out.text, "dictated text");
    }

    #[tokio::test]
    async fn maps_non_2xx_to_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(401).set_body_string(r#"{"error":"unauthorized"}"#))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "whisper-1".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-bad");

        let engine = OpenAiSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let err = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    model: None,
                    language: None,
                    provider_ref: Some("openai".into()),
                },
            )
            .await
            .unwrap_err();
        assert!(err.to_string().contains("401"));
    }

    #[tokio::test]
    async fn errors_auth_when_missing_credentials() {
        let creds = Arc::new(FakeCredentials {
            keys: Mutex::new(HashMap::new()),
        });
        let engine = OpenAiSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs: Arc::new(FakeConfigs {
                entries: Mutex::new(HashMap::new()),
            }),
            provider_ref: "openai".into(),
        };
        let err = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts::default(),
            )
            .await
            .unwrap_err();
        match &err {
            EngineError::Auth(msg) => assert!(msg.contains("missing api key")),
            _ => panic!("expected EngineError::Auth, got {:?}", err),
        }
        assert!(err.to_string().contains("check the API key in provider settings"));
    }

    #[tokio::test]
    async fn http_error_carries_status() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(
                ResponseTemplate::new(429).set_body_string(r#"{"error":"rate limited"}"#),
            )
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "whisper-1".into(),
            },
        );
        let creds = FakeCredentials::with_key("openai", "sk-test");
        let engine = OpenAiSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: creds,
            configs,
            provider_ref: "openai".into(),
        };
        let err = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                SttOpts::default(),
            )
            .await
            .unwrap_err();
        match &err {
            EngineError::Http { status, .. } => assert_eq!(*status, 429),
            _ => panic!("expected EngineError::Http, got {:?}", err),
        }
    }
}
