use kea_core::dictation::DictationSettings;
use kea_core::resolve::{Resolution, SlotResolver};
use kea_core::rewrite::{RewriteInput, build_llm_request};
use kea_core::rewrite::{PresetRepo, PromptOverrideRepo, RewriteMode};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::BindingRepo;
use kea_engines::EngineRegistry;
use kea_engines::traits::{AudioPcm, SttOpts};
use kea_platform::TextIo;
use kea_platform::audio::util::resample_linear;
use kea_platform::{AudioIo, PcmFrame};

use crate::feature::{CapKind, CapSlot, Command, Feature};
use crate::rewrite::{ContentStorageOpts, maybe_record_conversation};

const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

pub struct DictationFeature;

impl Feature for DictationFeature {
    fn id(&self) -> &str {
        "dictation"
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot {
            name: "stt",
            kind: CapKind::Stt,
        }]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "push_to_talk".into(),
            title: "Push to Talk".into(),
            default_accelerator: Some(default_dictation_accelerator().into()),
        }]
    }
}

fn default_dictation_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+D"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+D"
    }
}

fn pcm_to_audio(pcm: PcmFrame) -> AudioPcm {
    let frame = if pcm.sample_rate_hz != WHISPER_SAMPLE_RATE_HZ {
        resample_linear(&pcm, WHISPER_SAMPLE_RATE_HZ)
    } else {
        pcm
    };
    AudioPcm {
        samples: frame.samples,
        sample_rate_hz: frame.sample_rate_hz,
    }
}

pub async fn run_dictation(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    audio: &mut dyn AudioIo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
) -> Result<String, String> {
    run_dictation_with_storage(
        engines,
        bindings,
        actions,
        presets,
        overrides,
        audio,
        textio,
        settings,
        ContentStorageOpts::default(),
    )
    .await
}

pub async fn run_dictation_with_storage(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    audio: &mut dyn AudioIo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
    storage: ContentStorageOpts<'_>,
) -> Result<String, String> {
    let _frame_rx = audio
        .start_mic()
        .await
        .map_err(|e| e.to_string())?;
    let pcm = audio.stop_mic().await.map_err(|e| e.to_string())?;

    let resolver = SlotResolver::new(engines, bindings);
    let binding = match resolver.resolve_stt("dictation", "stt").await {
        Ok(Resolution::Bound(b)) => b,
        Ok(Resolution::NeedsChoice(_)) => {
            return Err("multiple stt engines available; bind the dictation stt slot".into());
        }
        Ok(Resolution::Unresolvable) => return Err("no stt engine available".into()),
        Err(e) => return Err(e.to_string()),
    };
    let engine_id = binding.engine_id.clone();

    let action_id = actions
        .record(NewAction {
            feature_id: "dictation".into(),
            command: "push_to_talk".into(),
            engine_id: engine_id.clone(),
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;

    let engine = match engines.stt(&engine_id) {
        Some(eng) => eng,
        None => {
            let msg = format!("no stt engine '{engine_id}'");
            if let Err(inner) = actions.finish(action_id, "error", Some(&msg)).await {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "dictation: failed to finish action as error in DB"
                );
            }
            return Err(msg);
        }
    };

    let stt_opts = SttOpts {
        model: binding
            .model
            .clone()
            .or_else(|| settings.active_model.clone()),
        language: None,
        provider_ref: binding.provider_ref.clone(),
    };

    let transcript = match engine.transcribe(pcm_to_audio(pcm), stt_opts).await {
        Ok(t) => t,
        Err(e) => {
            if let Err(inner) = actions
                .finish(action_id, "error", Some(&e.to_string()))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %action_id,
                    "dictation: failed to finish action as error in DB"
                );
            }
            return Err(e.to_string());
        }
    };

    let mut final_text = transcript.text;

    if settings.post_process {
        let transcript_text = final_text.clone();
        let llm_binding = match resolver.resolve_llm("rewrite", "llm").await {
            Ok(Resolution::Bound(b)) => b,
            Ok(Resolution::NeedsChoice(_)) => {
                let msg = "multiple llm engines available; bind the rewrite llm slot";
                if let Err(inner) = actions
                    .finish(action_id, "error", Some(msg))
                    .await
                {
                    tracing::warn!(
                        error = %inner,
                        action_id = %action_id,
                        "dictation: failed to finish action as error in DB"
                    );
                }
                return Err("multiple llm engines available; bind the rewrite llm slot for audio refinement"
                    .into());
            }
            Ok(Resolution::Unresolvable) => {
                let msg = "no llm engine available";
                if let Err(inner) = actions
                    .finish(action_id, "error", Some(msg))
                    .await
                {
                    tracing::warn!(
                        error = %inner,
                        action_id = %action_id,
                        "dictation: failed to finish action as error in DB"
                    );
                }
                return Err("no llm engine available for audio refinement".into());
            }
            Err(e) => {
                if let Err(inner) = actions.finish(action_id, "error", Some(&e.to_string())).await {
                    tracing::warn!(
                        error = %inner,
                        action_id = %action_id,
                        "dictation: failed to finish action as error in DB"
                    );
                }
                return Err(e.to_string());
            }
        };
        let llm_engine_id = llm_binding.engine_id.clone();

        let mut llm_req = match build_llm_request(
            &RewriteInput {
                source_text: final_text.clone(),
                mode: RewriteMode::AudioRefinement,
                preset_id: None,
                custom_instruction: None,
            },
            presets,
            overrides,
        )
        .await
        {
            Ok(req) => req,
            Err(e) => {
                if let Err(inner) = actions.finish(action_id, "error", Some(&e.to_string())).await {
                    tracing::warn!(
                        error = %inner,
                        action_id = %action_id,
                        "dictation: failed to finish action as error in DB"
                    );
                }
                return Err(e.to_string());
            }
        };

        llm_req.model = llm_binding.model.clone();

        let llm = match engines.llm(&llm_engine_id) {
            Some(eng) => eng,
            None => {
                let msg = format!("no llm engine '{llm_engine_id}'");
                if let Err(inner) = actions.finish(action_id, "error", Some(&msg)).await {
                    tracing::warn!(
                        error = %inner,
                        action_id = %action_id,
                        "dictation: failed to finish action as error in DB"
                    );
                }
                return Err(msg);
            }
        };

        final_text = match llm.complete(llm_req).await {
            Ok(resp) => {
                if let Err(e) = maybe_record_conversation(
                    storage,
                    action_id,
                    "dictation",
                    &llm_engine_id,
                    llm_binding.model.clone(),
                    llm_binding.provider_ref.clone(),
                    &transcript_text,
                    &resp.text,
                )
                .await
                {
                    if let Err(inner) = actions
                        .finish(action_id, "error", Some(&e))
                        .await
                    {
                        tracing::warn!(
                            error = %inner,
                            action_id = %action_id,
                            "dictation: failed to finish action as error in DB"
                        );
                    }
                    return Err(e);
                }
                resp.text
            }
            Err(e) => {
                if let Err(inner) = actions
                    .finish(action_id, "error", Some(&e.to_string()))
                    .await
                {
                    tracing::warn!(
                        error = %inner,
                        action_id = %action_id,
                        "dictation: failed to finish action as error in DB"
                    );
                }
                return Err(e.to_string());
            }
        };
    }

    if let Err(e) = textio.insert_at_cursor(&final_text).await {
        if let Err(inner) = actions.finish(action_id, "error", Some(&e.to_string())).await {
            tracing::warn!(
                error = %inner,
                action_id = %action_id,
                "dictation: failed to finish action as error in DB"
            );
        }
        return Err(e.to_string());
    }

    if let Err(e) = actions.finish(action_id, "ok", None).await {
        tracing::warn!(
            error = %e,
            action_id = %action_id,
            "dictation: failed to finish action as ok in DB"
        );
    }

    Ok(final_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::conversations::ConversationRepo;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::noop::NoopLlmEngine;
    use kea_engines::traits::{EngineCaps, EngineError, SttEngine, Transcript};
    use kea_platform::{AudioIoError, DictationState, TextIoError};
    use std::sync::{Arc, Mutex};

    struct FakeStt {
        text: String,
    }

    #[async_trait]
    impl SttEngine for FakeStt {
        fn id(&self) -> &str {
            "fake-stt"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["fake".into()],
            }
        }

        async fn transcribe(
            &self,
            _audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            Ok(Transcript {
                text: self.text.clone(),
            })
        }
    }

    struct FakeAudioIo {
        state: DictationState,
        buffered: PcmFrame,
    }

    impl FakeAudioIo {
        fn with_pcm(buffered: PcmFrame) -> Self {
            Self {
                state: DictationState::Idle,
                buffered,
            }
        }
    }

    #[async_trait]
    impl AudioIo for FakeAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.state = DictationState::Listening;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.state = DictationState::Idle;
            Ok(self.buffered.clone())
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            self.state
        }
    }

    struct FakeTextIo {
        inserted: Mutex<Option<String>>,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(String::new())
        }

        async fn replace(&self, _text: &str) -> Result<(), TextIoError> {
            Ok(())
        }

        async fn insert_at_cursor(&self, text: &str) -> Result<(), TextIoError> {
            *self.inserted.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    async fn test_repos() -> (
        BindingRepo,
        ActionRepo,
        PresetRepo,
        PromptOverrideRepo,
    ) {
        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        (
            BindingRepo::new(config_pool.clone()),
            ActionRepo::new(data_pool),
            PresetRepo::new(config_pool.clone()),
            PromptOverrideRepo::new(config_pool),
        )
    }

    #[test]
    fn dictation_declares_stt_slot_and_push_to_talk() {
        let f = DictationFeature;
        assert_eq!(f.id(), "dictation");
        assert_eq!(f.required_caps()[0].name, "stt");
        assert_eq!(f.required_caps()[0].kind, CapKind::Stt);
        assert_eq!(f.commands()[0].id, "push_to_talk");
    }

    #[tokio::test]
    async fn run_dictation_transcribes_and_inserts() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "hello world".into(),
        }));

        let textio = Arc::new(FakeTextIo {
            inserted: Mutex::new(None),
        });

        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
        };

        let mut audio = FakeAudioIo::with_pcm(PcmFrame {
            samples: vec![0.0; 1600],
            sample_rate_hz: 16_000,
        });

        let out = run_dictation(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &settings,
        )
        .await
        .unwrap();

        assert_eq!(out, "hello world");
        assert_eq!(
            textio.inserted.lock().unwrap().as_deref(),
            Some("hello world")
        );

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "dictation");
        assert_eq!(rows[0].command, "push_to_talk");
        assert_eq!(rows[0].engine_id, "fake-stt");
        assert_eq!(rows[0].status, "ok");
    }

    #[tokio::test]
    async fn post_process_calls_llm_audio_refinement() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "um hello".into(),
        }));
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            inserted: Mutex::new(None),
        });

        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: true,
            active_model: None,
        };

        let mut audio = FakeAudioIo::with_pcm(PcmFrame {
            samples: vec![0.0; 800],
            sample_rate_hz: 16_000,
        });

        let out = run_dictation(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &settings,
        )
        .await
        .unwrap();

        assert!(out.contains("echo:"));
        assert!(out.contains("transcribed speech"));
        assert!(out.contains("um hello"));
        assert_eq!(textio.inserted.lock().unwrap().as_deref(), Some(out.as_str()));

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows[0].feature_id, "dictation");
        assert_eq!(rows[0].status, "ok");
    }

    #[tokio::test]
    async fn post_process_records_conversation_when_storage_enabled() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "um hello".into(),
        }));
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            inserted: Mutex::new(None),
        });

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        let actions = ActionRepo::new(data_pool.clone());
        let conversations = ConversationRepo::new(data_pool.clone());
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);

        let settings = DictationSettings {
            post_process: true,
            active_model: None,
        };

        let mut audio = FakeAudioIo::with_pcm(PcmFrame {
            samples: vec![0.0; 800],
            sample_rate_hz: 16_000,
        });

        run_dictation_with_storage(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &settings,
            ContentStorageOpts::enabled(&conversations),
        )
        .await
        .unwrap();

        // Also seed a rewrite conversation so History distinguishes by feature_id.
        let rewrite_conversations = ConversationRepo::new(data_pool.clone());
        let rewrite_conv_id = rewrite_conversations
            .start(&kea_core::store::conversations::NewConversation {
                action_id: None,
                feature_id: "rewrite".into(),
                engine_id: "openai".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();
        rewrite_conversations
            .append_message(&kea_core::store::conversations::NewMessage {
                conversation_id: rewrite_conv_id,
                role: "user".into(),
                content: "hello".into(),
                token_count: None,
            })
            .await
            .unwrap();
        rewrite_conversations
            .append_message(&kea_core::store::conversations::NewMessage {
                conversation_id: rewrite_conv_id,
                role: "assistant".into(),
                content: "hi there".into(),
                token_count: None,
            })
            .await
            .unwrap();

        let recent = conversations.list_recent(10).await.unwrap();
        assert_eq!(recent.len(), 2, "both dictation and rewrite conversations are listed");
        let features: Vec<&str> = recent.iter().map(|r| r.feature_id.as_str()).collect();
        assert!(features.contains(&"dictation"), "dictation conversation present");
        assert!(features.contains(&"rewrite"), "rewrite conversation present");
        let dictation_row = recent.iter().find(|r| r.feature_id == "dictation").unwrap();
        assert_eq!(dictation_row.engine_id, "noop");

        let messages = conversations.list_messages(dictation_row.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "user");
        assert_eq!(messages[0].content, "um hello");
        assert_eq!(messages[1].role, "assistant");
    }

    #[tokio::test]
    async fn post_process_skips_conversation_when_storage_disabled() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "um hello".into(),
        }));
        reg.register_llm(Arc::new(NoopLlmEngine));

        let textio = Arc::new(FakeTextIo {
            inserted: Mutex::new(None),
        });

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool.clone());
        let actions = ActionRepo::new(data_pool.clone());
        let conversations = ConversationRepo::new(data_pool);
        let presets = PresetRepo::new(config_pool.clone());
        let overrides = PromptOverrideRepo::new(config_pool);

        let settings = DictationSettings {
            post_process: true,
            active_model: None,
        };

        let mut audio = FakeAudioIo::with_pcm(PcmFrame {
            samples: vec![0.0; 800],
            sample_rate_hz: 16_000,
        });

        run_dictation_with_storage(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &settings,
            ContentStorageOpts::disabled(),
        )
        .await
        .unwrap();

        assert!(conversations.list_recent(1).await.unwrap().is_empty());
    }
}
