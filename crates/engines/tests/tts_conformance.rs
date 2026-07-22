use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kea_engines::{
    bytes_to_pcm_wav, CredentialSource, NoopTtsEngine, OpenAiTtsEngine, ProviderConfig,
    ProviderConfigSource, ReqwestHttpClient, TtsEngine, TtsOpts,
};
use kea_engines::stt::audio::pcm_to_wav_bytes;
use kea_engines::traits::AudioPcm;
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
        samples: vec![0.1, -0.1, 0.2, 0.0],
        sample_rate_hz: 24_000,
    })
    .unwrap()
}

struct TtsCase {
    name: &'static str,
    engine: Arc<dyn TtsEngine>,
    text: &'static str,
    opts: TtsOpts,
    expect_error: bool,
    expect_sample_rate: Option<u32>,
}

#[tokio::test]
async fn tts_engine_trait_conformance() {
    let server = MockServer::start().await;
    let wav = minimal_wav_fixture();
    Mock::given(method("POST"))
        .and(path("/v1/audio/speech"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(wav))
        .mount(&server)
        .await;

    let openai_with_creds = OpenAiTtsEngine {
        http: Arc::new(ReqwestHttpClient::new()),
        credentials: FakeCredentials::with_key("openai", "sk-test"),
        configs: FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "tts-1".into(),
            },
        ),
        provider_ref: "openai".into(),
    };

    let openai_no_creds = OpenAiTtsEngine {
        http: Arc::new(ReqwestHttpClient::new()),
        credentials: FakeCredentials::empty(),
        configs: FakeConfigs::with_config(
            "openai",
            ProviderConfig {
                base_url: format!("{}/v1", server.uri()),
                default_model: "tts-1".into(),
            },
        ),
        provider_ref: "openai".into(),
    };

    let cases = vec![
        TtsCase {
            name: "noop_tts_nonempty_text",
            engine: Arc::new(NoopTtsEngine),
            text: "hello",
            opts: TtsOpts::default(),
            expect_error: false,
            expect_sample_rate: Some(24_000),
        },
        TtsCase {
            name: "openai_tts_nonempty_text",
            engine: Arc::new(openai_with_creds),
            text: "hello",
            opts: TtsOpts {
                format: Some("wav".into()),
                provider_ref: Some("openai".into()),
                ..TtsOpts::default()
            },
            expect_error: false,
            expect_sample_rate: Some(24_000),
        },
        TtsCase {
            name: "openai_tts_missing_credentials",
            engine: Arc::new(openai_no_creds),
            text: "hello",
            opts: TtsOpts::default(),
            expect_error: true,
            expect_sample_rate: None,
        },
    ];

    for case in cases {
        let caps = case.engine.capabilities();
        assert!(
            !caps.models.is_empty(),
            "{}: capabilities.models must be non-empty",
            case.name
        );
        assert_eq!(
            case.engine.id(),
            case.engine.id(),
            "{}: id must be stable",
            case.name
        );

        let result = case.engine.synthesize(case.text, case.opts).await;
        if case.expect_error {
            assert!(result.is_err(), "{}: expected error", case.name);
            continue;
        }

        let pcm = result.expect(&case.name);
        assert!(!pcm.samples.is_empty(), "{}: samples must be non-empty", case.name);
        if let Some(rate) = case.expect_sample_rate {
            assert_eq!(
                pcm.sample_rate_hz, rate,
                "{}: unexpected sample rate",
                case.name
            );
        }
    }
}

#[test]
fn bytes_to_pcm_wav_decodes_fixture() {
    let wav = minimal_wav_fixture();
    let pcm = bytes_to_pcm_wav(&wav).unwrap();
    assert_eq!(pcm.sample_rate_hz, 24_000);
    assert!(!pcm.samples.is_empty());
}
