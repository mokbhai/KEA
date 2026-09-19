use std::sync::Arc;

use async_trait::async_trait;

use crate::http::{Auth, HttpClient, MultipartPart};
use crate::provider::{self, CredentialSource, Defaults, ProviderConfigSource, OPENAI_BASE_URL};
use crate::stt::audio::pcm_to_wav_bytes;
use crate::stt::segments::from_openai_verbose;
use crate::traits::{AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, Transcript};

const DEFAULT_MODEL: &str = "whisper-1";

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
        // The hosted transcription endpoint cannot serve an unauthenticated
        // call, so fail before uploading the audio.
        let api_key = provider.require_key()?;
        let model = opts.model.as_deref().unwrap_or(&provider.default_model);
        let wav = pcm_to_wav_bytes(&audio)?;
        let url = format!(
            "{}/audio/transcriptions",
            provider.base_url.trim_end_matches('/')
        );
        let mut parts = vec![
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
            // Asks for per-segment timing, which the subtitle writers need.
            // Sent unconditionally because the response is parsed
            // defensively: an endpoint that ignores this field answers with
            // the plain body it always did, and that still parses. Word-level
            // granularity is deliberately not requested — subtitles do not
            // need it and it is another field for a compatible endpoint to
            // reject outright.
            MultipartPart {
                name: "response_format".into(),
                filename: None,
                content_type: None,
                data: b"verbose_json".to_vec(),
            },
        ];
        // The endpoint's `prompt` field biases decoding toward spellings it
        // would otherwise guess at. Sent only when there is something to say:
        // an empty prompt is not neutral, it is a prompt that says nothing and
        // still costs a multipart field.
        if !opts.vocabulary.is_empty() {
            parts.push(MultipartPart {
                name: "prompt".into(),
                filename: None,
                content_type: None,
                data: opts.vocabulary.join(", ").into_bytes(),
            });
        }
        let text = self
            .http
            .post_multipart(&url, Auth::Bearer(api_key), parts)
            .await?;
        let parsed: serde_json::Value =
            serde_json::from_str(&text).map_err(|e| EngineError::Other(e.to_string()))?;
        let content = parsed["text"]
            .as_str()
            .ok_or_else(|| EngineError::Other("missing text field".into()))?;
        Ok(Transcript {
            text: content.to_string(),
            segments: from_openai_verbose(&parsed),
        })
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

    /// Groq's whole integration.
    ///
    /// It serves OpenAI's own transcription wire format, so it needs no
    /// engine of its own — only a `provider_ref` on the binding. This asserts
    /// the two halves of that claim: that the request goes to Groq's
    /// endpoint, and that the default model is the one that makes Groq worth
    /// selecting, with nothing stored in provider config.
    #[tokio::test]
    async fn groq_is_reached_through_this_engine_with_no_stored_config() {
        let resolved = crate::provider::resolve(
            FakeCredentials::with_key("groq", "gsk-test").as_ref(),
            // Deliberately empty: a user who added a Groq key configured
            // nothing else.
            Arc::new(FakeConfigs {
                entries: Mutex::new(HashMap::new()),
            })
            .as_ref(),
            Some("groq"),
            "openai",
            Some(Defaults {
                base_url: OPENAI_BASE_URL,
                model: DEFAULT_MODEL,
            }),
        )
        .await
        .unwrap();
        assert_eq!(resolved.base_url, "https://api.groq.com/openai/v1");
        assert_eq!(resolved.default_model, "whisper-large-v3-turbo");

        // And the request this engine builds from that lands on the
        // compatible path.
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openai/v1/audio/transcriptions"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer gsk-test",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"text":"fast"}"#))
            .mount(&server)
            .await;
        let engine = OpenAiSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: FakeCredentials::with_key("groq", "gsk-test"),
            configs: FakeConfigs::with_config(
                "groq",
                ProviderConfig {
                    base_url: format!("{}/openai/v1", server.uri()),
                    default_model: "whisper-large-v3-turbo".into(),
                },
            ),
            provider_ref: "openai".into(),
        };
        let out = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                SttOpts {
                    provider_ref: Some("groq".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(out.text, "fast");
    }

    #[tokio::test]
    async fn transcribes_against_mock_openai_stt() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"text":"dictated text"}"#))
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
                    vocabulary: Vec::new(),
                },
            )
            .await
            .unwrap();
        assert_eq!(out.text, "dictated text");
        // The bare body an OpenAI-compatible endpoint answers with: no
        // timing, and emphatically not an error.
        assert!(out.segments.is_empty());
    }

    /// The other half of the same contract: when the endpoint *does* honour
    /// `verbose_json`, the timing has to land in the transcript.
    #[tokio::test]
    async fn verbose_json_segments_reach_the_transcript() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                r#"{"text":"one two","segments":[{"start":0.0,"end":1.5,"text":" one"},{"start":1.5,"end":3.0,"text":" two"}]}"#,
            ))
            .mount(&server)
            .await;

        let configs = FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "whisper-1".into(),
            },
        );
        let engine = OpenAiSttEngine {
            http: Arc::new(ReqwestHttpClient::new()),
            credentials: FakeCredentials::with_key("openai", "sk-test"),
            configs,
            provider_ref: "openai".into(),
        };
        let out = engine
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                SttOpts::default(),
            )
            .await
            .unwrap();
        assert_eq!(out.text, "one two");
        assert_eq!(out.segments.len(), 2);
        assert_eq!(out.segments[0].end_ms, 1_500);
        assert_eq!(out.segments[1].text, "two");
    }

    #[tokio::test]
    async fn server_rejection_becomes_an_auth_error() {
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
                    vocabulary: Vec::new(),
                },
            )
            .await
            .unwrap_err();
        match &err {
            EngineError::Auth(msg) => assert!(msg.contains("401")),
            _ => panic!("expected EngineError::Auth, got {:?}", err),
        }
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
        assert!(err
            .to_string()
            .contains("check the API key in provider settings"));
    }

    #[tokio::test]
    async fn rate_limit_is_retryable() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(429).set_body_string(r#"{"error":"rate limited"}"#))
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
            EngineError::Retryable { status, .. } => assert_eq!(*status, 429),
            _ => panic!("expected EngineError::Retryable, got {:?}", err),
        }
    }
}
