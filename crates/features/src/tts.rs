use kea_core::resolve::SlotResolver;
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::tts::TtsSettings;
use kea_engines::traits::TtsOpts;
use kea_engines::EngineRegistry;
use kea_platform::audio::AudioIo;
use kea_platform::textio::TextIo;
use kea_platform::PcmFrame;
use std::future::Future;

use crate::feature::{ActionGuard, CapKind, CapSlot, Command, Feature};

pub struct TtsFeature;

impl Feature for TtsFeature {
    fn id(&self) -> &str {
        "tts"
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot {
            name: "tts",
            kind: CapKind::Tts,
        }]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "read_selection".into(),
            title: "Read Selection Aloud".into(),
            default_accelerator: Some(default_tts_accelerator().into()),
        }]
    }
}

fn default_tts_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+T"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+T"
    }
}

pub async fn run_tts_synthesize(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    settings: &TtsSettings,
) -> Result<(i64, PcmFrame), String> {
    let text = textio
        .capture_selection()
        .await
        .map_err(|e| e.to_string())?;
    if text.trim().is_empty() {
        return Err("no selection".into());
    }

    let resolver = SlotResolver::new(engines, bindings);
    let binding = resolver
        .require_tts("tts")
        .await
        .map_err(|e| e.to_string())?;

    let action_id = actions
        .record(NewAction {
            feature_id: "tts".into(),
            command: "read_selection".into(),
            engine_id: binding.engine_id.clone(),
            model: binding
                .model
                .clone()
                .or_else(|| settings.active_model.clone()),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;

    // From here the ledger row exists. Synthesis owns it only until it hands
    // the frame back: the caller plays the audio and closes the row.
    let guard = ActionGuard::new(actions, action_id, "tts");
    match synthesize(engines, &binding, settings, &text).await {
        Ok(frame) => Ok((guard.release(), frame)),
        Err(e) => Err(guard.fail(e).await),
    }
}

async fn synthesize(
    engines: &EngineRegistry,
    binding: &Binding,
    settings: &TtsSettings,
    text: &str,
) -> Result<PcmFrame, String> {
    let engine_id = &binding.engine_id;
    let engine = engines
        .tts(engine_id)
        .ok_or_else(|| format!("no tts engine '{engine_id}'"))?;

    let tts_opts = TtsOpts {
        model: binding
            .model
            .clone()
            .or_else(|| settings.active_model.clone()),
        voice: settings.active_voice.clone(),
        format: None,
        provider_ref: binding.provider_ref.clone(),
        speed: Some(settings.speed),
    };

    let pcm = engine
        .synthesize(text, tts_opts)
        .await
        .map_err(|e| e.to_string())?;

    Ok(PcmFrame {
        samples: pcm.samples,
        sample_rate_hz: pcm.sample_rate_hz,
    })
}

/// Synthesizes the current selection, plays it with `play`, and closes the
/// ledger row that synthesis opened.
///
/// Playback is a callback because the two callers reach the speakers by
/// different routes: this crate hands the frame to [`AudioIo`], while the app
/// plays it on a blocking thread so the capture mutex behind its `AudioIo` is
/// not held for the length of the audio. The action lifecycle is the same
/// either way, so it lives here and not at the call sites.
pub async fn run_tts_with_player<F, Fut>(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    settings: &TtsSettings,
    play: F,
) -> Result<(), String>
where
    F: FnOnce(PcmFrame) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let (action_id, pcm) = run_tts_synthesize(engines, bindings, actions, textio, settings).await?;

    let guard = ActionGuard::new(actions, action_id, "tts");
    match play(pcm).await {
        Ok(()) => {
            guard.succeed().await;
            Ok(())
        }
        Err(e) => Err(guard.fail(e).await),
    }
}

pub async fn run_tts(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    audio: &dyn AudioIo,
    settings: &TtsSettings,
) -> Result<(), String> {
    run_tts_with_player(
        engines,
        bindings,
        actions,
        textio,
        settings,
        |pcm| async move { audio.play(pcm).await.map_err(|e| e.to_string()) },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::actions::ActionStatus;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::noop::NoopTtsEngine;
    use kea_platform::{AudioIoError, DictationState, ReplaceMode, TextIoError};
    use std::sync::{Arc, Mutex};

    struct FakeTextIo {
        selection: String,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }

        async fn replace_with_mode(
            &self,
            _text: &str,
            _mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            Ok(())
        }
    }

    struct FakePlayAudioIo {
        last_played: Mutex<Option<PcmFrame>>,
    }

    impl FakePlayAudioIo {
        fn last_played(&self) -> Option<PcmFrame> {
            self.last_played.lock().unwrap().clone()
        }
    }

    impl Default for FakePlayAudioIo {
        fn default() -> Self {
            Self {
                last_played: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AudioIo for FakePlayAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            Ok(PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            })
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            DictationState::Idle
        }

        async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
            *self.last_played.lock().unwrap() = Some(pcm);
            Ok(())
        }
    }

    async fn test_repos() -> (BindingRepo, ActionRepo) {
        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();
        (BindingRepo::new(config_pool), ActionRepo::new(data_pool))
    }

    #[test]
    fn tts_declares_tts_slot_and_read_selection() {
        let f = TtsFeature;
        assert_eq!(f.id(), "tts");
        assert_eq!(f.required_caps()[0].name, "tts");
        assert_eq!(f.required_caps()[0].kind, CapKind::Tts);
        let cmds = f.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "read_selection");
        assert_eq!(cmds[0].title, "Read Selection Aloud");
        assert!(cmds[0].default_accelerator.is_some());
    }

    #[tokio::test]
    async fn run_tts_plays_synthesized_audio() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));

        let fake_audio = FakePlayAudioIo::default();
        let fake_text = FakeTextIo {
            selection: "hello world".into(),
        };

        let (bindings, actions) = test_repos().await;
        let settings = TtsSettings::default();

        run_tts(
            &reg,
            &bindings,
            &actions,
            &fake_text,
            &fake_audio,
            &settings,
        )
        .await
        .unwrap();

        let played = fake_audio.last_played().expect("audio should be played");
        assert!(!played.samples.is_empty());
        assert_eq!(played.sample_rate_hz, 24_000);

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "tts");
        assert_eq!(rows[0].command, "read_selection");
        assert_eq!(rows[0].engine_id, "noop-tts");
        assert_eq!(rows[0].status, ActionStatus::Ok);
    }

    #[tokio::test]
    async fn run_tts_synthesize_returns_pcm_no_audio_needed() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));

        let fake_text = FakeTextIo {
            selection: "hello world".into(),
        };

        let (bindings, actions) = test_repos().await;
        let settings = TtsSettings::default();

        let (action_id, pcm) = run_tts_synthesize(&reg, &bindings, &actions, &fake_text, &settings)
            .await
            .unwrap();

        assert!(!pcm.samples.is_empty());
        assert_eq!(pcm.sample_rate_hz, 24_000);
        assert!(action_id > 0);
    }

    #[tokio::test]
    async fn run_tts_rejects_empty_selection() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));

        let fake_audio = FakePlayAudioIo::default();
        let fake_text = FakeTextIo {
            selection: "   ".into(),
        };

        let (bindings, actions) = test_repos().await;

        let err = run_tts(
            &reg,
            &bindings,
            &actions,
            &fake_text,
            &fake_audio,
            &TtsSettings::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "no selection");
        assert!(fake_audio.last_played().is_none());
    }
}
