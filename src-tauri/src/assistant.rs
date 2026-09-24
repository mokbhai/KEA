//! Driving one assistant session: listen, route, answer, speak.
//!
//! The ordering rules live in [`kea_core::assistant::session`], which is pure
//! and tested without a microphone. This module is the part that cannot be:
//! it owns the device, the engines and the window, and its job is to turn
//! those into the events that state machine consumes.
//!
//! **The microphone is borrowed, not held.** A session takes the device when
//! the user presses the binding and returns it when the request ends — exactly
//! what dictation does. Nothing here listens in the background, which is why
//! `AudioIo` needed no change for this feature.
//!
//! **Everything outside this module reaches it through a named seam.** The
//! engines arrive as trait objects, the window as [`Surface`], speech as
//! [`Speaker`], and the device as a `&mut dyn AudioIo`. That is not taste: an
//! `AppHandle` cannot be built in a unit test and neither can `AppState`, so
//! threading them through the pipeline would leave every rule this module owns
//! — when a request ends, when the offline decode is paid for, what the ledger
//! records when the user cancels mid-action — verifiable only by hand against
//! a real microphone. The seams cost one indirection each and buy the tests
//! below.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use kea_core::assistant::dispatch::{ActionOutcome, Dispatcher, RequestPlan};
use kea_core::assistant::routing::{route, RoutingOutcome};
use kea_core::assistant::session::{Session, SessionEvent, SessionState};
use kea_core::assistant::{
    plan, validate_args, ActionRegistry, ActionSpec, AssistantSettings, AssistantSettingsRepo,
    Disclosure, ResolvedArgs,
};
use kea_core::resolve::SlotResolver;
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::settings::SettingsRepo;
use kea_core::TtsSettingsRepo;
use kea_engines::traits::{LlmEngine, StreamingSttEngine, SttEngine, SttOpts};
use kea_features::{ActionGuard, ASSISTANT_FEATURE_ID};
use kea_platform::audio::segment::{find_segment_cut, SegmentCutConfig};
use kea_platform::audio::{AudioIo, DictationState, PcmFrame};
use tauri::AppHandle;

use crate::events;
use crate::AppState;

/// Endpointing tuned for a spoken request rather than a meeting.
///
/// The defaults elsewhere are a three-second pause and a five-second floor,
/// which are right for segmenting a conversation and far too slow here: a
/// question is short, and three seconds of dead air after "what time is it"
/// reads as a hang. The floor matters as much as the pause — without it,
/// drawing breath mid-question ends the request.
const REQUEST_CUT: SegmentCutConfig = SegmentCutConfig {
    pause_secs: 1.2,
    min_secs: 0.8,
    max_secs: 20.0,
};

/// Asks [`find_segment_cut`] only "was anyone speaking in these samples".
///
/// Every field is zero, so the hard cap fires on the first call and the answer
/// comes back as `has_speech` over the whole buffer. A second energy test
/// written here would be a second threshold to keep in agreement with the one
/// doing the endpointing, and the two would drift the first time either was
/// tuned — a session that endpoints on a pause the speech check calls silence
/// is a session that captures a question and then throws it away.
const SPEECH_PROBE: SegmentCutConfig = SegmentCutConfig {
    pause_secs: 0.0,
    min_secs: 0.0,
    max_secs: 0.0,
};

/// How long an answer stays on screen after it has finished being spoken.
///
/// The session ends when this elapses, which takes the overlay down — so
/// without it the answer would flash and vanish the instant playback stopped,
/// leaving nothing to read or copy. Escape cuts it short, and so does a hotkey
/// press starting the next session.
const PRESENT_DWELL_SECS: u64 = 6;

/// Hard ceiling on one capture, independent of the cut config.
///
/// The cut logic needs *some* speech to find a pause after; a session opened in
/// a silent room would otherwise hold the microphone until something happened.
const MAX_CAPTURE_SECS: f32 = 25.0;

/// How long a turn waits for the user to start talking before closing.
///
/// [`MAX_CAPTURE_SECS`] is the backstop for a session that heard a question and
/// never heard it end; this is the one for a session that heard nothing at all,
/// and it has to be far shorter because it is the window a *follow-up* waits
/// in. After an answer the session keeps listening (see [`run_session`]), and
/// the user who is finished says nothing — so this interval is how long the
/// microphone stays open past the end of a conversation, and twenty-five
/// seconds of that is not a feature, it is a light on the menu bar nobody
/// asked for.
const SPEECH_WAIT_SECS: f32 = 4.0;

/// Frames buffered for the live recogniser before hypotheses start being
/// dropped. Display-only, so dropping is the correct response to a decoder
/// that has fallen behind — see `kea_features::dictation::spawn_partials`.
const LIVE_TAP_DEPTH: usize = 32;

// ===========================================================================
// Seams
// ===========================================================================

/// Where a session's visible output goes.
///
/// A trait rather than an `AppHandle` because the interesting properties of
/// this module are about *what* is shown and *when* — that the request text is
/// visible before an action runs, that a partial never appears when live
/// recognition is absent, that the spoken text is the displayed text — and none
/// of those can be asserted against a real window.
pub trait Surface: Send + Sync + 'static {
    fn state(&self, state: &SessionState);
    /// A live hypothesis, while the user is still speaking.
    fn partial(&self, text: &str);
    /// The finished answer. Re-emitted with `speech_error` set when speech was
    /// attempted and failed, so the text on screen is never taken down by the
    /// voice failing — see [`present`].
    fn answer(
        &self,
        request: &str,
        text: &str,
        disclosure: Option<&Disclosure>,
        speech_error: Option<&str>,
    );
}

/// Reading an answer aloud.
///
/// Separate from the TTS engine trait, and a seam of its own, because what this
/// module can get wrong is *which string* it hands to speech and *whether it
/// hands over one at all* — not how a voice renders it. A fake at the engine
/// level would need a bound slot, a settings row and an engine registry to
/// stand up, and would still be testing `kea_features::tts`, which has its own
/// tests.
#[async_trait]
pub trait Speaker: Send + Sync {
    /// Synthesize and play `text`, returning once playback has finished or
    /// been stopped.
    async fn speak(&self, text: &str) -> Result<(), String>;
}

/// The live recogniser a session shows partials from.
///
/// Held as the engine rather than an opened stream so the session owns the
/// failure: `open` answers [`kea_engines::traits::EngineError::ModelNotInstalled`]
/// as its *normal* case, and that has to degrade into "no partial text" rather
/// than into a failed request.
struct LiveRecognizer<'a> {
    engine: &'a dyn StreamingSttEngine,
    opts: SttOpts,
}

/// What one turn's capture produced.
struct Capture {
    /// The authoritative buffer, from `stop_mic`.
    pcm: PcmFrame,
    /// What the live recogniser made of it, when there was one and it
    /// produced anything. `None` is the ordinary case on a machine with no
    /// streaming model installed.
    streaming_text: Option<String>,
}

/// Everything one turn needs that is not the microphone.
///
/// Borrowed rather than owned so `run_session` can resolve the engines once and
/// every turn of the session run against the same ones: re-resolving per turn
/// would let a binding changed mid-conversation take effect halfway through,
/// which is a state nobody asked for and nothing would test.
struct TurnDeps<'a> {
    surface: &'a Arc<dyn Surface>,
    cancel: &'a Arc<AtomicBool>,
    offline: &'a dyn SttEngine,
    offline_opts: SttOpts,
    llm: &'a dyn LlmEngine,
    llm_binding: Binding,
    registry: ActionRegistry,
    dispatcher: &'a Dispatcher,
    actions: &'a ActionRepo,
    speaker: &'a dyn Speaker,
    settings: AssistantSettings,
}

// ===========================================================================
// The session
// ===========================================================================

/// Run one session end to end: one request, and any follow-ups the user speaks
/// before they stop talking to it.
///
/// Errors are returned rather than emitted here so the caller owns how a
/// failure reaches the user; every internal step that can fail already emitted
/// the state the surface needs.
pub async fn run_session(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    let surface: Arc<dyn Surface> = Arc::new(WindowSurface { app: app.clone() });
    let mut session = Session::new();

    // Cleared on the way in rather than on the way out: a press landing just
    // after the previous session ended must not cancel this one before it
    // starts.
    state.assistant_cancel.store(false, Ordering::Relaxed);
    crate::commands::set_assistant_cancellable(state, true);
    // Escape belongs to the rest of the Mac again the moment this returns, by
    // every path including the early ones below.
    let _escape = EscapeGuard {
        state: state.clone(),
    };
    let _overlay = OverlayGuard { app: app.clone() };

    // Asked of the device rather than of a flag: dictation and meeting capture
    // both hold it, and the contract is that activating while it is in use says
    // so. A silently dropped press is indistinguishable from a broken shortcut.
    {
        let audio = state.audio.lock().await;
        if audio.state() != DictationState::Idle {
            return Err("the microphone is already in use".into());
        }
    }

    // Resolved before the microphone opens, not after it closes. A user with
    // nothing bound to the assistant's LLM slot otherwise speaks a whole
    // question into a session that was never able to answer it, and is told so
    // only once they have finished.
    let bindings = BindingRepo::new(state.config_pool.clone());
    let prepared = prepare(state, &bindings).await;
    let prepared = match prepared {
        Ok(p) => p,
        Err(error) => {
            session.apply(SessionEvent::Failed {
                message: error.clone(),
            });
            surface.state(session.state());
            return Err(error);
        }
    };

    let speaker = TtsSpeaker {
        state: state.clone(),
    };
    let actions = ActionRepo::new(state.data_pool.clone());
    let dispatcher = build_dispatcher(state, app);
    let deps = TurnDeps {
        surface: &surface,
        cancel: &state.assistant_cancel,
        offline: prepared.offline.as_ref(),
        offline_opts: prepared.offline_opts.clone(),
        llm: prepared.llm.as_ref(),
        llm_binding: prepared.llm_binding.clone(),
        registry: ActionRegistry::default(),
        dispatcher: &dispatcher,
        actions: &actions,
        speaker: &speaker,
        settings: prepared.settings.clone(),
    };

    loop {
        surface.state(session.state());

        let capture = {
            let mut audio = state.audio.lock().await;
            let live = prepared.streaming.as_ref().map(|engine| LiveRecognizer {
                engine: engine.as_ref(),
                opts: prepared.streaming_opts.clone(),
            });
            capture_turn(&mut **audio, live, &state.assistant_cancel, &surface).await
        };

        let capture = match capture {
            Ok(capture) => capture,
            Err(error) => {
                session.apply(SessionEvent::Failed {
                    message: error.clone(),
                });
                surface.state(session.state());
                return Err(error);
            }
        };

        // Cancelled while listening: the audio is dropped and nothing is sent.
        if cancelled(&state.assistant_cancel) {
            return Ok(());
        }

        match run_turn(&deps, &mut session, capture).await {
            // Nothing was said. The session closes — on the first turn because
            // the binding was pressed by accident, on a later one because the
            // conversation is over.
            Ok(false) => return Ok(()),
            Ok(true) => {}
            Err(error) => {
                // A cancel that arrived mid-flight is not a failure to report:
                // the user ended it deliberately, and an error banner for their
                // own action reads as a bug.
                if cancelled(&state.assistant_cancel) {
                    return Ok(());
                }
                session.apply(SessionEvent::Failed {
                    message: error.clone(),
                });
                surface.state(session.state());
                return Err(error);
            }
        }

        if cancelled(&state.assistant_cancel) {
            return Ok(());
        }

        // Leave the answer up long enough to read. Polled rather than slept in
        // one go so Escape still ends the session immediately.
        for _ in 0..(PRESENT_DWELL_SECS * 10) {
            if cancelled(&state.assistant_cancel) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
        if cancelled(&state.assistant_cancel) {
            return Ok(());
        }

        // Back to listening without the user pressing anything: a follow-up is
        // spoken, not activated. The turn just completed stays in
        // `session.turns` and becomes context for the next one; the session
        // value is dropped when this function returns, which is the whole of
        // "history does not outlive the session".
        session.apply(SessionEvent::FollowUpStarted);
    }
}

/// The engines and settings a session runs against.
struct Prepared {
    offline: Arc<dyn SttEngine>,
    offline_opts: SttOpts,
    /// `None` when no streaming model is selected or this build registers no
    /// engine for one. Live partials are an enhancement, never a dependency.
    streaming: Option<Arc<dyn StreamingSttEngine>>,
    streaming_opts: SttOpts,
    llm: Arc<dyn LlmEngine>,
    llm_binding: Binding,
    settings: AssistantSettings,
}

async fn prepare(state: &Arc<AppState>, bindings: &BindingRepo) -> Result<Prepared, String> {
    let stt_binding = SlotResolver::new(&state.engines, bindings)
        .require_stt(ASSISTANT_FEATURE_ID)
        .await
        .map_err(|e| e.to_string())?;
    let offline = state
        .engines
        .stt(&stt_binding.engine_id)
        .ok_or_else(|| format!("no stt engine '{}'", stt_binding.engine_id))?;

    let llm_binding = SlotResolver::new(&state.engines, bindings)
        .require_llm(ASSISTANT_FEATURE_ID)
        .await
        .map_err(|e| e.to_string())?;
    let llm = state
        .engines
        .llm(&llm_binding.engine_id)
        .ok_or_else(|| format!("no llm engine '{}'", llm_binding.engine_id))?;

    // The streaming model is the one dictation already downloads and names.
    // Sharing the setting rather than adding an assistant-specific one keeps a
    // single answer to "is live recognition available on this machine"; the
    // *dictation* HUD's `show_partials` toggle is deliberately not consulted,
    // because it is about that HUD and turning it off must not silently take
    // the assistant's two-pass routing down to one pass.
    let streaming_model = SettingsRepo::new(state.config_pool.clone())
        .get_optional::<String>(crate::commands::STREAMING_MODEL_SETTING)
        .await
        .unwrap_or_default()
        .map(|m| m.trim().to_string())
        .filter(|m| !m.is_empty());
    let streaming = streaming_model
        .as_ref()
        .and_then(|_| state.engines.any_streaming_stt());

    let settings = AssistantSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .unwrap_or_default();

    Ok(Prepared {
        offline,
        offline_opts: SttOpts {
            model: stt_binding.model.clone(),
            provider_ref: stt_binding.provider_ref.clone(),
            ..Default::default()
        },
        streaming,
        streaming_opts: SttOpts {
            model: streaming_model,
            ..Default::default()
        },
        llm,
        llm_binding,
        settings,
    })
}

/// One turn, from captured audio to a presented answer.
///
/// `Ok(false)` means nothing was said, which closes the session. That decision
/// lives here rather than at the call site because it is the spec's hardest
/// requirement in this module — an empty request must never become a prompt —
/// and a guard written at the call site is a guard the tests reach around.
async fn run_turn(
    deps: &TurnDeps<'_>,
    session: &mut Session,
    capture: Capture,
) -> Result<bool, String> {
    if !contains_speech(&capture.pcm) {
        return Ok(false);
    }

    session.apply(SessionEvent::RequestEnded);
    deps.surface.state(session.state());
    complete_turn(deps, session, capture).await?;
    Ok(true)
}

/// Hold the microphone until the user stops talking.
///
/// Frames are *copied* to the live recogniser rather than handed to it, which
/// dictation does not have to do: there the streaming tap is the only consumer
/// of the frame stream, while here the same frames also drive endpointing. The
/// copy is one `Vec<f32>` per 20ms frame, against a decoder that is about to
/// run a neural net over it.
async fn capture_turn(
    audio: &mut dyn AudioIo,
    live: Option<LiveRecognizer<'_>>,
    cancel: &Arc<AtomicBool>,
    surface: &Arc<dyn Surface>,
) -> Result<Capture, String> {
    let mut frames = audio.start_mic().await.map_err(|e| e.to_string())?;

    // Opened after the device, so a recogniser that takes a moment to load its
    // bundle cannot delay the microphone opening — the user is already talking.
    // The frames that arrive during the load are buffered by the capture layer
    // and still reach `stop_mic`, so the cost is a late first *partial*, never
    // lost audio. Awaited rather than spawned, unlike dictation's equivalent,
    // because here the hypothesis is an input to routing and the turn has
    // nothing to route without it.
    let live = match live {
        Some(live) => match live.engine.open(live.opts).await {
            Ok(stream) => Some(stream),
            Err(error) => {
                // The normal case on a machine that never downloaded the
                // model. Silent by design: an enhancement that announces its
                // own absence is worse than one that is simply absent.
                tracing::debug!(%error, "assistant: no live partials for this request");
                None
            }
        },
        None => None,
    };

    let mut pump = live.map(|stream| {
        let (tap_tx, tap_rx) = tokio::sync::mpsc::channel(LIVE_TAP_DEPTH);
        let (session, mut partials) = kea_features::dictation::spawn_partials(stream, tap_rx);
        let emit = surface.clone();
        tokio::spawn(async move {
            while let Some(partial) = partials.recv().await {
                emit.partial(&partial.text);
            }
        });
        (tap_tx, session)
    });

    let mut samples: Vec<f32> = Vec::new();
    let mut rate = 0u32;
    let mut heard_speech = false;

    while let Some(frame) = frames.recv().await {
        if rate == 0 {
            rate = frame.sample_rate_hz;
        }
        samples.extend_from_slice(&frame.samples);
        if let Some((tap, _)) = &mut pump {
            let _ = tap.try_send(frame);
        }

        if cancelled(cancel) {
            break;
        }
        if rate == 0 {
            continue;
        }
        let elapsed = samples.len() as f32 / rate as f32;
        // Checked once, at the point the window closes: before then the answer
        // is "not yet", and after then the turn is over either way.
        if !heard_speech && elapsed >= SPEECH_WAIT_SECS {
            heard_speech = speech_in(&samples, rate);
            if !heard_speech {
                break;
            }
        }
        if find_segment_cut(&samples, rate, REQUEST_CUT).is_some() {
            break;
        }
        if elapsed >= MAX_CAPTURE_SECS {
            break;
        }
    }

    // `stop_mic` returns the authoritative buffer — the same one dictation
    // transcribes — so the frames above are only used to decide *when* to stop.
    // Taking both keeps the endpointing honest without the buffer having two
    // owners.
    let pcm = audio.stop_mic().await.map_err(|e| e.to_string())?;

    let streaming_text = match pump {
        Some((tap, session)) => {
            // Dropped first so the pump sees its input close and finalizes,
            // rather than waiting for the cancel signal to race it.
            drop(tap);
            session.finish().await
        }
        None => None,
    };

    Ok(Capture {
        pcm,
        streaming_text,
    })
}

async fn complete_turn(
    deps: &TurnDeps<'_>,
    session: &mut Session,
    capture: Capture,
) -> Result<(), String> {
    // **Routing runs on the live hypothesis when there is one.** Intent
    // survives word error — a misheard verb still routes — so waiting for a
    // full offline re-decode before deciding *what was asked* is latency paid
    // on every request for an improvement most requests cannot use. The
    // offline pass is bought back below, and only for the arguments that
    // cannot survive being misheard.
    let live = capture
        .streaming_text
        .as_deref()
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(str::to_string);

    let (utterance, offline_text) = match live {
        Some(text) => (text, None),
        None => {
            let text = transcribe(deps, &capture.pcm).await?;
            (text.clone(), Some(text))
        }
    };
    if utterance.trim().is_empty() {
        return Err("I didn't catch that.".into());
    }
    session.set_request(utterance.clone());

    let outcome = route(
        deps.llm,
        &deps.llm_binding,
        &deps.registry,
        &utterance,
        session.history(),
    )
    .await
    .map_err(|e| e.to_string())?;

    match plan(outcome, &deps.registry) {
        RequestPlan::Answer { text } => present(deps, session, &utterance, text, None).await,

        RequestPlan::Invoke { spec, args } => {
            // The last gate before anything happens outside this process. An
            // action that has not started must not start now.
            if cancelled(deps.cancel) {
                return Ok(());
            }
            let args = accurate_args(deps, session, &capture.pcm, spec, args, offline_text).await?;
            let ActionOutcome { text, disclosure } = invoke_recorded(deps, spec, &args).await?;
            if cancelled(deps.cancel) {
                return Ok(());
            }
            let spoken = text.unwrap_or_else(|| format!("Done: {}.", spec.title));
            present(deps, session, &utterance, spoken, disclosure).await
        }

        // Both of these are words, not failures: the assistant understood
        // enough to ask, and the session stays open for the answer.
        RequestPlan::Clarify { question } => {
            present(deps, session, &utterance, question, None).await
        }

        RequestPlan::Fail { message } => Err(message),
    }
}

/// Re-derive the action's spoken arguments from the accuracy-oriented engine.
///
/// **Only the values change, never the action.** The action was chosen from the
/// live hypothesis and stays chosen; what a streaming decoder loses first is
/// proper nouns, and a proper noun is exactly what an argument cannot survive
/// losing — "open sarari" still routes to `open_app`, and still opens nothing.
///
/// The accurate values are obtained by routing the offline transcript a second
/// time and lifting the spoken arguments out of the result. The alternative —
/// aligning the first pass's argument string against the offline transcript and
/// substituting the best-matching span — needs no second completion, and was
/// rejected because it is a fuzzy string match standing between a transcript
/// and something that runs: it fails silently and differently for every
/// argument, where a second route either produces the same action with better
/// values or is discarded.
///
/// An action with no spoken arguments never reaches this function's expensive
/// half — [`ActionSpec::needs_accurate_transcript`] is checked first — which is
/// the whole reason that flag exists.
async fn accurate_args(
    deps: &TurnDeps<'_>,
    session: &Session,
    pcm: &PcmFrame,
    spec: &'static ActionSpec,
    mut args: ResolvedArgs,
    already_decoded: Option<String>,
) -> Result<ResolvedArgs, String> {
    if !spec.needs_accurate_transcript() {
        return Ok(args);
    }
    // Already offline: routing ran on the accurate transcript to begin with,
    // because no live recogniser produced anything. Re-decoding would buy the
    // same words twice.
    if already_decoded.is_some() {
        return Ok(args);
    }

    let accurate = match transcribe(deps, pcm).await {
        Ok(text) => text,
        Err(error) => {
            // The live hypothesis already produced a usable route. Failing the
            // whole request because the *second* opinion was unavailable would
            // turn a degraded answer into no answer.
            tracing::warn!(%error, "assistant: no accurate transcript for the action's arguments");
            return Ok(args);
        }
    };
    if accurate.trim().is_empty() {
        return Ok(args);
    }

    let second = route(
        deps.llm,
        &deps.llm_binding,
        &deps.registry,
        &accurate,
        session.history(),
    )
    .await
    .map_err(|e| e.to_string())?;

    // A second pass that changed its mind about the action is discarded, not
    // acted on: the user saw the first decision, and swapping the action under
    // them on the strength of a better transcript is exactly the "it did
    // something I didn't ask for" failure this feature cannot afford.
    let RoutingOutcome::Action {
        spec: second_spec,
        raw_args,
    } = second
    else {
        return Ok(args);
    };
    if second_spec.id != spec.id {
        return Ok(args);
    }
    let Ok(accurate_args) = validate_args(spec, &raw_args) else {
        return Ok(args);
    };

    for name in spec.spoken_arg_names() {
        // Absent in the second pass means the accurate transcript did not
        // carry the value, not that the first pass's value is now wrong.
        if let Some(value) = accurate_args.get(name) {
            args.replace(name, value.clone());
        }
    }
    Ok(args)
}

/// Run the action, writing the ledger row that says it happened.
///
/// The row's `command` is the action id, so History reads the same vocabulary
/// the registry and the router use. Its `engine_id` is the *router's* engine,
/// which is the honest answer to "what decided this": the action itself runs no
/// engine, and leaving the column empty would make an assistant invocation the
/// only row in the ledger that cannot say where it came from.
async fn invoke_recorded(
    deps: &TurnDeps<'_>,
    spec: &'static ActionSpec,
    args: &ResolvedArgs,
) -> Result<ActionOutcome, String> {
    let action_id = deps
        .actions
        .record(NewAction {
            feature_id: ASSISTANT_FEATURE_ID.to_string(),
            command: spec.id.to_string(),
            engine_id: deps.llm_binding.engine_id.clone(),
            model: deps.llm_binding.model.clone(),
            provider_ref: deps.llm_binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;
    let guard = ActionGuard::new(deps.actions, action_id, ASSISTANT_FEATURE_ID);

    let outcome = deps.dispatcher.invoke(spec, args).await;

    // Cancellation is checked before the outcome, and wins over it. A user who
    // pressed Escape while an action was running is not looking at a fault, and
    // a red row in History for their own decision is a bug report waiting to be
    // filed. An action that also failed on its way out was already doomed by
    // the cancel; the reason recorded is the one the user can act on.
    if cancelled(deps.cancel) {
        let reason = guard.cancel("the user cancelled the session").await;
        return Err(reason);
    }

    match outcome {
        Ok(outcome) => {
            guard.succeed().await;
            Ok(outcome)
        }
        Err(error) => Err(guard.fail(error).await),
    }
}

/// Decode the buffered request with the accuracy-oriented engine.
///
/// Timed, and logged whether it succeeded or not, because design.md's open
/// question — what the offline re-decode actually costs — is the one that
/// decides when the second pass is worth skipping, and the failed decode is
/// the one most worth pricing since it is pure cost. `samples` is counted as
/// the engine receives them so `elapsed_ms / samples` compares across capture
/// devices at different rates; the same shape the dictation path logs, so the
/// two can be read against each other.
async fn transcribe(deps: &TurnDeps<'_>, pcm: &PcmFrame) -> Result<String, String> {
    let started = std::time::Instant::now();
    let samples = pcm.samples.len();
    let result =
        kea_features::meeting::transcribe_pcm_segment(deps.offline, pcm, deps.offline_opts.clone())
            .await;
    tracing::info!(
        engine = deps.offline.id(),
        samples,
        sample_rate_hz = pcm.sample_rate_hz,
        elapsed_ms = started.elapsed().as_millis() as u64,
        ok = result.is_ok(),
        "assistant: offline decode finished"
    );
    Ok(result.map_err(|e| e.to_string())?.text)
}

/// Show the answer, speak it, and move the session to `Presenting`.
///
/// The answer reaches the surface *before* speech is attempted, and the same
/// text is what gets spoken. Both orderings matter: the design forbids a short
/// spoken version diverging from a longer written one, and a voice that fails
/// must not be able to take the text down with it — which is what emitting
/// after synthesis would allow.
async fn present(
    deps: &TurnDeps<'_>,
    session: &mut Session,
    request: &str,
    answer: String,
    disclosure: Option<Disclosure>,
) -> Result<(), String> {
    deps.surface
        .answer(request, &answer, disclosure.as_ref(), None);

    let spoken = if deps.settings.speak_answers {
        match deps.speaker.speak(&answer).await {
            Ok(()) => true,
            Err(error) => {
                // Speech is attempted, never required. The answer stays on
                // screen and the failure is re-emitted alongside it, rather
                // than becoming a session failure that replaces the answer the
                // user can still read.
                tracing::warn!(%error, "assistant: the answer could not be spoken");
                deps.surface
                    .answer(request, &answer, disclosure.as_ref(), Some(&error));
                false
            }
        }
    } else {
        false
    };

    session.apply(SessionEvent::Answered {
        text: answer,
        spoken,
    });
    // Playback above is awaited to completion, so by the time the state is
    // emitted it is no longer speaking. Reported honestly rather than as
    // `speaking: true`, which would leave a stop control on screen for audio
    // that already finished.
    session.apply(SessionEvent::SpeechFinished);
    deps.surface.state(session.state());
    Ok(())
}

/// Whether the user has asked for this session to stop.
fn cancelled(cancel: &Arc<AtomicBool>) -> bool {
    cancel.load(Ordering::Relaxed)
}

/// Whether anything in `pcm` reads as speech to the endpointing detector.
fn contains_speech(pcm: &PcmFrame) -> bool {
    speech_in(&pcm.samples, pcm.sample_rate_hz)
}

fn speech_in(samples: &[f32], sample_rate_hz: u32) -> bool {
    find_segment_cut(samples, sample_rate_hz, SPEECH_PROBE)
        .map(|cut| cut.has_speech)
        .unwrap_or(false)
}

// ===========================================================================
// The real world behind the seams
// ===========================================================================

/// The session surface as the running app shows it.
struct WindowSurface {
    app: AppHandle,
}

impl Surface for WindowSurface {
    fn state(&self, state: &SessionState) {
        events::emit_assistant_state(&self.app, state);
    }

    fn partial(&self, text: &str) {
        events::emit_assistant_partial(&self.app, text);
    }

    fn answer(
        &self,
        request: &str,
        text: &str,
        disclosure: Option<&Disclosure>,
        speech_error: Option<&str>,
    ) {
        events::emit_assistant_answer(&self.app, request, text, disclosure, speech_error);
    }
}

/// Speaking through the user's bound TTS slot and the shared playback path.
struct TtsSpeaker {
    state: Arc<AppState>,
}

#[async_trait]
impl Speaker for TtsSpeaker {
    async fn speak(&self, text: &str) -> Result<(), String> {
        let settings = TtsSettingsRepo::new(SettingsRepo::new(self.state.config_pool.clone()))
            .get()
            .await
            .unwrap_or_default();
        let bindings = BindingRepo::new(self.state.config_pool.clone());
        let pcm = kea_features::tts::speak_text_for(
            &self.state.engines,
            &bindings,
            &settings,
            ASSISTANT_FEATURE_ID,
            text,
        )
        .await?;

        let cancel = self.state.assistant_cancel.clone();
        tokio::task::spawn_blocking(move || {
            kea_platform::audio::playback::play_pcm_cancellable(&pcm, &cancel)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
    }
}

/// Takes the overlay down when the session ends, however it ends.
///
/// Paired with the Escape guard for the same reason: `run_session` has several
/// early exits, and the one that forgets leaves a pill on screen over every
/// other application until the next session opens.
struct OverlayGuard {
    app: AppHandle,
}

impl Drop for OverlayGuard {
    fn drop(&mut self) {
        crate::overlay::sync_assistant_visibility(&self.app, false);
    }
}

/// Releases the Escape binding when the session ends, however it ends.
///
/// A guard rather than a call at each return: `run_session` has several early
/// exits and more will be added, and the one that forgets is the one that
/// leaves Escape captured from every other application on the Mac until KEA is
/// restarted.
struct EscapeGuard {
    state: Arc<AppState>,
}

impl Drop for EscapeGuard {
    fn drop(&mut self) {
        crate::commands::set_assistant_cancellable(&self.state, false);
    }
}

/// The handlers this build installs.
fn build_dispatcher(state: &Arc<AppState>, app: &AppHandle) -> Dispatcher {
    let mut d = Dispatcher::new();
    d.register(Arc::new(kea_features::assistant::ReadFocused::new(
        Arc::from(kea_platform::new_text_io()),
        Arc::from(kea_platform::screen::new_screen_reader()),
    )));
    d.register(Arc::new(kea_features::assistant::OpenApp::new(Arc::from(
        kea_platform::apps::new_app_launcher(),
    ))));
    // The two handlers that need `AppState` and an `AppHandle` — starting a
    // meeting goes through the app layer's own meeting start, not past it.
    crate::assistant_handlers::register_state_handlers(&mut d, state, app);
    d
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_core::assistant::dispatch::{ActionFailure, ActionHandler};
    use kea_core::assistant::registry::{OPEN_APP, READ_FOCUSED};
    use kea_core::store::actions::ActionStatus;
    use kea_core::store::db::{open_pool, run_data_migrations};
    use kea_engines::traits::{
        AudioPcm, EngineCaps, EngineError, LlmRequest, LlmResponse, Partial, SttStream, Transcript,
    };
    use kea_platform::AudioIoError;
    use std::sync::Mutex;

    // -- fakes ------------------------------------------------------------

    /// What the session showed, in the order it showed it.
    #[derive(Debug, Clone, PartialEq)]
    enum Shown {
        State(SessionState),
        Partial(String),
        Answer {
            request: String,
            text: String,
            speech_error: Option<String>,
        },
    }

    #[derive(Default)]
    struct RecordingSurface {
        shown: Mutex<Vec<Shown>>,
    }

    impl RecordingSurface {
        fn shown(&self) -> Vec<Shown> {
            self.shown.lock().unwrap().clone()
        }

        fn partials(&self) -> Vec<String> {
            self.shown()
                .into_iter()
                .filter_map(|s| match s {
                    Shown::Partial(text) => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn answers(&self) -> Vec<Shown> {
            self.shown()
                .into_iter()
                .filter(|s| matches!(s, Shown::Answer { .. }))
                .collect()
        }
    }

    impl Surface for RecordingSurface {
        fn state(&self, state: &SessionState) {
            self.shown.lock().unwrap().push(Shown::State(state.clone()));
        }
        fn partial(&self, text: &str) {
            self.shown
                .lock()
                .unwrap()
                .push(Shown::Partial(text.to_string()));
        }
        fn answer(
            &self,
            request: &str,
            text: &str,
            _disclosure: Option<&Disclosure>,
            speech_error: Option<&str>,
        ) {
            self.shown.lock().unwrap().push(Shown::Answer {
                request: request.to_string(),
                text: text.to_string(),
                speech_error: speech_error.map(str::to_string),
            });
        }
    }

    /// An offline engine that answers with canned text and counts how often it
    /// was asked. The count is the point: the two-pass rule is a claim about
    /// *how many times* this runs.
    struct CountingStt {
        text: String,
        calls: Arc<AtomicUsizeCell>,
    }

    /// `AtomicUsize` under a name that reads at the call sites below.
    #[derive(Default)]
    struct AtomicUsizeCell(std::sync::atomic::AtomicUsize);

    impl AtomicUsizeCell {
        fn get(&self) -> usize {
            self.0.load(Ordering::SeqCst)
        }
        fn bump(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    impl CountingStt {
        fn new(text: &str) -> (Self, Arc<AtomicUsizeCell>) {
            let calls = Arc::new(AtomicUsizeCell::default());
            (
                Self {
                    text: text.to_string(),
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    #[async_trait]
    impl SttEngine for CountingStt {
        fn id(&self) -> &str {
            "counting-stt"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn transcribe(
            &self,
            _audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            self.calls.bump();
            Ok(Transcript::text_only(self.text.clone()))
        }
    }

    /// An LLM whose reply is a function of the utterance in the prompt, so a
    /// test can say "route this transcript one way and that one another" —
    /// which is what makes the two-pass rule observable.
    struct ScriptedLlm {
        reply: Box<dyn Fn(&str) -> String + Send + Sync>,
        prompts: Arc<Mutex<Vec<String>>>,
    }

    impl ScriptedLlm {
        fn new(
            reply: impl Fn(&str) -> String + Send + Sync + 'static,
        ) -> (Self, Arc<Mutex<Vec<String>>>) {
            let prompts = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    reply: Box::new(reply),
                    prompts: prompts.clone(),
                },
                prompts,
            )
        }
    }

    /// The utterance as `build_routing_prompt` writes it into the prompt.
    fn utterance_of(prompt: &str) -> String {
        prompt
            .rsplit_once("User said: ")
            .map(|(_, rest)| rest.trim().to_string())
            .unwrap_or_default()
    }

    #[async_trait]
    impl LlmEngine for ScriptedLlm {
        fn id(&self) -> &str {
            "scripted"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
            self.prompts.lock().unwrap().push(req.prompt.clone());
            Ok(LlmResponse::untracked((self.reply)(&utterance_of(
                &req.prompt,
            ))))
        }
    }

    /// A speaker that records what it was asked to say, and can refuse.
    struct RecordingSpeaker {
        said: Mutex<Vec<String>>,
        fails_with: Option<String>,
    }

    impl RecordingSpeaker {
        fn working() -> Self {
            Self {
                said: Mutex::new(Vec::new()),
                fails_with: None,
            }
        }
        fn broken(message: &str) -> Self {
            Self {
                said: Mutex::new(Vec::new()),
                fails_with: Some(message.to_string()),
            }
        }
        fn said(&self) -> Vec<String> {
            self.said.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Speaker for RecordingSpeaker {
        async fn speak(&self, text: &str) -> Result<(), String> {
            self.said.lock().unwrap().push(text.to_string());
            match &self.fails_with {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            }
        }
    }

    /// A streaming recogniser that emits one hypothesis per frame and
    /// finalizes to the last of them.
    struct ScriptedStreaming {
        hypotheses: Vec<String>,
    }

    struct ScriptedStream {
        hypotheses: std::collections::VecDeque<String>,
        last: String,
    }

    #[async_trait]
    impl StreamingSttEngine for ScriptedStreaming {
        fn id(&self) -> &str {
            "scripted-streaming"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn open(&self, _opts: SttOpts) -> Result<Box<dyn SttStream>, EngineError> {
            Ok(Box::new(ScriptedStream {
                hypotheses: self.hypotheses.iter().cloned().collect(),
                last: String::new(),
            }))
        }
    }

    #[async_trait]
    impl SttStream for ScriptedStream {
        async fn accept(&mut self, _audio: AudioPcm) -> Result<Option<Partial>, EngineError> {
            match self.hypotheses.pop_front() {
                Some(text) => {
                    self.last = text.clone();
                    Ok(Some(Partial {
                        text,
                        segment: 0,
                        endpoint: false,
                    }))
                }
                None => Ok(None),
            }
        }
        async fn finalize(self: Box<Self>) -> Result<Transcript, EngineError> {
            Ok(Transcript::text_only(self.last.clone()))
        }
    }

    /// The ordinary case on a machine that never downloaded the model.
    struct NotInstalled;

    #[async_trait]
    impl StreamingSttEngine for NotInstalled {
        fn id(&self) -> &str {
            "not-installed"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn open(&self, _opts: SttOpts) -> Result<Box<dyn SttStream>, EngineError> {
            Err(EngineError::ModelNotInstalled("parakeet-stream".into()))
        }
    }

    /// A capture device that delivers the frames it was given, one at a time,
    /// and returns as its buffer only the frames it actually got to deliver.
    ///
    /// The single-slot channel is what makes that true: the sender parks until
    /// the session takes the previous frame, so a session that stops early
    /// leaves the rest undelivered — which is how a test can tell endpointing
    /// from running out of audio.
    struct FramesAudio {
        frames: Vec<PcmFrame>,
        rate: u32,
        delivered: Arc<Mutex<Vec<f32>>>,
        state: DictationState,
    }

    impl FramesAudio {
        fn new(frames: Vec<PcmFrame>, rate: u32) -> Self {
            Self {
                frames,
                rate,
                delivered: Arc::new(Mutex::new(Vec::new())),
                state: DictationState::Idle,
            }
        }
    }

    #[async_trait]
    impl AudioIo for FramesAudio {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.state = DictationState::Listening;
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            let frames = self.frames.clone();
            let delivered = self.delivered.clone();
            tokio::spawn(async move {
                for frame in frames {
                    delivered.lock().unwrap().extend_from_slice(&frame.samples);
                    if tx.send(frame).await.is_err() {
                        return;
                    }
                }
                // The device does not close when it runs out of script: a real
                // microphone keeps the channel open, and a session that only
                // ever ends because its input ended proves nothing about
                // endpointing.
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                drop(tx);
            });
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.state = DictationState::Idle;
            Ok(PcmFrame {
                samples: self.delivered.lock().unwrap().clone(),
                sample_rate_hz: self.rate,
            })
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            self.state
        }
    }

    /// An action that succeeds, recording the arguments it was handed.
    struct RecordingHandler {
        id: &'static str,
        text: &'static str,
        seen: Arc<Mutex<Vec<ResolvedArgs>>>,
    }

    #[async_trait]
    impl ActionHandler for RecordingHandler {
        fn id(&self) -> &'static str {
            self.id
        }
        async fn run(&self, args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
            self.seen.lock().unwrap().push(args.clone());
            Ok(ActionOutcome {
                text: Some(self.text.to_string()),
                disclosure: None,
            })
        }
    }

    struct FailingHandler {
        id: &'static str,
    }

    #[async_trait]
    impl ActionHandler for FailingHandler {
        fn id(&self) -> &'static str {
            self.id
        }
        async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
            Err(ActionFailure::Unavailable {
                what: "the frontmost window".into(),
            })
        }
    }

    /// An action that the user cancels while it is running.
    struct CancellingHandler {
        id: &'static str,
        cancel: Arc<AtomicBool>,
    }

    #[async_trait]
    impl ActionHandler for CancellingHandler {
        fn id(&self) -> &'static str {
            self.id
        }
        async fn run(&self, _args: &ResolvedArgs) -> Result<ActionOutcome, ActionFailure> {
            self.cancel.store(true, Ordering::Relaxed);
            Ok(ActionOutcome {
                text: Some("read something".into()),
                disclosure: None,
            })
        }
    }

    // -- fixtures ---------------------------------------------------------

    /// 20ms of a loud constant tone, which the energy detector calls speech.
    fn speech_frame(rate: u32) -> PcmFrame {
        let len = rate as usize / 50;
        PcmFrame {
            samples: (0..len)
                .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
                .collect(),
            sample_rate_hz: rate,
        }
    }

    /// 20ms of silence.
    fn silent_frame(rate: u32) -> PcmFrame {
        PcmFrame {
            samples: vec![0.0; rate as usize / 50],
            sample_rate_hz: rate,
        }
    }

    fn frames(rate: u32, speech_ms: u32, silence_ms: u32) -> Vec<PcmFrame> {
        let mut out = Vec::new();
        out.extend((0..speech_ms / 20).map(|_| speech_frame(rate)));
        out.extend((0..silence_ms / 20).map(|_| silent_frame(rate)));
        out
    }

    fn spoken_pcm(rate: u32, secs: f32) -> PcmFrame {
        let len = (rate as f32 * secs) as usize;
        PcmFrame {
            samples: (0..len)
                .map(|i| if i % 2 == 0 { 0.5 } else { -0.5 })
                .collect(),
            sample_rate_hz: rate,
        }
    }

    fn silent_pcm(rate: u32, secs: f32) -> PcmFrame {
        PcmFrame {
            samples: vec![0.0; (rate as f32 * secs) as usize],
            sample_rate_hz: rate,
        }
    }

    fn binding() -> Binding {
        Binding {
            engine_id: "scripted".into(),
            model: None,
            provider_ref: None,
        }
    }

    fn capture_of(pcm: PcmFrame, streaming_text: Option<&str>) -> Capture {
        Capture {
            pcm,
            streaming_text: streaming_text.map(str::to_string),
        }
    }

    fn answers_with(text: &'static str) -> impl Fn(&str) -> String + Send + Sync + 'static {
        move |_utterance: &str| format!(r#"{{"answer": "{text}"}}"#)
    }

    fn routes_to_read_focused(_utterance: &str) -> String {
        r#"{"action": "read_focused"}"#.to_string()
    }

    /// One turn's worth of fakes, owned together.
    ///
    /// [`TurnDeps`] borrows all of them, so a free helper taking one argument
    /// per field would hand every test eight bindings to keep alive by hand.
    /// Owning them in one place is what lets a test below say only what it is
    /// actually about.
    struct Harness {
        recording: Arc<RecordingSurface>,
        surface: Arc<dyn Surface>,
        cancel: Arc<AtomicBool>,
        offline: CountingStt,
        offline_calls: Arc<AtomicUsizeCell>,
        llm: ScriptedLlm,
        prompts: Arc<Mutex<Vec<String>>>,
        speaker: RecordingSpeaker,
        dispatcher: Dispatcher,
        actions: ActionRepo,
        settings: AssistantSettings,
    }

    impl Harness {
        /// `offline_says` is what the accuracy-oriented engine returns, and
        /// `reply` is how the router answers a given utterance.
        async fn new(
            offline_says: &str,
            reply: impl Fn(&str) -> String + Send + Sync + 'static,
        ) -> Self {
            let recording = Arc::new(RecordingSurface::default());
            let surface: Arc<dyn Surface> = recording.clone();
            let (offline, offline_calls) = CountingStt::new(offline_says);
            let (llm, prompts) = ScriptedLlm::new(reply);
            Self {
                recording,
                surface,
                cancel: Arc::new(AtomicBool::new(false)),
                offline,
                offline_calls,
                llm,
                prompts,
                speaker: RecordingSpeaker::working(),
                dispatcher: Dispatcher::new(),
                actions: ledger().await,
                settings: AssistantSettings::default(),
            }
        }

        fn handling(mut self, handler: Arc<dyn ActionHandler>) -> Self {
            self.dispatcher.register(handler);
            self
        }

        fn speaking_through(mut self, speaker: RecordingSpeaker) -> Self {
            self.speaker = speaker;
            self
        }

        fn with_settings(mut self, settings: AssistantSettings) -> Self {
            self.settings = settings;
            self
        }

        fn deps(&self) -> TurnDeps<'_> {
            TurnDeps {
                surface: &self.surface,
                cancel: &self.cancel,
                offline: &self.offline,
                offline_opts: SttOpts::default(),
                llm: &self.llm,
                llm_binding: binding(),
                registry: ActionRegistry::default(),
                dispatcher: &self.dispatcher,
                actions: &self.actions,
                speaker: &self.speaker,
                settings: self.settings.clone(),
            }
        }

        async fn turn(&self, session: &mut Session, capture: Capture) -> Result<bool, String> {
            run_turn(&self.deps(), session, capture).await
        }

        /// The answer text as the surface received it.
        fn displayed(&self) -> Vec<String> {
            self.recording
                .answers()
                .into_iter()
                .filter_map(|s| match s {
                    Shown::Answer { text, .. } => Some(text),
                    _ => None,
                })
                .collect()
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    async fn ledger() -> ActionRepo {
        let pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&pool).await.unwrap();
        ActionRepo::new(pool)
    }

    // -- capture ----------------------------------------------------------

    /// Task 6.2's stated verification: the canned buffer a `ReplayAudioIo`
    /// hands back is captured, transcribed, routed and answered — end to end,
    /// with no microphone anywhere in it.
    #[tokio::test]
    async fn a_canned_buffer_is_captured_and_answered_as_a_request() {
        let h = Harness::new("what is the capital of france", answers_with("Paris.")).await;
        let mut audio = crate::commands::ReplayAudioIo::new(spoken_pcm(16_000, 2.0));

        let capture = capture_turn(&mut audio, None, &h.cancel, &h.surface)
            .await
            .expect("a replayed buffer captures");

        let asked = h
            .turn(&mut Session::new(), capture)
            .await
            .expect("the request completes");

        assert!(asked, "a buffer with speech in it is a request");
        assert_eq!(
            h.recording.answers(),
            vec![Shown::Answer {
                request: "what is the capital of france".into(),
                text: "Paris.".into(),
                speech_error: None,
            }]
        );
    }

    /// Task 6.2: the point of streaming the frames at all is that the user can
    /// see whether they were heard *before* the request is acted on.
    #[tokio::test]
    async fn live_hypotheses_reach_the_surface_while_the_user_is_still_speaking() {
        let h = Harness::new("unused", answers_with("unused")).await;
        let mut audio = FramesAudio::new(frames(16_000, 200, 2000), 16_000);
        let engine = ScriptedStreaming {
            hypotheses: vec!["what".into(), "what time".into(), "what time is it".into()],
        };

        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                Some(LiveRecognizer {
                    engine: &engine,
                    opts: SttOpts::default(),
                }),
                &h.cancel,
                &h.surface,
            ),
        )
        .await
        .expect("capture ends on the pause")
        .expect("capture succeeds");

        assert_eq!(
            h.recording.partials(),
            vec!["what", "what time", "what time is it"],
            "every hypothesis the recogniser produced is shown as it arrives"
        );
        assert_eq!(
            capture.streaming_text.as_deref(),
            Some("what time is it"),
            "the final hypothesis is what routing will run on"
        );
    }

    /// Task 6.3. The device never closes its channel here, so the only thing
    /// that can end this capture is the pause — and the proof is that the
    /// speech *after* the pause was never delivered.
    #[tokio::test]
    async fn a_sustained_pause_ends_the_request() {
        let rate = 16_000;
        let h = Harness::new("unused", answers_with("unused")).await;
        let mut script = frames(rate, 600, 2000);
        script.extend(frames(rate, 600, 0));
        let mut audio = FramesAudio::new(script, rate);

        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(&mut audio, None, &h.cancel, &h.surface),
        )
        .await
        .expect("the pause ends the capture rather than the script running out")
        .expect("capture succeeds");

        let secs = capture.pcm.samples.len() as f32 / rate as f32;
        assert!(
            secs < 3.0,
            "capture should stop during the pause, not read the speech after it; got {secs}s"
        );
        assert!(
            secs > 1.0,
            "and it should not stop before the speech finished; got {secs}s"
        );
    }

    /// Task 6.3's hard requirement. An empty request must never become a
    /// prompt, so the check is on the engines being untouched rather than on
    /// what the session displayed.
    #[tokio::test]
    async fn a_session_with_no_speech_at_all_never_reaches_the_language_model() {
        let h = Harness::new(
            "should never be asked",
            answers_with("should never be asked"),
        )
        .await;

        let asked = h
            .turn(
                &mut Session::new(),
                capture_of(silent_pcm(16_000, 3.0), None),
            )
            .await
            .expect("a silent turn is not an error");

        assert!(!asked, "a silent turn closes the session");
        assert!(h.prompts().is_empty(), "no prompt was built");
        assert_eq!(
            h.offline_calls.get(),
            0,
            "and nothing was transcribed either"
        );
        assert!(
            h.displayed().is_empty(),
            "and the user is shown no answer to a question they did not ask"
        );
    }

    /// Task 6.4. Live partials are an enhancement, never a dependency: with
    /// the streaming model absent the request still captures and still
    /// completes, and the surface simply shows no partial text.
    #[tokio::test]
    async fn a_missing_streaming_model_costs_the_partials_and_nothing_else() {
        let h = Harness::new("what time is it", answers_with("Ten past four.")).await;
        let mut audio = FramesAudio::new(frames(16_000, 600, 2000), 16_000);

        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                Some(LiveRecognizer {
                    engine: &NotInstalled,
                    opts: SttOpts::default(),
                }),
                &h.cancel,
                &h.surface,
            ),
        )
        .await
        .expect("capture still endpoints")
        .expect("capture still succeeds");

        assert!(
            h.recording.partials().is_empty(),
            "nothing recognised means nothing shown"
        );
        assert_eq!(capture.streaming_text, None);

        h.turn(&mut Session::new(), capture)
            .await
            .expect("the request completes without a live recogniser");

        assert_eq!(
            h.offline_calls.get(),
            1,
            "the offline engine carries the whole request when nothing streamed"
        );
        assert_eq!(
            h.recording.answers(),
            vec![Shown::Answer {
                request: "what time is it".into(),
                text: "Ten past four.".into(),
                speech_error: None,
            }]
        );
    }

    // -- the two-pass rule ------------------------------------------------

    /// Task 6.5, the cheap direction. This is the case the whole
    /// `from_speech` flag exists for: an action with nothing lifted out of
    /// speech must not pay for a complete re-decode.
    #[tokio::test]
    async fn an_action_with_no_spoken_arguments_never_invokes_the_offline_engine() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let h = Harness::new("should never be asked", routes_to_read_focused)
            .await
            .handling(Arc::new(RecordingHandler {
                id: READ_FOCUSED.id,
                text: "a login form",
                seen: seen.clone(),
            }));

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("read this")),
        )
        .await
        .expect("the action runs");

        assert_eq!(seen.lock().unwrap().len(), 1, "the action ran");
        assert_eq!(
            h.offline_calls.get(),
            0,
            "and it resolved without waiting for the accurate transcript"
        );
    }

    /// Task 6.5, the expensive direction, and the reason it is worth paying:
    /// the live hypothesis routes correctly and still names the wrong
    /// application. Intent survives word error; proper nouns do not.
    #[tokio::test]
    async fn an_action_with_spoken_arguments_takes_their_values_from_the_offline_transcript() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let h = Harness::new("open safari", |utterance: &str| {
            let app = utterance.rsplit(' ').next().unwrap_or_default();
            format!(r#"{{"action": "open_app", "args": {{"app": "{app}"}}}}"#)
        })
        .await
        .handling(Arc::new(RecordingHandler {
            id: OPEN_APP.id,
            text: "Opened it.",
            seen: seen.clone(),
        }));

        h.turn(
            &mut Session::new(),
            // What the live recogniser heard: right verb, wrong proper noun.
            capture_of(spoken_pcm(16_000, 2.0), Some("open sarari")),
        )
        .await
        .expect("the action runs");

        assert_eq!(
            seen.lock().unwrap()[0].text("app"),
            Some("safari"),
            "the argument value comes from the accurate transcript, not the live one"
        );
        assert_eq!(
            h.offline_calls.get(),
            1,
            "one re-decode, not one per argument"
        );
        assert_eq!(
            h.prompts().len(),
            2,
            "the action came from the live hypothesis and its arguments from the accurate one"
        );
    }

    /// The second pass re-reads arguments; it does not get a vote on the
    /// action. A router that changes its mind on the better transcript would
    /// otherwise swap the action out from under a user who has already been
    /// shown the first decision — and the swap is invisible whenever the two
    /// actions happen to share an argument name, which is exactly the case
    /// built here.
    #[tokio::test]
    async fn a_second_pass_that_names_a_different_action_leaves_the_first_ones_arguments_alone() {
        let opened = Arc::new(Mutex::new(Vec::new()));
        let read = Arc::new(Mutex::new(Vec::new()));
        let h = Harness::new("read this instead", |utterance: &str| {
            if utterance.contains("sarari") {
                r#"{"action": "open_app", "args": {"app": "sarari"}}"#.to_string()
            } else {
                // A different action, carrying an argument that would validate
                // perfectly well against the first one.
                r#"{"action": "read_focused", "args": {"app": "something else"}}"#.to_string()
            }
        })
        .await
        .handling(Arc::new(RecordingHandler {
            id: OPEN_APP.id,
            text: "Opened it.",
            seen: opened.clone(),
        }))
        .handling(Arc::new(RecordingHandler {
            id: READ_FOCUSED.id,
            text: "a login form",
            seen: read.clone(),
        }));

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("open sarari")),
        )
        .await
        .expect("the action the live hypothesis chose still runs");

        assert!(
            read.lock().unwrap().is_empty(),
            "the second pass ran nothing"
        );
        assert_eq!(
            opened.lock().unwrap()[0].text("app"),
            Some("sarari"),
            "a second opinion about the action is not a second opinion about its arguments"
        );
    }

    // -- follow-ups -------------------------------------------------------

    /// Task 6.7. The follow-up is only worth having if it knows what was
    /// already said, so the assertion is on the prompt the second turn built.
    #[tokio::test]
    async fn a_follow_up_turn_carries_the_earlier_turns_as_context() {
        let h = Harness::new("unused", answers_with("Paris.")).await;
        let mut session = Session::new();

        h.turn(
            &mut session,
            capture_of(
                spoken_pcm(16_000, 2.0),
                Some("what is the capital of france"),
            ),
        )
        .await
        .unwrap();
        session.apply(SessionEvent::FollowUpStarted);
        h.turn(
            &mut session,
            capture_of(spoken_pcm(16_000, 2.0), Some("how many people live there")),
        )
        .await
        .unwrap();

        let prompts = h.prompts();
        assert!(
            !prompts[0].contains("Earlier in this conversation"),
            "the first turn of a session has no history to carry"
        );
        assert!(
            prompts[1].contains("what is the capital of france") && prompts[1].contains("Paris."),
            "the follow-up carries both halves of the completed turn"
        );
    }

    /// Task 6.7's other half. History lives on the session value, so closing
    /// the session is what discards it — a later session starts from nothing
    /// because there is nowhere for the previous one's turns to have been kept.
    #[tokio::test]
    async fn a_later_session_cannot_see_an_earlier_sessions_turns() {
        let h = Harness::new("unused", answers_with("Paris.")).await;

        let mut first = Session::new();
        h.turn(
            &mut first,
            capture_of(
                spoken_pcm(16_000, 2.0),
                Some("what is the capital of france"),
            ),
        )
        .await
        .unwrap();
        drop(first);

        let mut second = Session::new();
        h.turn(
            &mut second,
            capture_of(spoken_pcm(16_000, 2.0), Some("how many people live there")),
        )
        .await
        .unwrap();

        assert!(
            !h.prompts()[1].contains("Earlier in this conversation"),
            "a new session starts with no history"
        );
    }

    // -- speaking ---------------------------------------------------------

    /// Task 6.8. The design forbids a short spoken version diverging from a
    /// longer written one, so this asserts identity rather than similarity.
    #[tokio::test]
    async fn the_spoken_text_is_exactly_the_text_that_was_displayed() {
        let h = Harness::new("unused", answers_with("Paris, on the Seine.")).await;

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .unwrap();

        assert_eq!(h.speaker.said(), h.displayed());
    }

    /// Task 6.10. Speech is the default, never a prerequisite.
    #[tokio::test]
    async fn an_answer_that_cannot_be_spoken_is_still_shown_and_the_failure_reported() {
        let h = Harness::new("unused", answers_with("Paris."))
            .await
            .speaking_through(RecordingSpeaker::broken(
                "no tts engine is bound for assistant",
            ));
        let mut session = Session::new();

        h.turn(
            &mut session,
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .expect("an unspeakable answer is not a failed request");

        let answers = h.recording.answers();
        assert!(
            matches!(&answers[0], Shown::Answer { text, speech_error: None, .. } if text == "Paris."),
            "the answer reaches the screen before speech is even attempted"
        );
        assert!(
            matches!(&answers[1], Shown::Answer { text, speech_error: Some(e), .. }
                if text == "Paris." && e.contains("no tts engine")),
            "and the failure is reported alongside it, not instead of it"
        );
        assert_eq!(
            session.state(),
            &SessionState::Presenting { speaking: false }
        );
    }

    /// The `speak_answers` switch. Off means no synthesis at all, not
    /// synthesis into a muted player: the whole cost of the voice is what the
    /// user turned off.
    #[tokio::test]
    async fn speaking_switched_off_synthesizes_nothing_at_all() {
        let h = Harness::new("unused", answers_with("Paris."))
            .await
            .with_settings(AssistantSettings {
                speak_answers: false,
                show_answers: true,
            });
        let mut session = Session::new();

        h.turn(
            &mut session,
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .unwrap();

        assert!(h.speaker.said().is_empty(), "the voice was never asked");
        assert_eq!(h.displayed(), vec!["Paris."], "the answer is still shown");
        assert_eq!(
            session.state(),
            &SessionState::Presenting { speaking: false },
            "and no stop-speaking control is offered for audio that never played"
        );
    }

    // -- the ledger -------------------------------------------------------

    /// Task 3.3.
    #[tokio::test]
    async fn a_completed_action_is_recorded_against_its_action_id() {
        let h = Harness::new("unused", routes_to_read_focused)
            .await
            .handling(Arc::new(RecordingHandler {
                id: READ_FOCUSED.id,
                text: "a login form",
                seen: Arc::new(Mutex::new(Vec::new())),
            }));

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("read this")),
        )
        .await
        .unwrap();

        let rows = h.actions.recent(10).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, ASSISTANT_FEATURE_ID);
        assert_eq!(
            rows[0].command, "read_focused",
            "History reads the registry's vocabulary, not a per-feature synonym"
        );
        assert_eq!(rows[0].status, ActionStatus::Ok);
    }

    /// Task 3.3. An action that could not run is a fault, and is recorded as
    /// one so the user can see why nothing happened.
    #[tokio::test]
    async fn an_action_that_could_not_run_is_recorded_as_an_error() {
        let h = Harness::new("unused", routes_to_read_focused)
            .await
            .handling(Arc::new(FailingHandler {
                id: READ_FOCUSED.id,
            }));

        let error = h
            .turn(
                &mut Session::new(),
                capture_of(spoken_pcm(16_000, 2.0), Some("read this")),
            )
            .await
            .expect_err("a failed action fails the turn");

        assert!(
            error.contains("unavailable"),
            "the reason survives: {error}"
        );
        let rows = h.actions.recent(10).await.unwrap();
        assert_eq!(rows[0].status, ActionStatus::Error);
        assert!(
            h.displayed().is_empty(),
            "and the request is not reported as completed"
        );
    }

    /// Task 3.3's subtle case. Cancelling is the user's own choice, and a red
    /// row in History for a decision they made reads as a fault in the app.
    #[tokio::test]
    async fn a_session_cancelled_mid_action_is_recorded_as_cancelled_not_failed() {
        let cancel = Arc::new(AtomicBool::new(false));
        let h = Harness::new("unused", routes_to_read_focused)
            .await
            .handling(Arc::new(CancellingHandler {
                id: READ_FOCUSED.id,
                cancel: cancel.clone(),
            }));
        // The handler flips the session's own flag, which is what Escape does.
        let h = Harness { cancel, ..h };

        let _ = h
            .turn(
                &mut Session::new(),
                capture_of(spoken_pcm(16_000, 2.0), Some("read this")),
            )
            .await;

        let rows = h.actions.recent(10).await.unwrap();
        assert_eq!(
            rows[0].status,
            ActionStatus::Cancelled,
            "the user's own choice is not a failure"
        );
        assert!(
            h.displayed().is_empty(),
            "and a cancelled action presents nothing"
        );
    }
}
