//! The OS's own voices as an engine.
//!
//! The one local engine with no catalog: nothing is downloaded, and the
//! voices are whatever the user has installed through System Settings — which
//! on a Mac includes the enhanced and premium ones. It is an engine rather
//! than a special case in the read-aloud feature so it goes through the same
//! binding, the same playback path and the same picker as everything else.

use std::sync::Arc;

use async_trait::async_trait;
use kea_platform::tts::SystemTtsInference;

use crate::traits::{AudioPcm, EngineCaps, EngineError, TtsEngine, TtsOpts};

/// The engine id this registers under, shared with the UI's engine table.
pub const SYSTEM_TTS_ENGINE_ID: &str = "system-tts";

pub struct SystemTtsEngine {
    inference: Arc<dyn SystemTtsInference>,
}

impl SystemTtsEngine {
    pub fn new(inference: Arc<dyn SystemTtsInference>) -> Self {
        Self { inference }
    }
}

#[async_trait]
impl TtsEngine for SystemTtsEngine {
    fn id(&self) -> &str {
        SYSTEM_TTS_ENGINE_ID
    }

    /// The installed voices *are* the models here — there is nothing else a
    /// binding could name, and a picker with an empty model list would offer
    /// the engine with no way to choose within it.
    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: self
                .inference
                .voices()
                .into_iter()
                .map(|voice| voice.id)
                .collect(),
        }
    }

    async fn synthesize(&self, text: &str, opts: TtsOpts) -> Result<AudioPcm, EngineError> {
        if text.trim().is_empty() {
            return Err(EngineError::Config("empty text".into()));
        }

        // `voice` first, then `model`: the read-aloud settings store the
        // choice as a voice, while a binding stores it as a model, and both
        // name the same AVSpeechSynthesisVoice identifier.
        let voice = opts
            .voice
            .as_deref()
            .or(opts.model.as_deref())
            .map(str::trim)
            .filter(|v| !v.is_empty());

        let pcm = self
            .inference
            .synthesize(text, voice, opts.speed.unwrap_or(1.0))
            .await
            .map_err(|e| EngineError::Other(e.to_string()))?;

        Ok(AudioPcm {
            samples: pcm.samples,
            sample_rate_hz: pcm.sample_rate_hz,
        })
    }
}

pub fn register_system_tts_engine(
    reg: &mut crate::registry::EngineRegistry,
    inference: Arc<dyn SystemTtsInference>,
) {
    reg.register_tts(Arc::new(SystemTtsEngine::new(inference)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_platform::audio::PcmFrame;
    use kea_platform::tts::{SystemTtsError, SystemVoice};
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSystemTts {
        seen: Mutex<Vec<(String, Option<String>, f32)>>,
    }

    #[async_trait]
    impl SystemTtsInference for FakeSystemTts {
        fn voices(&self) -> Vec<SystemVoice> {
            vec![
                SystemVoice {
                    id: "com.apple.voice.compact.en-US.Samantha".into(),
                    name: "Samantha".into(),
                    language: "en-US".into(),
                    quality: "default".into(),
                },
                SystemVoice {
                    id: "com.apple.voice.premium.en-US.Ava".into(),
                    name: "Ava".into(),
                    language: "en-US".into(),
                    quality: "premium".into(),
                },
            ]
        }

        async fn synthesize(
            &self,
            text: &str,
            voice_id: Option<&str>,
            speed: f32,
        ) -> Result<PcmFrame, SystemTtsError> {
            self.seen
                .lock()
                .unwrap()
                .push((text.to_string(), voice_id.map(str::to_string), speed));
            Ok(PcmFrame {
                samples: vec![0.0; text.len() * 10],
                sample_rate_hz: 22_050,
            })
        }
    }

    #[tokio::test]
    async fn the_installed_voices_are_the_models() {
        let engine = SystemTtsEngine::new(Arc::new(FakeSystemTts::default()));
        assert_eq!(engine.id(), "system-tts");
        assert_eq!(
            engine.capabilities().models,
            vec![
                "com.apple.voice.compact.en-US.Samantha".to_string(),
                "com.apple.voice.premium.en-US.Ava".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn it_returns_pcm_for_the_playback_path_rather_than_speaking() {
        let fake = Arc::new(FakeSystemTts::default());
        let engine = SystemTtsEngine::new(fake.clone());
        let pcm = engine
            .synthesize(
                "read aloud",
                TtsOpts {
                    voice: Some("com.apple.voice.premium.en-US.Ava".into()),
                    speed: Some(1.5),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 22_050);
        assert!(!pcm.samples.is_empty());
        assert_eq!(
            fake.seen.lock().unwrap()[0],
            (
                "read aloud".to_string(),
                Some("com.apple.voice.premium.en-US.Ava".to_string()),
                1.5
            )
        );
    }

    /// A binding names the voice in `model`, the read-aloud settings name it
    /// in `voice`. Both have to reach the synthesizer, or picking a voice in
    /// one place would silently do nothing.
    #[tokio::test]
    async fn a_binding_model_names_the_voice_too() {
        let fake = Arc::new(FakeSystemTts::default());
        let engine = SystemTtsEngine::new(fake.clone());
        engine
            .synthesize(
                "hi",
                TtsOpts {
                    model: Some("com.apple.voice.compact.en-US.Samantha".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        // An empty string is "no choice", not a voice named "".
        engine
            .synthesize(
                "hi",
                TtsOpts {
                    voice: Some("  ".into()),
                    ..Default::default()
                },
            )
            .await
            .unwrap();

        let seen = fake.seen.lock().unwrap();
        assert_eq!(
            seen[0].1,
            Some("com.apple.voice.compact.en-US.Samantha".to_string())
        );
        assert_eq!(seen[1].1, None);
        assert_eq!(seen[1].2, 1.0, "no speed asked for means the natural rate");
    }

    #[tokio::test]
    async fn empty_text_is_refused_before_the_synthesizer_is_started() {
        let fake = Arc::new(FakeSystemTts::default());
        let engine = SystemTtsEngine::new(fake.clone());
        let err = engine
            .synthesize("   ", TtsOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("empty text"));
        assert!(fake.seen.lock().unwrap().is_empty());
    }
}
