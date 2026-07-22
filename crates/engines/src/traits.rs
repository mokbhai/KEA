use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum EngineError {
    #[error("authentication failed: {0} — check the API key in provider settings")]
    Auth(String),
    #[error("HTTP {status}: {body}")]
    Http { status: u16, body: String },
    #[error("bad configuration: {0}")]
    Config(String),
    #[error("model not installed: {0} — download it in Settings")]
    ModelNotInstalled(String),
    #[error("{0}")]
    Other(String),
}

impl EngineError {
    pub fn http(status: u16, body: String) -> Self {
        let body = if body.len() > 200 {
            let truncated: String = body.chars().take(200).collect();
            format!("{}…", truncated)
        } else {
            body
        };
        EngineError::Http { status, body }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct EngineCaps { pub models: Vec<String> }

#[derive(Debug, Clone, Deserialize)]
pub struct LlmRequest {
    pub prompt: String,
    #[serde(default)]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LlmResponse { pub text: String }

#[async_trait]
pub trait LlmEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError>;
}

#[derive(Debug, Clone, PartialEq)]
pub struct AudioPcm {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Clone, Default)]
pub struct SttOpts {
    pub model: Option<String>,
    pub language: Option<String>,
    pub provider_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Transcript {
    pub text: String,
}

#[async_trait]
pub trait SttEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError>;
}

#[derive(Debug, Clone, Default)]
pub struct TtsOpts {
    pub model: Option<String>,
    pub voice: Option<String>,
    pub format: Option<String>,
    pub provider_ref: Option<String>,
}

#[async_trait]
pub trait TtsEngine: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> EngineCaps;
    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError>;
}

#[cfg(test)]
mod tts_types_tests {
    use super::*;

    struct EchoTts;

    #[async_trait]
    impl TtsEngine for EchoTts {
        fn id(&self) -> &str {
            "echo-tts"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["echo".into()],
            }
        }

        async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
            let n = text.len().min(100);
            Ok(AudioPcm {
                samples: vec![0.1; n * 100],
                sample_rate_hz: 24_000,
            })
        }
    }

    #[tokio::test]
    async fn tts_engine_synthesize_returns_pcm() {
        let engine = EchoTts;
        let pcm = engine
            .synthesize("hello", TtsOpts::default())
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 24_000);
        assert!(!pcm.samples.is_empty());
    }
}

#[cfg(test)]
mod stt_types_tests {
    use super::*;

    #[test]
    fn audio_pcm_holds_mono_samples() {
        let pcm = AudioPcm {
            samples: vec![0.0, 0.5, -0.5],
            sample_rate_hz: 16_000,
        };
        assert_eq!(pcm.samples.len(), 3);
        assert_eq!(pcm.sample_rate_hz, 16_000);
    }

    #[test]
    fn transcript_roundtrips_json() {
        let t = Transcript {
            text: "hello world".into(),
        };
        let json = serde_json::to_string(&t).unwrap();
        let back: Transcript = serde_json::from_str(&json).unwrap();
        assert_eq!(back, t);
    }
}
