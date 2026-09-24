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
use std::sync::{Arc, Mutex};

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
    ///
    /// `action` is the title of the action the request was routed to, and is
    /// `None` when the request was answered instead. It travels with the
    /// answer rather than on the state event because it is part of what the
    /// user is being shown, not part of where the session has got to: the
    /// routing spec requires that an invocation be distinguishable from an
    /// answer, and without it the two are the same event with different words
    /// in it.
    fn answer(
        &self,
        request: &str,
        text: &str,
        action: Option<&str>,
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

    /// Silence an answer that is still playing.
    ///
    /// **Three things stop an answer, and they are one method rather than
    /// three.** The user's stop control, cancelling the session (watched for
    /// in [`present`], because nothing else is looking at the flag while
    /// playback is awaited), and the next turn opening the microphone (see
    /// [`capture_turn`]). Each arrives from a different world — a command, a
    /// hotkey, this module's own loop — and the alternative, a stop path per
    /// caller, would give playback three ways to be left running and three
    /// places to get the "already finished" case wrong.
    ///
    /// Idempotent, and harmless when nothing is playing: every caller is
    /// reacting to something the user did, not to a belief about the audio.
    fn stop(&self);
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
    /// Where the answer on screen is kept for the copy binding to find.
    ///
    /// The surface cannot be asked for it: it is a webview in a click-through,
    /// non-focusable overlay, and the copy shortcut is handled on the hotkey
    /// thread, which has `AppState` and no way to interrogate a window. So the
    /// text is written here at the same moment it is shown, which is also what
    /// makes "the copied text is the displayed text" a property a test can
    /// state.
    answer_store: &'a Arc<Mutex<Option<String>>>,
    /// Raised while an answer is being read aloud, and lowered the moment it
    /// stops however it stopped.
    ///
    /// Read by the hotkey dispatch, which is what gives Escape its second
    /// meaning: silence the voice while one is playing, cancel the session
    /// otherwise. Held outside this module because the press arrives on the
    /// hotkey thread, which can reach `AppState` and nothing else.
    speaking: &'a Arc<AtomicBool>,
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
    crate::commands::set_assistant_copyable(state, true);
    // Escape and the copy key belong to the rest of the Mac again the moment
    // this returns, by every path including the early ones below.
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

    let speaker = TtsSpeaker::new(state.clone());
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
        answer_store: &state.assistant_answer,
        speaking: &state.assistant_speaking,
    };

    loop {
        surface.state(session.state());

        let capture = {
            let mut audio = state.audio.lock().await;
            let live = prepared.streaming.as_ref().map(|engine| LiveRecognizer {
                engine: engine.as_ref(),
                opts: prepared.streaming_opts.clone(),
            });
            capture_turn(
                &mut **audio,
                live,
                &state.assistant_cancel,
                &state.assistant_submit,
                &state.assistant_answer,
                &surface,
                deps.speaker,
            )
            .await
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

        // A cancel that landed while the microphone was open closes the
        // session here, as `Ok(false)` out of `run_turn` — the guard lives
        // there rather than being repeated at this call site, for the same
        // reason the empty-request guard does.
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
///
/// The cancel check is here for the same reason, and *before* the speech check
/// so that it holds whether or not the user got a word out: a session
/// cancelled while it was listening must send nothing, and "nothing" includes
/// the transcription of audio that was already captured.
async fn run_turn(
    deps: &TurnDeps<'_>,
    session: &mut Session,
    capture: Capture,
) -> Result<bool, String> {
    if cancelled(deps.cancel) {
        return Ok(false);
    }
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
    submit: &Arc<AtomicBool>,
    answer_store: &Arc<Mutex<Option<String>>>,
    surface: &Arc<dyn Surface>,
    speaker: &dyn Speaker,
) -> Result<Capture, String> {
    // Cleared on the way in, not on the way out, and for the same reason
    // `assistant_cancel` is: the activation key pressed as one turn ends
    // would otherwise end the follow-up before the user had said a word.
    submit.store(false, Ordering::Relaxed);

    // The previous answer stops being copyable at the same moment it stops
    // being on screen — the surface clears it on seeing `Listening`. Leaving
    // it would make the copy key put an answer to a question the user has
    // already moved on from onto their clipboard, with nothing visible to say
    // which answer they got.
    *answer_store.lock().unwrap_or_else(|p| p.into_inner()) = None;

    // The third of the stop paths on [`Speaker::stop`]: a turn that opens the
    // microphone over the tail of the previous answer records the assistant's
    // own voice and hands it to the recogniser as part of the follow-up. The
    // ordering is the whole of it — stopping after `start_mic` would already
    // have let that audio in.
    //
    // Usually there is nothing to stop, because [`present`] awaits playback.
    // Usually is not always: playback runs on a blocking thread, and dropping
    // the future that awaited it does not stop the thread — only this flag
    // does.
    speaker.stop();

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
        // The explicit end of a request. Checked before the pause detector and
        // before the no-speech window, because those two are guesses about
        // when the user finished and this is the user saying so: a submit that
        // had to wait for `SPEECH_WAIT_SECS` to elapse would be a key press
        // that appears to do nothing for four seconds.
        if submit.load(Ordering::Relaxed) {
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
        RequestPlan::Answer { text } => present(deps, session, &utterance, text, None, None).await,

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
            // The action travels with the answer from here on: this is the one
            // branch where the user is being told that something *happened*
            // rather than answered, and the routing spec asks for exactly that
            // to be visible.
            present(
                deps,
                session,
                &utterance,
                spoken,
                disclosure,
                Some(spec.title),
            )
            .await
        }

        // Both of these are words, not failures: the assistant understood
        // enough to ask, and the session stays open for the answer.
        RequestPlan::Clarify { question } => {
            // No action: a question about which action was meant is not an
            // invocation of one, and labelling it with a candidate would say
            // the assistant had chosen when its whole point is that it has not.
            present(deps, session, &utterance, question, None, None).await
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
    action: Option<&str>,
) -> Result<(), String> {
    deps.surface
        .answer(request, &answer, action, disclosure.as_ref(), None);
    // Stored beside the emit, never anywhere else, so the text the copy key
    // yields cannot drift from the text on screen — which is the same reason
    // the design forbids a spoken answer that differs from the shown one.
    *deps.answer_store.lock().unwrap_or_else(|p| p.into_inner()) = Some(answer.clone());

    // **`Presenting` is emitted before playback, not after it.**
    //
    // Playback is awaited, so emitting afterwards means the only state the
    // surface ever sees is `speaking: false` — and the stop control, which
    // exists precisely for the seconds an answer is in the air, would never
    // appear in the running app. Reporting the *intent* to speak is what makes
    // it appear for exactly as long as there is audio to stop; the second emit
    // below takes it away again the moment there is not.
    let will_speak = deps.settings.speak_answers;
    session.apply(SessionEvent::Answered {
        text: answer.clone(),
        spoken: will_speak,
    });
    deps.surface.state(session.state());

    if will_speak {
        deps.speaking.store(true, Ordering::Relaxed);
        let result = speak_until_cancelled(deps, &answer).await;
        // Lowered before anything else can await, so Escape stops meaning
        // "silence this" the instant there is nothing left to silence.
        deps.speaking.store(false, Ordering::Relaxed);
        if let Err(error) = result {
            // Speech is attempted, never required. The answer stays on screen
            // and the failure is re-emitted alongside it, rather than becoming
            // a session failure that replaces the answer the user can still
            // read.
            tracing::warn!(%error, "assistant: the answer could not be spoken");
            deps.surface
                .answer(request, &answer, action, disclosure.as_ref(), Some(&error));
        }
    }

    session.apply(SessionEvent::SpeechFinished);
    deps.surface.state(session.state());
    Ok(())
}

/// Speak `text`, and stop it if the session is cancelled while it plays.
///
/// The second of the stop paths on [`Speaker::stop`]. Speaking is awaited, so
/// for the length of an answer this module is inside a single `.await` and
/// nothing is watching the cancel flag — and "during the answer" is precisely
/// when a cancel lands, because the answer is the part the user is sitting
/// through. So the wait is interleaved with a poll of the flag.
///
/// A poll rather than a `watch` channel selected over: the flag is an
/// `AtomicBool` shared with a synchronous playback thread and a capture loop
/// (see `AppState::assistant_cancel`), and giving it a second, async spelling
/// purely for this one waiter would mean two signals to set and one of them to
/// forget. The cost is one timer per 50 ms of speech.
async fn speak_until_cancelled(deps: &TurnDeps<'_>, text: &str) -> Result<(), String> {
    let speaking = deps.speaker.speak(text);
    tokio::pin!(speaking);
    loop {
        tokio::select! {
            // Biased so a completed answer is never reported as a stop it
            // raced: when both are ready, finishing wins.
            biased;
            result = &mut speaking => return result,
            _ = tokio::time::sleep(SPEECH_CANCEL_POLL) => {
                if cancelled(deps.cancel) {
                    deps.speaker.stop();
                }
            }
        }
    }
}

/// How often a playing answer looks at the session's cancel flag.
///
/// The same 50 ms `play_pcm_cancellable` already wakes on, so a stop costs at
/// most one extra tick beyond what playback itself would take to notice. A
/// quarter of a second would be cheaper and would read as Escape having been
/// ignored, which is the specific complaint cancellation exists to answer.
const SPEECH_CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(50);

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
        action: Option<&str>,
        disclosure: Option<&Disclosure>,
        speech_error: Option<&str>,
    ) {
        events::emit_assistant_answer(&self.app, request, text, action, disclosure, speech_error);
    }
}

/// Speaking through the user's bound TTS slot and the shared playback path.
struct TtsSpeaker {
    state: Arc<AppState>,
    /// Set while an answer should stop playing.
    ///
    /// Its own flag rather than a share of `assistant_cancel`, although the
    /// session's cancel is one of the things that sets it (via
    /// [`speak_until_cancelled`]): stopping an answer leaves the session open
    /// with its answer on screen, and cancelling closes it. One flag for both
    /// would make the microphone-opening stop in [`capture_turn`] cancel every
    /// follow-up the instant it started.
    ///
    /// Taken from `AppState` rather than minted here, because the user's own
    /// stop arrives as a hotkey press: a flag private to this struct could
    /// only ever be set by this module, which is every caller except the one
    /// the control exists for.
    stop: Arc<AtomicBool>,
}

impl TtsSpeaker {
    fn new(state: Arc<AppState>) -> Self {
        let stop = state.assistant_speech_stop.clone();
        Self { state, stop }
    }
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

        // Read and cleared in one step, on the way into playback rather than
        // on the way out: a stop that arrived while the answer was still being
        // synthesized has to silence *this* answer, and a flag cleared at the
        // top of `speak` would have thrown that stop away and played it
        // anyway. Cleared at all because the flag outlives one answer — the
        // next turn's stop in `capture_turn` sets it before any of this runs.
        if self.stop.swap(false, Ordering::Relaxed) || cancelled(&self.state.assistant_cancel) {
            return Ok(());
        }

        let stop = self.stop.clone();
        tokio::task::spawn_blocking(move || {
            kea_platform::audio::playback::play_pcm_cancellable(&pcm, &stop)
        })
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())
    }

    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
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

/// Releases the session's two bindings when it ends, however it ends.
///
/// A guard rather than a call at each return: `run_session` has several early
/// exits and more will be added, and the one that forgets is the one that
/// leaves Escape — and `Cmd+Shift+C`, which other applications bind to real
/// commands — captured from every other application on the Mac until KEA is
/// restarted.
///
/// It also drops the answer, so the copy key cannot be handed a stale one by
/// a session that starts before the user notices the last one ended.
struct EscapeGuard {
    state: Arc<AppState>,
}

impl Drop for EscapeGuard {
    fn drop(&mut self) {
        crate::commands::set_assistant_cancellable(&self.state, false);
        crate::commands::set_assistant_copyable(&self.state, false);
        // Both cleared here rather than at the next session's start, because
        // they are read by the hotkey thread: a `speaking` left set would give
        // Escape the wrong meaning between sessions, when there is no voice to
        // silence and a stale stop waiting to swallow the next answer.
        self.state
            .assistant_speaking
            .store(false, Ordering::Relaxed);
        self.state
            .assistant_speech_stop
            .store(false, Ordering::Relaxed);
        *self
            .state
            .assistant_answer
            .lock()
            .unwrap_or_else(|p| p.into_inner()) = None;
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
            /// The action's title, or `None` when the request was answered.
            action: Option<String>,
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
            action: Option<&str>,
            _disclosure: Option<&Disclosure>,
            speech_error: Option<&str>,
        ) {
            self.shown.lock().unwrap().push(Shown::Answer {
                request: request.to_string(),
                action: action.map(str::to_string),
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

    /// How the fake speaker's playback behaves in time.
    ///
    /// A stop is only testable against a speaker that has something left to
    /// stop, so the two non-instant shapes here are not embellishment: they
    /// are the two ways the real speaker can still be playing when something
    /// asks it to stop.
    enum Playback {
        /// Returns as soon as the text has been "spoken" — enough for every
        /// test that is about what was said rather than when it ended.
        Instant,
        /// Does not return until [`Speaker::stop`] is called, the way
        /// `play_pcm_cancellable` does not return until the sink drains or its
        /// flag flips.
        ///
        /// It watches the stop and *nothing else* — in particular not the
        /// session's cancel flag — because that is what the real speaker does:
        /// playback polls the speaker's own flag, and a cancel reaches it only
        /// by being turned into a stop. A fake that also watched the cancel
        /// would end the answer by itself and leave the code that translates
        /// one into the other untested.
        UntilStopped,
        /// Returns while the audio is still playing. Not a contrivance: real
        /// playback is a blocking thread, and dropping the future that awaited
        /// it leaves the thread running with only the stop flag to end it.
        Detached,
    }

    /// A speaker that records what it was asked to say, can refuse, and can be
    /// caught mid-answer.
    struct RecordingSpeaker {
        said: Mutex<Vec<String>>,
        fails_with: Option<String>,
        playback: Playback,
        /// True between the start of an answer and whatever ended it.
        playing: Arc<AtomicBool>,
        stopped: Arc<AtomicBool>,
        timeline: Timeline,
    }

    impl RecordingSpeaker {
        fn working() -> Self {
            Self {
                said: Mutex::new(Vec::new()),
                fails_with: None,
                playback: Playback::Instant,
                playing: Arc::new(AtomicBool::new(false)),
                stopped: Arc::new(AtomicBool::new(false)),
                timeline: Timeline::default(),
            }
        }
        fn broken(message: &str) -> Self {
            Self {
                fails_with: Some(message.to_string()),
                ..Self::working()
            }
        }
        /// Plays until something stops it.
        fn playing_until_stopped() -> Self {
            Self {
                playback: Playback::UntilStopped,
                ..Self::working()
            }
        }
        /// Hands back control while the answer is still audible.
        fn leaving_playback_running() -> Self {
            Self {
                playback: Playback::Detached,
                ..Self::working()
            }
        }
        fn noting(mut self, timeline: Timeline) -> Self {
            self.timeline = timeline;
            self
        }
        fn said(&self) -> Vec<String> {
            self.said.lock().unwrap().clone()
        }
        fn is_playing(&self) -> bool {
            self.playing.load(Ordering::SeqCst)
        }
        /// Whether the stop control was used, as opposed to the answer having
        /// ended some other way.
        fn was_stopped(&self) -> bool {
            self.stopped.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Speaker for RecordingSpeaker {
        async fn speak(&self, text: &str) -> Result<(), String> {
            self.said.lock().unwrap().push(text.to_string());
            self.timeline.note("answer started");
            match &self.playback {
                Playback::Instant => {}
                Playback::Detached => self.playing.store(true, Ordering::SeqCst),
                Playback::UntilStopped => {
                    self.playing.store(true, Ordering::SeqCst);
                    while !self.stopped.load(Ordering::SeqCst) {
                        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                    }
                    self.playing.store(false, Ordering::SeqCst);
                }
            }
            match &self.fails_with {
                Some(message) => Err(message.clone()),
                None => Ok(()),
            }
        }

        fn stop(&self) {
            self.stopped.store(true, Ordering::SeqCst);
            if self.playing.swap(false, Ordering::SeqCst) {
                self.timeline.note("answer stopped");
            }
        }
    }

    /// What happened across two fakes, in the order it happened.
    ///
    /// Shared by the speaker and the capture device because the follow-up stop
    /// is entirely a claim about ordering: stopping playback *after* the
    /// microphone opened would have recorded the tail of the last answer into
    /// the next request, and a test that only checked "it was stopped" would
    /// pass on exactly that bug.
    #[derive(Clone, Default)]
    struct Timeline(Arc<Mutex<Vec<&'static str>>>);

    impl Timeline {
        fn note(&self, what: &'static str) {
            self.0.lock().unwrap().push(what);
        }
        fn events(&self) -> Vec<&'static str> {
            self.0.lock().unwrap().clone()
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
        timeline: Timeline,
        /// Flags to raise just before a given frame is handed over, each one
        /// standing in for a key press landing mid-question.
        raise_at: Vec<(usize, Arc<AtomicBool>)>,
    }

    impl FramesAudio {
        fn new(frames: Vec<PcmFrame>, rate: u32) -> Self {
            Self {
                frames,
                rate,
                delivered: Arc::new(Mutex::new(Vec::new())),
                state: DictationState::Idle,
                timeline: Timeline::default(),
                raise_at: Vec::new(),
            }
        }

        fn noting(mut self, timeline: Timeline) -> Self {
            self.timeline = timeline;
            self
        }

        /// Raise `cancel` just before the `nth` frame is handed over — the
        /// user pressing Escape part-way through their own question.
        ///
        /// Tied to the frame count rather than to a sleep because this device
        /// delivers as fast as the session reads: a wall-clock cancel would
        /// land after the whole script had already been consumed, and the test
        /// would be about running out of audio instead.
        fn cancelling_at(mut self, nth: usize, cancel: Arc<AtomicBool>) -> Self {
            self.raise_at.push((nth, cancel));
            self
        }

        /// Raise `submit` just before the `nth` frame — the user pressing the
        /// activation key again to say they have finished asking.
        ///
        /// Frame-counted for the same reason `cancelling_at` is: this device
        /// delivers as fast as the session reads, so a wall-clock submit would
        /// land after the script had run out and the test would be measuring
        /// the end of the audio instead.
        fn submitting_at(mut self, nth: usize, submit: Arc<AtomicBool>) -> Self {
            self.raise_at.push((nth, submit));
            self
        }
    }

    #[async_trait]
    impl AudioIo for FramesAudio {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.state = DictationState::Listening;
            self.timeline.note("microphone opened");
            let (tx, rx) = tokio::sync::mpsc::channel(1);
            let frames = self.frames.clone();
            let delivered = self.delivered.clone();
            let raise_at = self.raise_at.clone();
            tokio::spawn(async move {
                for (index, frame) in frames.into_iter().enumerate() {
                    for (nth, flag) in &raise_at {
                        if index == *nth {
                            flag.store(true, Ordering::Relaxed);
                        }
                    }
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
        /// The explicit-submit flag, raised by a second press of the
        /// activation key while the microphone is open.
        submit: Arc<AtomicBool>,
        /// Where `present` leaves the answer for the copy binding.
        answer_store: Arc<Mutex<Option<String>>>,
        /// Raised while an answer plays, which is what decides whether Escape
        /// silences the voice or ends the session.
        speaking: Arc<AtomicBool>,
        offline: CountingStt,
        offline_calls: Arc<AtomicUsizeCell>,
        llm: ScriptedLlm,
        prompts: Arc<Mutex<Vec<String>>>,
        /// Shared rather than owned: a stop arrives from outside the turn, so
        /// a test has to hold the speaker while `run_turn` is still borrowing
        /// it.
        speaker: Arc<RecordingSpeaker>,
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
                submit: Arc::new(AtomicBool::new(false)),
                answer_store: Arc::new(Mutex::new(None)),
                speaking: Arc::new(AtomicBool::new(false)),
                offline,
                offline_calls,
                llm,
                prompts,
                speaker: Arc::new(RecordingSpeaker::working()),
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
            self.speaker = Arc::new(speaker);
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
                speaker: &*self.speaker,
                settings: self.settings.clone(),
                answer_store: &self.answer_store,
                speaking: &self.speaking,
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

        let capture = capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            )
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
                action: None,
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
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
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
            capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            ),
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

    /// Task 6.3's explicit-submit path: the user has finished asking and says
    /// so, rather than waiting out a pause they did not intend.
    ///
    /// The script is unbroken speech with no qualifying pause in it, so the
    /// pause detector cannot end this capture and `MAX_CAPTURE_SECS` is
    /// twenty-five seconds away. Delete the submit check in `capture_turn` and
    /// this test reads the whole twelve-second script instead of the first
    /// second of it — it cannot pass by accident.
    #[tokio::test]
    async fn an_explicit_submit_ends_the_request_before_the_pause_would_have() {
        let rate = 16_000;
        let h = Harness::new("unused", answers_with("unused")).await;
        let mut audio = FramesAudio::new(frames(rate, 12_000, 0), rate)
            // 20ms frames, so this is the key pressed a second in.
            .submitting_at(50, h.submit.clone());

        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            ),
        )
        .await
        .expect("the submit ends the capture rather than the script running out")
        .expect("capture succeeds");

        let secs = capture.pcm.samples.len() as f32 / rate as f32;
        assert!(
            secs < 3.0,
            "the request ended where the user said it did, not at the hard cap; got {secs}s"
        );
        assert!(
            contains_speech(&capture.pcm),
            "and what they had already said is what gets answered — a submit is \
             not a cancel"
        );
    }

    /// A submit is per-turn, not per-session. Left standing it would end the
    /// follow-up the instant the microphone opened, before the user had said a
    /// word — the same race the session's cancel flag is cleared on the way in
    /// to avoid.
    #[tokio::test]
    async fn a_submit_does_not_survive_into_the_next_turn() {
        let rate = 16_000;
        let h = Harness::new("unused", answers_with("unused")).await;
        h.submit.store(true, Ordering::Relaxed);
        let mut script = frames(rate, 600, 2000);
        script.extend(frames(rate, 600, 0));
        let mut audio = FramesAudio::new(script, rate);

        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            ),
        )
        .await
        .expect("capture ends")
        .expect("capture succeeds");

        // A stale submit would break out on the first frame, so this is the
        // assertion that tells the two apart: 20ms of audio against the
        // second and a bit that ends at the pause.
        let secs = capture.pcm.samples.len() as f32 / rate as f32;
        assert!(
            secs > 1.0,
            "the stale submit was cleared, so this turn ran to its own endpoint; got {secs}s"
        );
        assert!(
            contains_speech(&capture.pcm),
            "and the user's question is in the buffer to be answered"
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
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
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
                action: None,
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

    /// Task 7.4's stop-speaking control, at the only point that can make it
    /// appear.
    ///
    /// Playback is awaited, so a `Presenting` emitted *after* it would only
    /// ever say `speaking: false` — and the control, which exists for exactly
    /// the seconds an answer is in the air, would never reach the running app
    /// while every component test of it went on passing against a payload no
    /// backend sent. That is the shape this asserts against: a
    /// `speaking: true` state reaching the surface, and a `speaking: false`
    /// one after it.
    #[tokio::test]
    async fn the_surface_is_told_an_answer_is_playing_while_it_is_still_playing() {
        let h = Harness::new("unused", answers_with("Paris.")).await;

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .expect("the turn answers");

        let states: Vec<SessionState> = h
            .recording
            .shown()
            .into_iter()
            .filter_map(|s| match s {
                Shown::State(state) => Some(state),
                _ => None,
            })
            .collect();

        assert!(
            states.contains(&SessionState::Presenting { speaking: true }),
            "the stop control has to be offered while there is audio to stop; got {states:?}"
        );
        assert_eq!(
            states.last(),
            Some(&SessionState::Presenting { speaking: false }),
            "and taken away again the moment there is not"
        );
    }

    /// The flag Escape reads to decide which of its two meanings applies. Left
    /// raised it would make every press after an answer silence a voice that
    /// has already stopped, and the session would become uncloseable.
    #[tokio::test]
    async fn the_playing_flag_is_lowered_once_the_answer_has_finished() {
        let h = Harness::new("unused", answers_with("Paris.")).await;

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .expect("the turn answers");

        assert!(!h.speaking.load(Ordering::Relaxed));
    }

    /// With speaking switched off there is nothing to stop, so the flag is
    /// never raised at all — Escape keeps its single meaning for a user who
    /// turned the voice off.
    #[tokio::test]
    async fn a_silent_answer_never_claims_to_be_playing() {
        let h = Harness::new("unused", answers_with("Paris."))
            .await
            .with_settings(AssistantSettings {
                speak_answers: false,
                show_answers: true,
            });

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .expect("the turn answers");

        let states: Vec<SessionState> = h
            .recording
            .shown()
            .into_iter()
            .filter_map(|s| match s {
                Shown::State(state) => Some(state),
                _ => None,
            })
            .collect();
        assert!(
            !states.contains(&SessionState::Presenting { speaking: true }),
            "no stop control for audio that never played; got {states:?}"
        );
        assert!(!h.speaking.load(Ordering::Relaxed));
    }

    // -- keeping the answer -----------------------------------------------

    /// Task 7.4's copy control. The surface is a click-through, non-focusable
    /// overlay, so the only control it can offer is a key — and the key is
    /// handled on the hotkey thread, which reaches the answer through this
    /// store and nothing else.
    ///
    /// The assertion is the *identity*, not merely that something was stored:
    /// the design's rule that the spoken text is the displayed text is worth
    /// nothing if the copied text is a third version.
    #[tokio::test]
    async fn the_answer_left_for_the_copy_key_is_exactly_the_answer_on_screen() {
        let h = Harness::new("what is the capital of france", answers_with("Paris.")).await;

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), None),
        )
        .await
        .expect("the turn answers");

        assert_eq!(h.displayed(), vec!["Paris."]);
        assert_eq!(
            h.answer_store.lock().unwrap().as_deref(),
            Some("Paris."),
            "the copy key yields the text the user is looking at"
        );
    }

    /// The answer stops being copyable at the moment it stops being on screen.
    ///
    /// The surface clears the answer when it sees `Listening`, so a store left
    /// standing would put an answer to a question the user has already moved
    /// on from onto their clipboard, with nothing visible to say which one
    /// they got.
    #[tokio::test]
    async fn the_next_turn_takes_the_previous_answer_out_of_reach_of_the_copy_key() {
        let rate = 16_000;
        let h = Harness::new("unused", answers_with("Paris.")).await;
        *h.answer_store.lock().unwrap() = Some("Paris.".into());
        let mut audio = FramesAudio::new(frames(rate, 600, 2000), rate);

        let _ = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            ),
        )
        .await
        .expect("capture ends on the pause")
        .expect("capture succeeds");

        assert_eq!(
            h.answer_store.lock().unwrap().as_deref(),
            None,
            "nothing on screen, nothing to copy"
        );
    }

    // -- cancellation -----------------------------------------------------

    /// Task 6.6, cancelling while the assistant is listening. The audio was
    /// really captured — the microphone was open and the user was talking —
    /// and the claim is that none of it goes anywhere: not to the transcriber,
    /// not to the router, not to the screen.
    #[tokio::test]
    async fn cancelling_while_listening_discards_the_captured_audio_and_sends_nothing() {
        let h = Harness::new(
            "should never be asked",
            answers_with("should never be asked"),
        )
        .await;
        // Escape a quarter of a second in, with the user still mid-question.
        let mut audio =
            FramesAudio::new(frames(16_000, 3000, 0), 16_000).cancelling_at(12, h.cancel.clone());

        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            ),
        )
        .await
        .expect("the cancel ends the capture")
        .expect("capture succeeds");

        assert!(
            contains_speech(&capture.pcm),
            "the microphone did record the user speaking — the point is what happens to it next"
        );

        let asked = h
            .turn(&mut Session::new(), capture)
            .await
            .expect("cancelling is the user's own choice, not a failure");

        assert!(!asked, "the session closes");
        assert!(h.prompts().is_empty(), "the request was never routed");
        assert_eq!(
            h.offline_calls.get(),
            0,
            "and the audio was never even transcribed"
        );
        assert!(h.displayed().is_empty());
        assert!(
            !h.recording
                .shown()
                .iter()
                .any(|s| matches!(s, Shown::State(SessionState::Failed { .. }))),
            "an error banner for the user's own Escape reads as a bug"
        );
    }

    /// Task 6.6, cancelling while the request is being processed. The router
    /// has already answered by the time the cancel lands, so the thing under
    /// test is the gate in front of dispatch: an action that had not begun
    /// must not begin now.
    ///
    /// Its opposite number is
    /// `a_session_cancelled_mid_action_is_recorded_as_cancelled_not_failed`,
    /// where the action *had* begun and is recorded as cancelled. Here there
    /// is no row at all, because nothing ran.
    #[tokio::test]
    async fn cancelling_while_processing_runs_no_action_that_had_not_begun() {
        let cancel = Arc::new(AtomicBool::new(false));
        let escape = cancel.clone();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let h = Harness::new("unused", move |_utterance: &str| {
            // Escape, arriving while the router was still thinking.
            escape.store(true, Ordering::Relaxed);
            r#"{"action": "read_focused"}"#.to_string()
        })
        .await
        .handling(Arc::new(RecordingHandler {
            id: READ_FOCUSED.id,
            text: "a login form",
            seen: seen.clone(),
        }));
        let h = Harness { cancel, ..h };

        h.turn(
            &mut Session::new(),
            capture_of(spoken_pcm(16_000, 2.0), Some("read this")),
        )
        .await
        .expect("cancelling is the user's own choice, not a failure");

        assert!(
            seen.lock().unwrap().is_empty(),
            "the action never ran, so nothing happened outside this process"
        );
        assert!(
            h.actions.recent(10).await.unwrap().is_empty(),
            "and nothing is recorded, because there is nothing that happened to record"
        );
        assert!(h.displayed().is_empty());
        assert!(
            !h.recording
                .shown()
                .iter()
                .any(|s| matches!(s, Shown::State(SessionState::Failed { .. }))),
            "an error banner for the user's own Escape reads as a bug"
        );
    }

    // -- stopping an answer -----------------------------------------------

    /// Task 6.9, the first of the three stop paths on [`Speaker::stop`]: the
    /// user stops the answer themselves. Stopping the voice is not cancelling
    /// the session — the text stays up and the session stays open — which is
    /// the distinction the whole separate flag exists for.
    #[tokio::test]
    async fn stopping_a_playing_answer_ends_the_speech_and_leaves_the_answer_on_screen() {
        let h = Harness::new("unused", answers_with("Paris, on the Seine."))
            .await
            .speaking_through(RecordingSpeaker::playing_until_stopped());
        let mut session = Session::new();
        let speaker = h.speaker.clone();

        let (result, ()) = tokio::join!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                h.turn(
                    &mut session,
                    capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
                ),
            ),
            async {
                while !speaker.is_playing() {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
                speaker.stop();
            }
        );
        result
            .expect("the stop ends the answer, rather than leaving the turn waiting on it")
            .expect("a stopped answer is not a failed request");

        assert!(!h.speaker.is_playing(), "the voice stopped");
        assert_eq!(
            h.displayed(),
            vec!["Paris, on the Seine."],
            "and the answer is still there to read"
        );
        assert!(
            !cancelled(&h.cancel),
            "stopping the voice does not cancel the session"
        );
        assert_eq!(
            session.state(),
            &SessionState::Presenting { speaking: false },
            "and no stop control is left on screen for audio that is no longer playing"
        );
    }

    /// Task 6.9, the second stop path. Speaking is awaited, so nothing in this
    /// module is watching the cancel flag while an answer plays — and that is
    /// exactly when a cancel arrives. The fake speaker deliberately ignores
    /// the session flag, so the only thing that can end this answer is
    /// [`speak_until_cancelled`] turning the cancel into a stop: delete that
    /// and this test hangs rather than passing quietly.
    #[tokio::test]
    async fn cancelling_the_session_while_an_answer_is_playing_stops_the_playback() {
        let h = Harness::new("unused", answers_with("Paris."))
            .await
            .speaking_through(RecordingSpeaker::playing_until_stopped());
        let mut session = Session::new();
        let speaker = h.speaker.clone();
        let cancel = h.cancel.clone();

        let (result, ()) = tokio::join!(
            tokio::time::timeout(
                std::time::Duration::from_secs(5),
                h.turn(
                    &mut session,
                    capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
                ),
            ),
            async {
                while !speaker.is_playing() {
                    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
                }
                // Escape, as the hotkey dispatch sets it.
                cancel.store(true, Ordering::Relaxed);
            }
        );
        result
            .expect("the cancel reaches the voice while it is still playing")
            .expect("cancelling is the user's own choice, not a failure");

        assert!(
            h.speaker.was_stopped(),
            "the session's cancel reached playback as a stop"
        );
        assert!(!h.speaker.is_playing());
        assert_eq!(
            h.displayed(),
            vec!["Paris."],
            "the answer was on screen before the voice started and the cancel did not take it down"
        );
    }

    /// Task 6.9, the third stop path, and the one whose ordering is the whole
    /// point: a follow-up that opens the microphone over the tail of the last
    /// answer records the assistant's own voice and transcribes it as part of
    /// the request.
    ///
    /// The speaker here hands control back while its audio is still playing,
    /// which is not a contrivance — real playback is a blocking thread that
    /// outlives the future which awaited it.
    #[tokio::test]
    async fn a_follow_up_turn_silences_the_previous_answer_before_opening_the_microphone() {
        let timeline = Timeline::default();
        let h = Harness::new("unused", answers_with("Paris."))
            .await
            .speaking_through(
                RecordingSpeaker::leaving_playback_running().noting(timeline.clone()),
            );
        let mut session = Session::new();

        h.turn(
            &mut session,
            capture_of(spoken_pcm(16_000, 2.0), Some("where is it")),
        )
        .await
        .unwrap();
        assert!(
            h.speaker.is_playing(),
            "the answer outlives the turn that spoke it"
        );

        session.apply(SessionEvent::FollowUpStarted);
        let mut audio =
            FramesAudio::new(frames(16_000, 600, 2000), 16_000).noting(timeline.clone());
        let capture = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            capture_turn(
                &mut audio,
                None,
                &h.cancel,
                &h.submit,
                &h.answer_store,
                &h.surface,
                &*h.speaker,
            ),
        )
        .await
        .expect("the follow-up captures")
        .expect("capture succeeds");

        assert_eq!(
            timeline.events(),
            vec!["answer started", "answer stopped", "microphone opened"],
            "silencing the answer after the microphone opened would record it into the follow-up"
        );
        assert!(
            contains_speech(&capture.pcm),
            "and the follow-up itself is still captured"
        );
    }

    // -- what the user is shown -------------------------------------------

    /// Task 7.5, the routing spec's visibility requirement: a user watching the
    /// surface can tell that something was *done* rather than answered, and
    /// what. Named by the action's title, because `read_focused` is the
    /// router's vocabulary and not a phrase anybody said.
    ///
    /// The other half — an answered request carrying no action at all — is
    /// asserted by `a_canned_buffer_is_captured_and_answered_as_a_request`.
    #[tokio::test]
    async fn an_invoked_action_is_named_on_screen_in_the_users_own_words() {
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

        assert_eq!(
            h.recording.answers(),
            vec![Shown::Answer {
                request: "read this".into(),
                action: Some(READ_FOCUSED.title.to_string()),
                text: "a login form".into(),
                speech_error: None,
            }]
        );
        assert_ne!(
            READ_FOCUSED.title, READ_FOCUSED.id,
            "a title that were the id would make the assertion above vacuous"
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
