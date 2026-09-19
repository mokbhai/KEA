use kea_core::dictation::{apply_vocabulary, hint_terms, DictationSettings};
use kea_core::resolve::SlotResolver;
use kea_core::rewrite::{build_llm_request, RewriteInput};
use kea_core::rewrite::{PresetRepo, PromptOverrideRepo, RewriteMode};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::vocabulary::VocabularyEntry;
use kea_engines::traits::{AudioPcm, SttOpts};
use kea_engines::EngineRegistry;
use kea_platform::audio::util::resample_linear;
use kea_platform::TextIo;
use kea_platform::{AudioIo, PcmFrame};

use crate::feature::{ActionGuard, CapKind, CapSlot, Command, Feature};
use crate::rewrite::{maybe_record_conversation, ContentStorageOpts};

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

#[allow(clippy::too_many_arguments)]
pub async fn run_dictation(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    audio: &mut dyn AudioIo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
    vocabulary: &[VocabularyEntry],
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
        vocabulary,
        ContentStorageOpts::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_dictation_with_storage(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    audio: &mut dyn AudioIo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
    vocabulary: &[VocabularyEntry],
    storage: ContentStorageOpts<'_>,
) -> Result<String, String> {
    let _frame_rx = audio.start_mic().await.map_err(|e| e.to_string())?;
    let pcm = audio.stop_mic().await.map_err(|e| e.to_string())?;

    let resolver = SlotResolver::new(engines, bindings);
    let binding = resolver
        .require_stt("dictation")
        .await
        .map_err(|e| e.to_string())?;

    let action_id = actions
        .record(NewAction {
            feature_id: "dictation".into(),
            command: "push_to_talk".into(),
            engine_id: binding.engine_id.clone(),
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;

    // From here the ledger row exists, so every exit closes it.
    let guard = ActionGuard::new(actions, action_id, "dictation");
    let result = run_dictation_inner(
        engines, &resolver, presets, overrides, textio, settings, vocabulary, storage, &binding,
        action_id, pcm,
    )
    .await;
    match result {
        Ok(text) => {
            guard.succeed().await;
            Ok(text)
        }
        Err(e) => Err(guard.fail(e).await),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_dictation_inner(
    engines: &EngineRegistry,
    resolver: &SlotResolver<'_>,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
    vocabulary: &[VocabularyEntry],
    storage: ContentStorageOpts<'_>,
    binding: &Binding,
    action_id: i64,
    pcm: PcmFrame,
) -> Result<String, String> {
    let engine_id = &binding.engine_id;
    let engine = engines
        .stt(engine_id)
        .ok_or_else(|| format!("no stt engine '{engine_id}'"))?;

    let stt_opts = SttOpts {
        model: binding
            .model
            .clone()
            .or_else(|| settings.active_model.clone()),
        language: None,
        provider_ref: binding.provider_ref.clone(),
        vocabulary: hint_terms(vocabulary),
    };

    let transcript = engine
        .transcribe(pcm_to_audio(pcm), stt_opts)
        .await
        .map_err(|e| e.to_string())?;

    // Before the refinement pass so the LLM sees correct proper nouns, and
    // before `transcript_text` is snapshotted below so History shows what was
    // actually inserted rather than what the decoder first guessed.
    let mut final_text = apply_vocabulary(&transcript.text, vocabulary);
    tracing::info!(
        action_id = %action_id,
        engine = %engine_id,
        chars = final_text.chars().count(),
        post_process = settings.post_process,
        "dictation: transcribed"
    );

    if settings.post_process {
        let transcript_text = final_text.clone();
        // Dictation borrows rewrite's slot, so its failures name what the
        // binding was wanted for.
        let llm_binding = resolver
            .require_llm("rewrite")
            .await
            .map_err(|e| e.with_purpose("audio refinement"))?;
        let llm_engine_id = llm_binding.engine_id.clone();

        let mut llm_req = build_llm_request(
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
        .map_err(|e| e.to_string())?;

        llm_req.model = llm_binding.model.clone();
        // Same reason as run_rewrite: the provider lives on the binding, not on
        // the shared engine instance.
        llm_req.provider_ref = llm_binding.provider_ref.clone();

        let llm = engines
            .llm(&llm_engine_id)
            .ok_or_else(|| format!("no llm engine '{llm_engine_id}'"))?;

        let resp = llm.complete(llm_req).await.map_err(|e| e.to_string())?;

        maybe_record_conversation(
            storage,
            action_id,
            "dictation",
            &llm_engine_id,
            llm_binding.model.clone(),
            llm_binding.provider_ref.clone(),
            &transcript_text,
            &resp.text,
        )
        .await?;

        tracing::info!(
            action_id = %action_id,
            engine = %llm_engine_id,
            chars = resp.text.chars().count(),
            "dictation: post-processed"
        );
        final_text = resp.text;
    }

    // The paste is the one step whose outcome the OS will not report (see
    // kea_platform::textio::macos), so both edges of it are logged: a run that
    // reaches "inserting" and never reaches "inserted" is the signature of a
    // synthetic keystroke that went nowhere.
    tracing::info!(
        action_id = %action_id,
        chars = final_text.chars().count(),
        "dictation: inserting into the focused app"
    );
    let insert_started = std::time::Instant::now();
    if let Err(e) = textio.insert_at_cursor(&final_text).await {
        tracing::error!(
            action_id = %action_id,
            error = %e,
            elapsed_ms = insert_started.elapsed().as_millis() as u64,
            "dictation: insertion failed"
        );
        return Err(e.to_string());
    }

    tracing::info!(
        action_id = %action_id,
        elapsed_ms = insert_started.elapsed().as_millis() as u64,
        "dictation: inserted"
    );

    Ok(final_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::actions::ActionStatus;
    use kea_core::store::conversations::{ConversationRepo, MessageRole};
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_core::store::vocabulary::VocabularyEntry;
    use kea_engines::noop::NoopLlmEngine;
    use kea_engines::traits::{EngineCaps, EngineError, SttEngine, Transcript};
    use kea_platform::{AudioIoError, DictationState, ReplaceMode, TextIoError};
    use std::sync::{Arc, Mutex};

    struct FakeStt {
        text: String,
    }

    /// Like [`FakeStt`], but keeps the options it was called with so a test can
    /// assert what actually reached the engine rather than only what came back.
    struct RecordingStt {
        text: String,
        seen: Arc<Mutex<Option<SttOpts>>>,
    }

    #[async_trait]
    impl SttEngine for RecordingStt {
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
            opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            *self.seen.lock().unwrap() = Some(opts);
            Ok(Transcript {
                text: self.text.clone(),
            })
        }
    }

    fn vocab(term: &str, sounds_like: Option<&str>, enabled: bool) -> VocabularyEntry {
        VocabularyEntry {
            id: format!("v-{term}"),
            term: term.into(),
            sounds_like: sounds_like.map(str::to_string),
            enabled,
            created_at: "2026-09-19T00:00:00Z".into(),
        }
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

        async fn replace_with_mode(
            &self,
            _text: &str,
            _mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            Ok(())
        }

        async fn insert_at_cursor(&self, text: &str) -> Result<(), TextIoError> {
            *self.inserted.lock().unwrap() = Some(text.to_string());
            Ok(())
        }
    }

    async fn test_repos() -> (BindingRepo, ActionRepo, PresetRepo, PromptOverrideRepo) {
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

    /// The end-to-end shape of the feature: the engine mishears, and what the
    /// user's text field receives is nonetheless the stored spelling.
    #[tokio::test]
    async fn vocabulary_is_applied_before_the_text_is_inserted() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "i pushed it to kitty claw today".into(),
        }));

        let textio = Arc::new(FakeTextIo {
            inserted: Mutex::new(None),
        });

        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
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
            &[vocab("KittyClaw", Some("kitty claw"), true)],
        )
        .await
        .unwrap();

        assert_eq!(out, "i pushed it to KittyClaw today");
        assert_eq!(
            textio.inserted.lock().unwrap().as_deref(),
            Some("i pushed it to KittyClaw today"),
            "the corrected text is what reaches the app, not the raw transcript"
        );
    }

    /// The hint layer. Only canonical spellings go to the engine, and only from
    /// enabled entries — biasing a decoder toward a term the user switched off
    /// would reintroduce the spelling they disabled.
    #[tokio::test]
    async fn only_enabled_terms_reach_the_engine_as_hints() {
        let seen = Arc::new(Mutex::new(None));
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(RecordingStt {
            text: "anything".into(),
            seen: seen.clone(),
        }));

        let textio = Arc::new(FakeTextIo {
            inserted: Mutex::new(None),
        });
        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
        };
        let mut audio = FakeAudioIo::with_pcm(PcmFrame {
            samples: vec![0.0; 1600],
            sample_rate_hz: 16_000,
        });

        run_dictation(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &settings,
            &[
                vocab("KittyClaw", Some("kitty claw"), true),
                vocab("Disabled", None, false),
            ],
        )
        .await
        .unwrap();

        let opts = seen.lock().unwrap().clone().expect("engine was called");
        assert_eq!(
            opts.vocabulary,
            vec!["KittyClaw".to_string()],
            "sounds_like values are the wrong spellings and must not be hinted"
        );
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
            hold_to_talk: false,
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
            &[],
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
        assert_eq!(rows[0].status, ActionStatus::Ok);
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
            hold_to_talk: false,
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
            &[],
        )
        .await
        .unwrap();

        assert!(out.contains("echo:"));
        assert!(out.contains("transcribed speech"));
        assert!(out.contains("um hello"));
        assert_eq!(
            textio.inserted.lock().unwrap().as_deref(),
            Some(out.as_str())
        );

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows[0].feature_id, "dictation");
        assert_eq!(rows[0].status, ActionStatus::Ok);
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
            hold_to_talk: false,
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
            &[],
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
                role: MessageRole::User,
                content: "hello".into(),
                token_count: None,
            })
            .await
            .unwrap();
        rewrite_conversations
            .append_message(&kea_core::store::conversations::NewMessage {
                conversation_id: rewrite_conv_id,
                role: MessageRole::Assistant,
                content: "hi there".into(),
                token_count: None,
            })
            .await
            .unwrap();

        let recent = conversations.list_recent(10).await.unwrap();
        assert_eq!(
            recent.len(),
            2,
            "both dictation and rewrite conversations are listed"
        );
        let features: Vec<&str> = recent.iter().map(|r| r.feature_id.as_str()).collect();
        assert!(
            features.contains(&"dictation"),
            "dictation conversation present"
        );
        assert!(
            features.contains(&"rewrite"),
            "rewrite conversation present"
        );
        let dictation_row = recent.iter().find(|r| r.feature_id == "dictation").unwrap();
        assert_eq!(dictation_row.engine_id, "noop");

        let messages = conversations.list_messages(dictation_row.id).await.unwrap();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, MessageRole::User);
        assert_eq!(messages[0].content, "um hello");
        assert_eq!(messages[1].role, MessageRole::Assistant);
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
            hold_to_talk: false,
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
            &[],
            ContentStorageOpts::disabled(),
        )
        .await
        .unwrap();

        assert!(conversations.list_recent(1).await.unwrap().is_empty());
    }
}
