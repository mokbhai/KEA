use kea_core::resolve::{Resolution, SlotResolver};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::BindingRepo;
use kea_core::tts::TtsSettings;
use kea_engines::EngineRegistry;
use kea_engines::traits::TtsOpts;
use kea_platform::audio::AudioIo;
use kea_platform::textio::TextIo;
use kea_platform::PcmFrame;

use crate::feature::{CapKind, CapSlot, Command, Feature};

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
    let engine_id = match resolver.resolve_tts("tts", "tts").await {
        Ok(Resolution::Bound(id)) => id,
        Ok(Resolution::NeedsChoice(_)) => {
            return Err("multiple tts engines available; bind the tts tts slot".into());
        }
        Ok(Resolution::Unresolvable) => return Err("no tts engine available".into()),
        Err(e) => return Err(e.to_string()),
    };

    let binding = bindings
        .get("tts", "tts")
        .await
        .map_err(|e| e.to_string())?;

    let action_id = actions
        .record(NewAction {
            feature_id: "tts".into(),
            command: "read_selection".into(),
            engine_id: engine_id.clone(),
            model: binding
                .as_ref()
                .and_then(|b| b.model.clone())
                .or_else(|| settings.active_model.clone()),
            provider_ref: binding.as_ref().and_then(|b| b.provider_ref.clone()),
        })
        .await
        .map_err(|e| e.to_string())?;

    let engine = match engines.tts(&engine_id) {
        Some(eng) => eng,
        None => {
            let msg = format!("no tts engine '{engine_id}'");
            if let Err(inner) = actions.finish(action_id, "error", Some(&msg)).await {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "tts: failed to finish action as error in DB"
                );
            }
            return Err(msg);
        }
    };

    let tts_opts = TtsOpts {
        model: binding
            .as_ref()
            .and_then(|b| b.model.clone())
            .or_else(|| settings.active_model.clone()),
        voice: settings.active_voice.clone(),
        format: None,
        provider_ref: binding.as_ref().and_then(|b| b.provider_ref.clone()),
    };

    let pcm = match engine.synthesize(&text, tts_opts).await {
        Ok(pcm) => pcm,
        Err(e) => {
            if let Err(inner) = actions
                .finish(action_id, "error", Some(&e.to_string()))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "tts: failed to finish action as error in DB"
                );
            }
            return Err(e.to_string());
        }
    };

    let frame = PcmFrame {
        samples: pcm.samples,
        sample_rate_hz: pcm.sample_rate_hz,
    };

    Ok((action_id, frame))
}

pub async fn run_tts(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    audio: &dyn AudioIo,
    settings: &TtsSettings,
) -> Result<(), String> {
    let (action_id, pcm) =
        run_tts_synthesize(engines, bindings, actions, textio, settings).await?;

    if let Err(e) = audio.play(pcm).await {
        if let Err(inner) = actions
            .finish(action_id, "error", Some(&e.to_string()))
            .await
        {
            tracing::warn!(
                error = %inner,
                action_id = %action_id,
                "tts: failed to finish action as error in DB"
            );
        }
        return Err(e.to_string());
    }

    if let Err(e) = actions.finish(action_id, "ok", None).await {
        tracing::warn!(
            error = %e,
            action_id = %action_id,
            "tts: failed to finish action as ok in DB"
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::noop::NoopTtsEngine;
    use kea_platform::{AudioIoError, DictationState, TextIoError};
    use std::sync::{Arc, Mutex};

    struct FakeTextIo {
        selection: String,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }

        async fn replace(&self, _text: &str) -> Result<(), TextIoError> {
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
        (
            BindingRepo::new(config_pool),
            ActionRepo::new(data_pool),
        )
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
        assert_eq!(rows[0].status, "ok");
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

        let (action_id, pcm) = run_tts_synthesize(
            &reg,
            &bindings,
            &actions,
            &fake_text,
            &settings,
        )
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
