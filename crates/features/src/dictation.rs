use kea_core::dictation::{
    apply_vocabulary, apply_voice_commands, hint_terms, DictationSettings, VoiceCommandConfig,
    VoiceCommandSettings,
};
use kea_core::resolve::SlotResolver;
use kea_core::rewrite::{build_llm_request, RewriteInput};
use kea_core::rewrite::{PresetRepo, PromptOverrideRepo, RewriteMode};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::vocabulary::VocabularyEntry;
use kea_engines::traits::{AudioPcm, Partial, SttOpts, SttStream, Transcript};
use kea_engines::EngineRegistry;
use kea_platform::audio::util::resample_linear;
use kea_platform::TextIo;
use kea_platform::{AudioIo, PcmFrame};

use crate::feature::{ActionGuard, CapKind, CapSlot, Command, Feature, ProfileOverrides};
use crate::rewrite::{maybe_record_conversation, ContentStorageOpts};

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;

/// How many partials may wait for the app layer to pick them up.
///
/// Small and dropping on full, for the same reason the decoder's own queue is:
/// a superseded hypothesis has no value, and a queue that grows makes the HUD
/// lag real time — which reads as a hang, the one thing live partials exist to
/// avoid.
const PARTIAL_CHANNEL_DEPTH: usize = 16;

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
    profile: &ProfileOverrides,
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
        profile,
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
    profile: &ProfileOverrides,
    storage: ContentStorageOpts<'_>,
) -> Result<String, String> {
    run_dictation_with_opts(
        engines,
        bindings,
        actions,
        presets,
        overrides,
        audio,
        textio,
        settings,
        vocabulary,
        profile,
        DictationRunOpts {
            storage,
            ..Default::default()
        },
    )
    .await
}

/// What a run needs beyond its engines and settings.
///
/// The streaming fields are here rather than on `DictationSettings` because
/// neither is a setting: the draft is produced by *this* run, and whether to
/// fall back to it is a decision the app layer makes from a setting it has
/// already read.
#[derive(Default)]
pub struct DictationRunOpts<'a> {
    pub storage: ContentStorageOpts<'a>,
    /// The last live hypothesis from the streaming pass, if there was one.
    ///
    /// Never inserted on the happy path — the offline engine's transcript is —
    /// which is the whole architecture of this feature. See `draft_fallback`.
    pub streaming_draft: Option<String>,
    /// Insert `streaming_draft` when the offline decode fails, instead of
    /// losing the audio entirely.
    ///
    /// Default **off**, and that is the honest default: inserting knowably
    /// worse text after a failure the user cannot see is exactly the
    /// regression the two-pass design exists to prevent. Off, the current
    /// behaviour is kept and the log says a draft was available.
    pub draft_fallback: bool,
}

#[allow(clippy::too_many_arguments)]
pub async fn run_dictation_with_opts(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    audio: &mut dyn AudioIo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
    vocabulary: &[VocabularyEntry],
    profile: &ProfileOverrides,
    opts: DictationRunOpts<'_>,
) -> Result<String, String> {
    run_dictation_with_commands(
        engines,
        bindings,
        actions,
        presets,
        overrides,
        audio,
        textio,
        settings,
        vocabulary,
        profile,
        opts,
        &VoiceCommandSettings::default(),
    )
    .await
}

/// The full entry point: everything [`run_dictation_with_opts`] takes, plus
/// the voice-command pass's two settings.
///
/// A separate argument rather than a field on [`DictationRunOpts`] only
/// because the app layer builds that struct as an exhaustive literal, in a
/// crate this one must not edit. Fold it in and delete this wrapper the moment
/// `src-tauri` moves over.
///
/// It takes the raw `VoiceCommandSettings` rather than a resolved
/// [`VoiceCommandConfig`] on purpose: the language gate needs the model the
/// run actually bound, and that is not known until the slot has resolved, in
/// here. Leaving the gate to the caller would give every caller a chance to
/// get it wrong.
#[allow(clippy::too_many_arguments)]
pub async fn run_dictation_with_commands(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    presets: &PresetRepo,
    overrides: &PromptOverrideRepo,
    audio: &mut dyn AudioIo,
    textio: &dyn TextIo,
    settings: &DictationSettings,
    vocabulary: &[VocabularyEntry],
    profile: &ProfileOverrides,
    opts: DictationRunOpts<'_>,
    voice_commands: &VoiceCommandSettings,
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
        engines,
        &resolver,
        presets,
        overrides,
        textio,
        settings,
        vocabulary,
        profile,
        opts,
        voice_commands,
        &binding,
        action_id,
        pcm,
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
    profile: &ProfileOverrides,
    opts: DictationRunOpts<'_>,
    voice_commands: &VoiceCommandSettings,
    binding: &Binding,
    action_id: i64,
    pcm: PcmFrame,
) -> Result<String, String> {
    let DictationRunOpts {
        storage,
        streaming_draft,
        draft_fallback,
    } = opts;
    let engine_id = &binding.engine_id;
    let engine = engines
        .stt(engine_id)
        .ok_or_else(|| format!("no stt engine '{engine_id}'"))?;

    let stt_opts = SttOpts {
        model: binding
            .model
            .clone()
            .or_else(|| settings.active_model.clone()),
        // Whisper honours this; the ONNX transducer has no language setting and
        // the engine drops the field rather than pretending to use it.
        language: settings.language.clone(),
        provider_ref: binding.provider_ref.clone(),
        vocabulary: hint_terms(vocabulary),
    };

    // Resolved here because the gate reads the model the slot actually bound,
    // which the caller does not know. An un-set language plus a multilingual
    // model resolves to off: the command table is English-only, and matching
    // English phrases against a transcript the user asked to be decoded as
    // German would corrupt it silently.
    let commands = VoiceCommandConfig::resolve(
        voice_commands,
        settings.language.as_deref(),
        stt_opts.model.as_deref(),
    );

    // The second pass, and the only one whose output is ever inserted. Any
    // live partials the user watched came from a different decoder and are
    // discarded here.
    let transcript = match engine.transcribe(pcm_to_audio(pcm), stt_opts).await {
        Ok(transcript) => transcript,
        Err(e) => {
            let message = e.to_string();
            // A failed decode loses the audio entirely and inserts nothing,
            // which is why the draft is worth mentioning even when it is not
            // used: the log is the only place the user's words still exist.
            match streaming_draft.filter(|draft| !draft.trim().is_empty()) {
                Some(draft) if draft_fallback => {
                    tracing::warn!(
                        action_id = %action_id,
                        error = %message,
                        "dictation: the offline decode failed; inserting the live draft instead"
                    );
                    Transcript::text_only(draft)
                }
                Some(draft) => {
                    tracing::warn!(
                        action_id = %action_id,
                        error = %message,
                        draft_chars = draft.chars().count(),
                        "dictation: the offline decode failed; a live draft was available but \
                         the draft fallback is off"
                    );
                    return Err(message);
                }
                None => return Err(message),
            }
        }
    };

    // Three passes over the transcript, and the order is the part that is easy
    // to get wrong on a later edit:
    //
    // 1. Voice commands first, because the vocabulary pass rewrites tokens and
    //    could manufacture or destroy a command phrase. A vocabulary entry
    //    whose `sounds_like` is "period" is perfectly legal and would
    //    otherwise eat every full stop in the transcript.
    // 2. Vocabulary second, because the punctuation the command pass inserts
    //    changes word boundaries — and `apply_vocabulary`'s rule 4 is written
    //    to match phrases across punctuation, so it is designed for punctuated
    //    input.
    // 3. The LLM refinement last, now seeing both correct proper nouns and
    //    correct structure.
    //
    // Both of the first two are pure and infallible, so nothing here can leave
    // the ledger row open; the `ActionGuard` in the caller stays the only
    // thing that closes it.
    let spoken = apply_voice_commands(&transcript.text, &commands);
    if !spoken.applied.is_empty() {
        tracing::info!(
            action_id = %action_id,
            commands = spoken.applied.len(),
            ids = %spoken
                .applied
                .iter()
                .map(|a| a.id.as_str())
                .collect::<Vec<_>>()
                .join(","),
            "dictation: voice commands applied"
        );
    }

    // Before the refinement pass so the LLM sees correct proper nouns, and
    // before `transcript_text` is snapshotted below so History shows what was
    // actually inserted rather than what the decoder first guessed.
    let mut final_text = apply_vocabulary(&spoken.text, vocabulary);
    tracing::info!(
        action_id = %action_id,
        engine = %engine_id,
        chars = final_text.chars().count(),
        post_process = settings.post_process,
        "dictation: transcribed"
    );

    // Tri-state: a profile may force the cleanup pass off for one app (a shell
    // prompt, a code editor) without changing the global setting.
    if profile.post_process_or(settings.post_process) {
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
    // `insert_at_cursor` hardcodes ClipboardPaste; going through
    // `replace_with_mode` is what lets a profile pick Accessibility insertion
    // for an app where the clipboard round-trip is disruptive.
    if let Err(e) = textio
        .replace_with_mode(&final_text, profile.replace_mode())
        .await
    {
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

/// A running streaming pass: the task feeding capture frames to a recognizer
/// and forwarding its hypotheses.
///
/// Owns no ledger row and closes no action. It is display-only, so it must
/// never be able to fail a dictation run — every exit here is a log line.
pub struct PartialsSession {
    cancel: tokio::sync::watch::Sender<bool>,
    join: tokio::task::JoinHandle<Option<String>>,
    dropped: Arc<AtomicU64>,
}

/// Starts the streaming pass over `frames`, returning the hypotheses.
///
/// A `Receiver<Partial>` rather than a callback because that is how the
/// meeting path already inverts this dependency: `crates/features` builds the
/// values and the app layer emits them. The features crate must not know about
/// `AppHandle`.
pub fn spawn_partials(
    stream: Box<dyn SttStream>,
    mut frames: tokio::sync::mpsc::Receiver<PcmFrame>,
) -> (PartialsSession, tokio::sync::mpsc::Receiver<Partial>) {
    let (partial_tx, partial_rx) = tokio::sync::mpsc::channel(PARTIAL_CHANNEL_DEPTH);
    let (cancel, mut cancel_rx) = tokio::sync::watch::channel(false);
    let dropped = Arc::new(AtomicU64::new(0));
    let task_dropped = dropped.clone();

    let join = tokio::spawn(async move {
        let mut stream = stream;
        loop {
            tokio::select! {
                changed = cancel_rx.changed() => {
                    if changed.is_err() || *cancel_rx.borrow() {
                        break;
                    }
                }
                frame = frames.recv() => {
                    // The capture side closed: the recording has ended.
                    let Some(frame) = frame else { break };
                    // Per-frame linear resampling puts small discontinuities
                    // at the frame boundaries that whole-buffer resampling
                    // does not. Acceptable here precisely because this path
                    // never produces inserted text.
                    match stream.accept(pcm_to_audio(frame)).await {
                        Ok(Some(partial)) => {
                            if partial_tx.try_send(partial).is_err() {
                                task_dropped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::debug!(error = %e, "dictation: streaming pass ended early");
                            return None;
                        }
                    }
                }
            }
        }

        let dropped = task_dropped.load(Ordering::Relaxed);
        if dropped > 0 {
            tracing::debug!(
                dropped,
                "dictation: dropped {dropped} partial hypotheses nobody read in time"
            );
        }

        match stream.finalize().await {
            Ok(transcript) => Some(transcript.text),
            Err(e) => {
                tracing::debug!(error = %e, "dictation: streaming pass produced no final text");
                None
            }
        }
    });

    (
        PartialsSession {
            cancel,
            join,
            dropped,
        },
        partial_rx,
    )
}

impl PartialsSession {
    /// Stops the pump and returns the last hypothesis, or `None` if the
    /// session errored, was never fed, or its task panicked.
    ///
    /// A `JoinError` is tolerated rather than propagated on purpose: the
    /// blocking decode lives outside the guard that owns the dictation run's
    /// processing state, so a panic in it must not wedge dictation.
    pub async fn finish(self) -> Option<String> {
        let _ = self.cancel.send(true);
        match self.join.await {
            Ok(text) => text,
            Err(e) => {
                tracing::warn!(error = %e, "dictation: the streaming pass panicked");
                None
            }
        }
    }

    /// Hypotheses that were produced but never read. Diagnostic only.
    pub fn dropped_partials(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
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
    use kea_engines::traits::{EngineCaps, EngineError, SttEngine};
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
            Ok(Transcript::text_only(self.text.clone()))
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
            Ok(Transcript::text_only(self.text.clone()))
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
        /// The mode the insertion actually went out with, so a test can assert
        /// a profile's choice reached the platform rather than only that some
        /// text arrived.
        mode: Mutex<Option<ReplaceMode>>,
    }

    impl FakeTextIo {
        fn new() -> Self {
            Self {
                inserted: Mutex::new(None),
                mode: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(String::new())
        }

        /// Only the required method is implemented. `insert_at_cursor`'s
        /// default routes through here, so a production path that switches
        /// between the two cannot quietly stop being observed — which is what
        /// happened when insertion moved to `replace_with_mode`.
        async fn replace_with_mode(
            &self,
            text: &str,
            mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            *self.inserted.lock().unwrap() = Some(text.to_string());
            *self.mode.lock().unwrap() = Some(mode);
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

    /// An STT engine that always refuses, so the draft path is exercised
    /// against the failure it exists for rather than a contrived one.
    struct FailingStt;

    #[async_trait]
    impl SttEngine for FailingStt {
        fn id(&self) -> &str {
            "failing-stt"
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
            Err(EngineError::Other("the model fell over".into()))
        }
    }

    /// A scripted streaming stream: one hypothesis per frame, optionally slow,
    /// optionally failing partway.
    struct FakeSttStream {
        script: Vec<&'static str>,
        index: usize,
        delay: std::time::Duration,
        fail_at: Option<usize>,
        endpoint_every: Option<usize>,
    }

    impl FakeSttStream {
        fn new(script: Vec<&'static str>) -> Self {
            Self {
                script,
                index: 0,
                delay: std::time::Duration::ZERO,
                fail_at: None,
                endpoint_every: None,
            }
        }
    }

    #[async_trait]
    impl SttStream for FakeSttStream {
        async fn accept(&mut self, _audio: AudioPcm) -> Result<Option<Partial>, EngineError> {
            if !self.delay.is_zero() {
                tokio::time::sleep(self.delay).await;
            }
            let step = self.index;
            self.index += 1;
            if self.fail_at == Some(step) {
                return Err(EngineError::Other("streaming decoder died".into()));
            }
            let Some(text) = self.script.get(step) else {
                return Ok(None);
            };
            let endpoint = self
                .endpoint_every
                .is_some_and(|n| n > 0 && (step + 1).is_multiple_of(n));
            Ok(Some(Partial {
                text: (*text).to_string(),
                segment: self.endpoint_every.map_or(0, |n| (step / n) as u32),
                endpoint,
            }))
        }

        async fn finalize(self: Box<Self>) -> Result<Transcript, EngineError> {
            if self.index == 0 {
                return Err(EngineError::Other("nothing was ever fed".into()));
            }
            Ok(Transcript::text_only(
                self.script
                    .get(self.index.min(self.script.len()).saturating_sub(1))
                    .copied()
                    .unwrap_or_default(),
            ))
        }
    }

    fn test_settings() -> DictationSettings {
        DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
        }
    }

    fn frame(samples: usize) -> PcmFrame {
        PcmFrame {
            samples: vec![0.25; samples],
            sample_rate_hz: 16_000,
        }
    }

    /// **The invariant the whole feature is built on.** The streaming pass is
    /// display-only: whatever it guessed, the offline engine's transcript is
    /// what gets inserted. This is the test that fails if anyone later
    /// "simplifies" the two passes into one.
    #[tokio::test]
    async fn the_offline_transcript_wins_over_the_live_draft() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "hello world".into(),
        }));
        let textio = Arc::new(FakeTextIo::new());
        let (bindings, actions, presets, overrides) = test_repos().await;
        let mut audio = FakeAudioIo::with_pcm(frame(1600));

        let out = run_dictation_with_opts(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &test_settings(),
            &[],
            &ProfileOverrides::default(),
            DictationRunOpts {
                streaming_draft: Some("hello wurld".into()),
                // Even with the fallback armed: it is a *failure* path, and
                // this run does not fail.
                draft_fallback: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(out, "hello world");
        assert_eq!(
            *textio.inserted.lock().unwrap(),
            Some("hello world".to_string())
        );
    }

    /// A failed decode loses the audio entirely today. The draft can stand in,
    /// but only when asked: inserting knowably worse text after a failure the
    /// user cannot see is the regression the two-pass design prevents.
    #[tokio::test]
    async fn the_draft_is_only_inserted_when_the_fallback_is_on() {
        for (fallback, expected) in [(false, None), (true, Some("hello wurld".to_string()))] {
            let mut reg = EngineRegistry::default();
            reg.register_stt(Arc::new(FailingStt));
            let textio = Arc::new(FakeTextIo::new());
            let (bindings, actions, presets, overrides) = test_repos().await;
            let mut audio = FakeAudioIo::with_pcm(frame(1600));

            let result = run_dictation_with_opts(
                &reg,
                &bindings,
                &actions,
                &presets,
                &overrides,
                &mut audio,
                textio.as_ref(),
                &test_settings(),
                &[],
                &ProfileOverrides::default(),
                DictationRunOpts {
                    streaming_draft: Some("hello wurld".into()),
                    draft_fallback: fallback,
                    ..Default::default()
                },
            )
            .await;

            assert_eq!(result.is_ok(), fallback, "fallback = {fallback}");
            assert_eq!(*textio.inserted.lock().unwrap(), expected);
        }
    }

    /// Partials come out in order, with their segments, and `finish` returns
    /// the last hypothesis.
    #[tokio::test]
    async fn the_pump_forwards_hypotheses_in_order_and_finishes_with_the_last() {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let mut stream = FakeSttStream::new(vec!["the", "the cat", "the cat sat"]);
        stream.endpoint_every = Some(3);
        let (session, mut partials) = spawn_partials(Box::new(stream), rx);

        for _ in 0..3 {
            tx.send(frame(160)).await.unwrap();
        }
        drop(tx);

        let mut seen = Vec::new();
        while let Some(partial) = partials.recv().await {
            seen.push(partial);
        }

        let texts: Vec<&str> = seen.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, vec!["the", "the cat", "the cat sat"]);
        // Only the last one closed a segment.
        assert_eq!(
            seen.iter().filter(|p| p.endpoint).count(),
            1,
            "segments must close on an endpoint and nowhere else"
        );
        assert!(seen.iter().all(|p| p.segment == 0));

        assert_eq!(session.finish().await.as_deref(), Some("the cat sat"));
    }

    /// A stream that dies mid-session must not panic and must not pretend to
    /// have produced a final hypothesis.
    #[tokio::test]
    async fn a_stream_that_errors_yields_no_final_text() {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        let mut stream = FakeSttStream::new(vec!["the", "the cat"]);
        stream.fail_at = Some(1);
        let (session, mut partials) = spawn_partials(Box::new(stream), rx);

        for _ in 0..2 {
            let _ = tx.send(frame(160)).await;
        }
        drop(tx);

        let mut seen = Vec::new();
        while let Some(partial) = partials.recv().await {
            seen.push(partial);
        }
        assert_eq!(seen.len(), 1);
        assert!(session.finish().await.is_none());
    }

    /// The property that makes a lossy preview acceptable: the session buffer
    /// the second pass decodes is written unconditionally, so a starved
    /// streaming consumer cannot cost the user a single sample of the audio
    /// that actually gets transcribed.
    #[tokio::test]
    async fn a_starved_streaming_pass_costs_the_recording_nothing() {
        // Bounded and dropping, exactly like the capture channel.
        let (tx, rx) = tokio::sync::mpsc::channel(64);
        let mut stream = FakeSttStream::new(vec!["falling behind"; 1000]);
        stream.delay = std::time::Duration::from_millis(2);
        let (session, mut partials) = spawn_partials(Box::new(stream), rx);

        // What the capture thread does: offer the frame to the streaming
        // consumer, then record it regardless of whether it landed.
        let mut recorded: Vec<PcmFrame> = Vec::new();
        let mut offered_and_dropped = 0u64;
        for i in 0..1000 {
            let frame = frame(160 + i % 3);
            if tx.try_send(frame.clone()).is_err() {
                offered_and_dropped += 1;
            }
            recorded.push(frame);
        }
        drop(tx);

        assert!(
            offered_and_dropped > 0,
            "a 2ms-per-frame decoder must fall behind 1000 frames"
        );
        assert_eq!(recorded.len(), 1000, "the recording keeps every frame");
        assert!(recorded
            .iter()
            .enumerate()
            .all(|(i, f)| f.samples.len() == 160 + i % 3));

        // Nothing reads the partials, so the pump's own channel fills too —
        // and drops rather than growing.
        let mut received = 0;
        while let Some(_partial) = partials.recv().await {
            received += 1;
        }
        assert!(received <= 1000);
        let _ = session.finish().await;
    }

    #[test]
    fn dictation_declares_stt_slot_and_push_to_talk() {
        let f = DictationFeature;
        assert_eq!(f.id(), "dictation");
        assert_eq!(f.required_caps()[0].name, "stt");
        assert_eq!(f.required_caps()[0].kind, CapKind::Stt);
        assert_eq!(f.commands()[0].id, "push_to_talk");
    }

    /// A profile's insertion mode has to reach the platform call, not just be
    /// stored. Getting this wrong is invisible: the text still lands, via the
    /// clipboard, and only the disruption the user was trying to avoid comes
    /// back.
    #[tokio::test]
    async fn a_profile_chooses_the_insertion_mode() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "hello".into(),
        }));
        let textio = Arc::new(FakeTextIo::new());
        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
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
            &[],
            &ProfileOverrides {
                insertion: Some(ReplaceMode::Accessibility),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(
            *textio.mode.lock().unwrap(),
            Some(ReplaceMode::Accessibility)
        );
    }

    /// The tri-state. `Some(false)` must beat a global `true` — that is the
    /// whole point of turning cleanup off for a shell prompt — and it must not
    /// be confused with `None`, which inherits.
    #[tokio::test]
    async fn a_profile_can_force_post_processing_off() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "raw text".into(),
        }));
        // No LLM engine is registered, so if the refinement pass ran at all the
        // run would fail to resolve one rather than quietly skipping it.
        let textio = Arc::new(FakeTextIo::new());
        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: true,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides {
                post_process: Some(false),
                ..Default::default()
            },
        )
        .await
        .unwrap();

        assert_eq!(out, "raw text");
    }

    /// The end-to-end shape of the feature: the engine mishears, and what the
    /// user's text field receives is nonetheless the stored spelling.
    #[tokio::test]
    async fn vocabulary_is_applied_before_the_text_is_inserted() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "i pushed it to kitty claw today".into(),
        }));

        let textio = Arc::new(FakeTextIo::new());

        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides::default(),
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

    /// A run with the voice-command pass on, which the app layer configures
    /// with two standalone settings keys.
    async fn run_with_commands(
        transcript: &str,
        vocabulary: &[VocabularyEntry],
        language: Option<&str>,
        voice: &VoiceCommandSettings,
    ) -> String {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: transcript.into(),
        }));
        let textio = Arc::new(FakeTextIo::new());
        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            language: language.map(str::to_string),
            ..test_settings()
        };
        let mut audio = FakeAudioIo::with_pcm(frame(1600));

        let out = run_dictation_with_commands(
            &reg,
            &bindings,
            &actions,
            &presets,
            &overrides,
            &mut audio,
            textio.as_ref(),
            &settings,
            vocabulary,
            &ProfileOverrides::default(),
            DictationRunOpts::default(),
            voice,
        )
        .await
        .unwrap();

        assert_eq!(
            textio.inserted.lock().unwrap().as_deref(),
            Some(out.as_str()),
            "what is returned and what is typed must be the same text"
        );
        out
    }

    /// The commands-on settings the app layer would hand in: master switch on,
    /// per-command defaults.
    fn commands_on() -> VoiceCommandSettings {
        VoiceCommandSettings {
            enabled: true,
            enabled_ids: None,
        }
    }

    /// **The ordering, at the feature seam.** Commands run before vocabulary,
    /// so a phrase the decoder misheard is still corrected *after* the full
    /// stop has been taken out of the word stream.
    #[tokio::test]
    async fn voice_commands_compose_with_the_vocabulary_pass() {
        let out = run_with_commands(
            "kitty claw period",
            &[vocab("KittyClaw", Some("kitty claw"), true)],
            Some("en"),
            &commands_on(),
        )
        .await;
        assert_eq!(out, "KittyClaw.");
    }

    /// The row that actually pins the order rather than merely surviving it. A
    /// vocabulary entry that *sounds like* a command word is perfectly legal,
    /// and if the vocabulary pass ran first it would eat every full stop in
    /// the transcript before the command pass ever saw one.
    #[tokio::test]
    async fn the_vocabulary_pass_cannot_eat_a_command_word() {
        let out = run_with_commands(
            "hello period",
            &[vocab("Periodic", Some("period"), true)],
            Some("en"),
            &commands_on(),
        )
        .await;
        assert_eq!(
            out, "hello.",
            "vocabulary first would have produced 'hello Periodic'"
        );
    }

    /// The language gate, honestly. The command list is English-only, so a run
    /// the user asked to decode as German gets its words left alone.
    #[tokio::test]
    async fn a_non_english_run_leaves_the_command_words_alone() {
        let out = run_with_commands("hallo period", &[], Some("de"), &commands_on()).await;
        assert_eq!(out, "hallo period");
    }

    /// And the master switch, which beats the language gate rather than
    /// sitting beside it.
    #[tokio::test]
    async fn the_pass_does_nothing_until_it_is_switched_on() {
        let out = run_with_commands(
            "hello world period",
            &[],
            Some("en"),
            &VoiceCommandSettings::default(),
        )
        .await;
        assert_eq!(out, "hello world period");
    }

    /// Auto-detect resolves the gate from the bound model, which is only known
    /// inside the run — so this is the test that fails if the gate is ever
    /// moved out to the caller.
    #[tokio::test]
    async fn auto_detect_runs_the_pass_only_for_an_english_only_model() {
        for (model, expected) in [
            ("ggml-base.en", "hello world."),
            ("ggml-large-v3", "hello world period"),
        ] {
            let mut reg = EngineRegistry::default();
            reg.register_stt(Arc::new(FakeStt {
                text: "hello world period".into(),
            }));
            let textio = Arc::new(FakeTextIo::new());
            let (bindings, actions, presets, overrides) = test_repos().await;
            let settings = DictationSettings {
                active_model: Some(model.to_string()),
                language: None,
                ..test_settings()
            };
            let mut audio = FakeAudioIo::with_pcm(frame(1600));

            let out = run_dictation_with_commands(
                &reg,
                &bindings,
                &actions,
                &presets,
                &overrides,
                &mut audio,
                textio.as_ref(),
                &settings,
                &[],
                &ProfileOverrides::default(),
                DictationRunOpts::default(),
                &commands_on(),
            )
            .await
            .unwrap();
            assert_eq!(out, expected, "model {model}");
        }
    }

    /// A retraction is a deletion, and the deletion has to reach the app — a
    /// pass that only changes the returned string would type the words.
    #[tokio::test]
    async fn a_retraction_removes_text_before_it_is_typed() {
        let out = run_with_commands(
            "the meeting is monday scratch that tuesday",
            &[],
            Some("en-US"),
            &commands_on(),
        )
        .await;
        assert_eq!(out, "tuesday");
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

        let textio = Arc::new(FakeTextIo::new());
        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides::default(),
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

        let textio = Arc::new(FakeTextIo::new());

        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: false,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides::default(),
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

        let textio = Arc::new(FakeTextIo::new());

        let (bindings, actions, presets, overrides) = test_repos().await;
        let settings = DictationSettings {
            post_process: true,
            active_model: None,
            hold_to_talk: false,
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides::default(),
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

        let textio = Arc::new(FakeTextIo::new());

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
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides::default(),
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

        let textio = Arc::new(FakeTextIo::new());

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
            input_device: None,
            preroll: true,
            language: None,
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
            &ProfileOverrides::default(),
            ContentStorageOpts::disabled(),
        )
        .await
        .unwrap();

        assert!(conversations.list_recent(1).await.unwrap().is_empty());
    }
}
