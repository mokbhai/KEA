//! The OS's own recognizer as an [`SttEngine`].
//!
//! Zero bytes to download, no model to manage, no key to paste — which makes
//! it the only local engine that works on a fresh install before the user has
//! decided anything. See `kea_platform::speech` for which Apple API this
//! reaches and why it is not the newer one.
//!
//! The platform layer takes a *file*, so this engine writes the buffer to a
//! temp WAV with the same writer every hosted engine uploads through. The
//! temp file is a `NamedTempFile`, so `Drop` removes it on every path
//! including a panic — audio of somebody dictating is exactly the thing that
//! must not be left in `$TMPDIR`.

use std::sync::Arc;

use async_trait::async_trait;
use kea_platform::{SpeechError, SpeechOpts, SpeechRecognition};

use crate::stt::audio::pcm_to_wav_bytes;
use crate::traits::{
    AudioPcm, EngineCaps, EngineError, SttEngine, SttOpts, SttSegment, Transcript,
};

pub const APPLE_STT_ENGINE_ID: &str = "apple-speech";

/// The single model id this engine offers.
///
/// It has no models in the sense the other engines do — nothing is
/// downloaded and nothing is selectable — but every binding carries a model
/// string, and an engine that advertises none reads in a picker as an engine
/// with nothing installed. One honest constant beats an empty list.
pub const APPLE_STT_MODEL: &str = "on-device";

pub struct AppleSttEngine {
    speech: Arc<dyn SpeechRecognition>,
}

impl AppleSttEngine {
    pub fn new(speech: Arc<dyn SpeechRecognition>) -> Self {
        Self { speech }
    }
}

fn map_error(error: SpeechError) -> EngineError {
    match error {
        // Not `ModelNotInstalled`: there is no model to install, and the
        // "download it in Settings" advice that variant carries would send
        // the user looking for something that does not exist. What they
        // actually have to do is grant a permission.
        SpeechError::NotAuthorized => EngineError::Auth(
            "speech recognition is not allowed — grant it in System Settings › Privacy & \
             Security › Speech Recognition"
                .into(),
        ),
        SpeechError::Unavailable => {
            EngineError::Config("on-device speech recognition is not available here".into())
        }
        SpeechError::Failed(message) => EngineError::Other(message),
    }
}

#[async_trait]
impl SttEngine for AppleSttEngine {
    fn id(&self) -> &str {
        APPLE_STT_ENGINE_ID
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: vec![APPLE_STT_MODEL.into()],
        }
    }

    async fn transcribe(&self, audio: AudioPcm, opts: SttOpts) -> Result<Transcript, EngineError> {
        let wav = pcm_to_wav_bytes(&audio)?;
        let speech = self.speech.clone();
        let speech_opts = SpeechOpts {
            // The recognizer picks a model per locale, so this one *is*
            // honoured — unlike the transducer, which has no language setting
            // and drops it.
            locale: opts.language.clone(),
            // Reaches `contextualStrings`. Advisory there as everywhere else:
            // the transcript still goes through `apply_vocabulary` afterwards.
            vocabulary: opts.vocabulary.clone(),
        };

        // Recognition is a blocking call that can take seconds; doing it on a
        // runtime worker would stall every other task on that thread.
        let result = tokio::task::spawn_blocking(move || {
            let file = tempfile::Builder::new()
                .prefix("kea-speech-")
                .suffix(".wav")
                .tempfile()
                .map_err(|e| SpeechError::Failed(e.to_string()))?;
            std::fs::write(file.path(), &wav).map_err(|e| SpeechError::Failed(e.to_string()))?;
            let transcript = speech.transcribe_file(file.path(), &speech_opts);
            // Explicit rather than implicit: the file holds a recording of
            // the user, and dropping it here rather than at the end of the
            // closure makes the lifetime one line long.
            drop(file);
            transcript
        })
        .await
        .map_err(|e| EngineError::Other(format!("speech task join failed: {e}")))?
        .map_err(map_error)?;

        Ok(Transcript {
            text: result.text,
            segments: result
                .segments
                .into_iter()
                .map(|s| SttSegment {
                    start_ms: s.start_ms,
                    end_ms: s.end_ms.max(s.start_ms),
                    text: s.text,
                })
                .collect(),
        })
    }
}

/// Registers the engine — but only on a system that can actually run it.
///
/// A recognizer that can only refuse is worse than an absent one: it would
/// appear in every picker and fail at the moment of use. `supports_on_device`
/// is the question that distinguishes the two, and it is asked here, once, at
/// composition time.
pub fn register_apple_stt_engine(
    reg: &mut crate::registry::EngineRegistry,
    speech: Arc<dyn SpeechRecognition>,
) -> bool {
    if !speech.supports_on_device(None) {
        tracing::info!("apple-speech: on-device recognition unsupported; engine not registered");
        return false;
    }
    reg.register_stt(Arc::new(AppleSttEngine::new(speech)));
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_platform::{SpeechAuth, SpeechSegment, SpeechTranscript};
    use std::path::Path;
    use std::sync::Mutex;

    #[derive(Default)]
    struct FakeSpeech {
        outcome: Option<Result<SpeechTranscript, SpeechError>>,
        seen: Mutex<Vec<SpeechOpts>>,
        /// The bytes the recognizer found at the path it was handed, and
        /// whether that path still existed.
        saw_file: Mutex<Option<usize>>,
        on_device: bool,
    }

    impl SpeechRecognition for FakeSpeech {
        fn authorization(&self) -> SpeechAuth {
            SpeechAuth::Granted
        }

        fn request_authorization(&self) -> SpeechAuth {
            SpeechAuth::Granted
        }

        fn supports_on_device(&self, _locale: Option<&str>) -> bool {
            self.on_device
        }

        fn transcribe_file(
            &self,
            path: &Path,
            opts: &SpeechOpts,
        ) -> Result<SpeechTranscript, SpeechError> {
            self.seen.lock().unwrap().push(opts.clone());
            *self.saw_file.lock().unwrap() = std::fs::metadata(path).ok().map(|m| m.len() as usize);
            match &self.outcome {
                Some(Ok(transcript)) => Ok(transcript.clone()),
                Some(Err(SpeechError::NotAuthorized)) => Err(SpeechError::NotAuthorized),
                Some(Err(SpeechError::Unavailable)) => Err(SpeechError::Unavailable),
                Some(Err(SpeechError::Failed(m))) => Err(SpeechError::Failed(m.clone())),
                None => Ok(SpeechTranscript::default()),
            }
        }
    }

    fn audio() -> AudioPcm {
        AudioPcm {
            samples: vec![0.0; 16_000],
            sample_rate_hz: 16_000,
        }
    }

    #[tokio::test]
    async fn the_buffer_reaches_the_recognizer_as_a_real_wav_file() {
        let speech = Arc::new(FakeSpeech {
            outcome: Some(Ok(SpeechTranscript {
                text: "hello there".into(),
                segments: vec![SpeechSegment {
                    start_ms: 100,
                    end_ms: 400,
                    text: "hello".into(),
                }],
            })),
            on_device: true,
            ..Default::default()
        });
        let engine = AppleSttEngine::new(speech.clone());
        let out = engine
            .transcribe(audio(), SttOpts::default())
            .await
            .unwrap();
        assert_eq!(out.text, "hello there");
        assert_eq!(out.segments.len(), 1);
        assert_eq!(out.segments[0].start_ms, 100);

        // 16k mono i16 samples plus a 44-byte header. The assertion that
        // matters is that the recognizer found a non-empty file where it was
        // pointed, which is the half of this engine that can go wrong.
        let size = speech.saw_file.lock().unwrap().expect("file must exist");
        assert_eq!(size, 16_000 * 2 + 44);
    }

    /// Language and vocabulary are honoured here — unlike the transducer,
    /// which has no language setting — so they have to arrive, not stop at
    /// the engine boundary.
    #[tokio::test]
    async fn the_locale_and_vocabulary_travel() {
        let speech = Arc::new(FakeSpeech {
            on_device: true,
            ..Default::default()
        });
        let engine = AppleSttEngine::new(speech.clone());
        engine
            .transcribe(
                audio(),
                SttOpts {
                    language: Some("en-GB".into()),
                    vocabulary: vec!["KittyClaw".into()],
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let seen = speech.seen.lock().unwrap();
        assert_eq!(seen[0].locale.as_deref(), Some("en-GB"));
        assert_eq!(seen[0].vocabulary, vec!["KittyClaw".to_string()]);
    }

    /// A missing grant is a permission problem, not a missing download — the
    /// "download it in Settings" advice would send the user hunting for a
    /// model that does not exist.
    #[tokio::test]
    async fn a_missing_grant_is_an_auth_error_naming_the_settings_pane() {
        let speech = Arc::new(FakeSpeech {
            outcome: Some(Err(SpeechError::NotAuthorized)),
            on_device: true,
            ..Default::default()
        });
        let err = AppleSttEngine::new(speech)
            .transcribe(audio(), SttOpts::default())
            .await
            .unwrap_err();
        assert!(matches!(err, EngineError::Auth(_)), "{err}");
        assert!(err.to_string().contains("Speech Recognition"), "{err}");
    }

    /// A framework message — the one-minute cap, the daily limit — reaches
    /// the user verbatim, because it is the only thing that distinguishes
    /// them.
    #[tokio::test]
    async fn a_framework_failure_keeps_its_own_words() {
        let speech = Arc::new(FakeSpeech {
            outcome: Some(Err(SpeechError::Failed("Retry limit exceeded".into()))),
            on_device: true,
            ..Default::default()
        });
        let err = AppleSttEngine::new(speech)
            .transcribe(audio(), SttOpts::default())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Retry limit exceeded"), "{err}");
    }

    /// An engine that can only refuse must not appear in a picker at all.
    #[test]
    fn an_unsupported_system_registers_nothing() {
        let mut reg = crate::registry::EngineRegistry::default();
        assert!(!register_apple_stt_engine(
            &mut reg,
            Arc::new(FakeSpeech {
                on_device: false,
                ..Default::default()
            })
        ));
        assert!(reg.stt(APPLE_STT_ENGINE_ID).is_none());

        assert!(register_apple_stt_engine(
            &mut reg,
            Arc::new(FakeSpeech {
                on_device: true,
                ..Default::default()
            })
        ));
        assert!(reg.stt(APPLE_STT_ENGINE_ID).is_some());
    }
}
