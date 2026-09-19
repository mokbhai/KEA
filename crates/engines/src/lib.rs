pub mod http;
pub mod llm;
pub mod noop;
pub mod provider;
pub mod registry;
pub mod stt;
pub mod traits;
pub mod tts;

pub use http::{Auth, ByteStream, HeaderPairs, HttpClient, MultipartPart, ReqwestHttpClient};
pub use llm::{
    discover_local_llms, AnthropicLlmEngine, LlmStream, LocalLlmServer, OpenAiCompatibleLlmEngine,
    OpenAiLlmEngine, ProbeTransport, ReqwestProbe, StreamingLlmEngine, ANTHROPIC_BASE_URL,
    ANTHROPIC_LLM_ENGINE_ID, LOCAL_LLM_PROBES,
};
pub use noop::{NoopLlmEngine, NoopSttEngine, NoopTtsEngine};
pub use provider::{
    well_known, CredentialSource, Defaults, ProviderConfig, ProviderConfigSource, ResolvedProvider,
    OPENAI_BASE_URL, WELL_KNOWN_PROVIDERS,
};
pub use registry::EngineRegistry;
pub use stt::{
    pcm_to_wav_bytes, resample_to_rate, DeepgramSttEngine, ElevenLabsSttEngine, OpenAiSttEngine,
    ParakeetSttEngine, StreamingZipformerEngine, WhisperSttEngine, DEEPGRAM_BASE_URL,
    DEEPGRAM_STT_ENGINE_ID, ELEVENLABS_BASE_URL, ELEVENLABS_STT_ENGINE_ID, STREAMING_STT_ENGINE_ID,
    STT_SAMPLE_RATE_HZ,
};
pub use tts::{bytes_to_pcm_wav, LocalTtsEngine, OpenAiTtsEngine};

#[cfg(feature = "whisper")]
pub use stt::register_whisper_stt_engine;

#[cfg(feature = "parakeet")]
pub use stt::register_parakeet_stt_engine;

#[cfg(feature = "streaming")]
pub use stt::register_streaming_stt_engine;

pub use traits::*;
#[cfg(feature = "tts-local")]
pub use tts::register_sherpa_tts_engine;

#[cfg(feature = "tts-system")]
pub use tts::{register_system_tts_engine, SystemTtsEngine, SYSTEM_TTS_ENGINE_ID};

#[cfg(feature = "stt-apple")]
pub use stt::{register_apple_stt_engine, AppleSttEngine, APPLE_STT_ENGINE_ID, APPLE_STT_MODEL};

use std::sync::Arc;

pub fn register_phase1_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialSource>,
    configs: Arc<dyn ProviderConfigSource>,
) {
    // Each engine is constructed once and registered twice — under
    // `LlmEngine` for the buffered `complete`, and under
    // `StreamingLlmEngine` for the incremental one. Two `Arc`s of the same
    // object rather than two objects, so a provider resolved one way cannot
    // resolve differently the other.
    let openai = Arc::new(OpenAiLlmEngine {
        http: http.clone(),
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "openai".into(),
    });
    reg.register_llm(openai.clone());
    reg.register_streaming_llm(openai);

    let anthropic = Arc::new(AnthropicLlmEngine {
        http: http.clone(),
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "anthropic".into(),
    });
    reg.register_llm(anthropic.clone());
    reg.register_streaming_llm(anthropic);

    // One compatible instance serves every user-added provider — including
    // the Ollama or LM Studio server `discover_local_llms` found, which is
    // stored as an ordinary provider with a base URL. See
    // `OpenAiCompatibleLlmEngine::resolve`.
    let compatible = Arc::new(OpenAiCompatibleLlmEngine {
        http,
        credentials,
        configs,
        provider_ref: "local-llm".into(),
    });
    reg.register_llm(compatible.clone());
    reg.register_streaming_llm(compatible);
}

pub fn register_phase2_stt_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialSource>,
    configs: Arc<dyn ProviderConfigSource>,
) {
    // Serves Groq too: `whisper-large-v3-turbo` behind an OpenAI-compatible
    // `/audio/transcriptions`, reached by binding this engine to the `groq`
    // provider_ref. That is the whole Groq integration — see
    // `provider::WELL_KNOWN_PROVIDERS` for why it needs no base URL typed in.
    reg.register_stt(Arc::new(OpenAiSttEngine {
        http: http.clone(),
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "openai".into(),
    }));
    reg.register_stt(Arc::new(DeepgramSttEngine {
        http: http.clone(),
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "deepgram".into(),
    }));
    reg.register_stt(Arc::new(ElevenLabsSttEngine {
        http,
        credentials,
        configs,
        provider_ref: "elevenlabs".into(),
    }));
}

pub fn register_phase4_tts_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialSource>,
    configs: Arc<dyn ProviderConfigSource>,
) {
    reg.register_tts(Arc::new(OpenAiTtsEngine {
        http,
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "openai".into(),
    }));
}

#[cfg(test)]
mod register_tests {
    use super::*;
    use async_trait::async_trait;

    struct FakeCredentials;

    #[async_trait]
    impl CredentialSource for FakeCredentials {
        async fn api_key(&self, _provider_ref: &str) -> Result<Option<String>, String> {
            Ok(None)
        }
    }

    struct FakeConfigs;

    #[async_trait]
    impl ProviderConfigSource for FakeConfigs {
        async fn config(&self, _provider_ref: &str) -> Option<ProviderConfig> {
            None
        }
    }

    #[test]
    fn registers_openai_engines() {
        let mut reg = EngineRegistry::default();
        register_phase1_engines(
            &mut reg,
            Arc::new(ReqwestHttpClient::new()),
            Arc::new(FakeCredentials),
            Arc::new(FakeConfigs),
        );
        let ids = reg.list_llm_ids();
        assert!(ids.contains(&"openai".to_string()));
        assert!(ids.contains(&"openai-compatible".to_string()));
        // Anthropic's native `/v1/messages`, which the compatible engine
        // cannot serve — see `llm::anthropic`.
        assert!(ids.contains(&ANTHROPIC_LLM_ENGINE_ID.to_string()));

        // Every one of them also answers as a streaming engine, under the
        // same id, so a caller can ask for live text by the id it already
        // holds.
        let streaming = reg.list_streaming_llm_ids();
        assert_eq!(streaming, ids);
        for id in &ids {
            assert!(reg.streaming_llm(id).is_some(), "{id} cannot stream");
        }
    }

    #[test]
    fn registers_openai_stt_engine() {
        let mut reg = EngineRegistry::default();
        register_phase2_stt_engines(
            &mut reg,
            Arc::new(ReqwestHttpClient::new()),
            Arc::new(FakeCredentials),
            Arc::new(FakeConfigs),
        );
        let ids = reg.list_stt_ids();
        assert!(ids.contains(&"openai-stt".to_string()));
        assert!(ids.contains(&DEEPGRAM_STT_ENGINE_ID.to_string()));
        assert!(ids.contains(&ELEVENLABS_STT_ENGINE_ID.to_string()));
        // Groq needs no id of its own: it rides `openai-stt` with a
        // provider_ref. See `provider::WELL_KNOWN_PROVIDERS`.
        assert!(!ids.contains(&"groq".to_string()));
        assert!(!ids.contains(&"whisper".to_string()));
    }

    #[test]
    fn registers_openai_tts_engine() {
        let mut reg = EngineRegistry::default();
        register_phase4_tts_engines(
            &mut reg,
            Arc::new(ReqwestHttpClient::new()),
            Arc::new(FakeCredentials),
            Arc::new(FakeConfigs),
        );
        let ids = reg.list_tts_ids();
        assert!(ids.contains(&"openai-tts".to_string()));
        assert!(!ids.contains(&"sherpa-tts".to_string()));
    }
}
