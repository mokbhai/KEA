pub mod http;
pub mod llm;
pub mod noop;
pub mod provider;
pub mod registry;
pub mod stt;
pub mod traits;
pub mod tts;

pub use http::{HttpClient, MultipartPart, ReqwestHttpClient};
pub use llm::{OpenAiCompatibleLlmEngine, OpenAiLlmEngine};
pub use noop::{NoopLlmEngine, NoopSttEngine, NoopTtsEngine};
pub use provider::{CredentialSource, ProviderConfig, ProviderConfigSource};
pub use registry::EngineRegistry;
pub use stt::{OpenAiSttEngine, ParakeetSttEngine, WhisperSttEngine, pcm_to_wav_bytes};
pub use tts::{LocalTtsEngine, OpenAiTtsEngine, bytes_to_pcm_wav};

#[cfg(feature = "whisper")]
pub use stt::register_whisper_stt_engine;

#[cfg(feature = "parakeet")]
pub use stt::register_parakeet_stt_engine;

#[cfg(feature = "tts-local")]
pub use tts::register_sherpa_tts_engine;
pub use traits::*;

use std::sync::Arc;

pub fn register_phase1_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialSource>,
    configs: Arc<dyn ProviderConfigSource>,
) {
    reg.register_llm(Arc::new(OpenAiLlmEngine {
        http: http.clone(),
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "openai".into(),
    }));
    reg.register_llm(Arc::new(OpenAiCompatibleLlmEngine {
        http,
        credentials,
        configs,
        provider_ref: "local-llm".into(),
    }));
}

pub fn register_phase2_stt_engines(
    reg: &mut EngineRegistry,
    http: Arc<dyn HttpClient>,
    credentials: Arc<dyn CredentialSource>,
    configs: Arc<dyn ProviderConfigSource>,
) {
    reg.register_stt(Arc::new(OpenAiSttEngine {
        http: http.clone(),
        credentials: credentials.clone(),
        configs: configs.clone(),
        provider_ref: "openai".into(),
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
