use async_trait::async_trait;
use crate::traits::{
    AudioPcm, EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse, SttEngine, SttOpts,
    Transcript, TtsEngine, TtsOpts,
};

pub struct NoopLlmEngine;

#[async_trait]
impl LlmEngine for NoopLlmEngine {
    fn id(&self) -> &str { "noop" }
    fn capabilities(&self) -> EngineCaps { EngineCaps { models: vec!["echo".into()] } }
    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
        Ok(LlmResponse { text: format!("echo: {}", req.prompt) })
    }
}

pub struct NoopSttEngine;

#[async_trait]
impl SttEngine for NoopSttEngine {
    fn id(&self) -> &str {
        "noop-stt"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["noop".into()],
        }
    }

    async fn transcribe(&self, audio: AudioPcm, _opts: SttOpts) -> Result<Transcript, EngineError> {
        Ok(Transcript {
            text: format!("heard: {} samples", audio.samples.len()),
        })
    }
}

pub struct NoopTtsEngine;

#[async_trait]
impl TtsEngine for NoopTtsEngine {
    fn id(&self) -> &str {
        "noop-tts"
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec!["noop".into()],
        }
    }

    async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
        let n = text.len().max(1);
        Ok(AudioPcm {
            samples: vec![0.0; n * 100],
            sample_rate_hz: 24_000,
        })
    }
}
