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

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kea_core::assistant::dispatch::{ActionOutcome, Dispatcher, RequestPlan};
use kea_core::assistant::routing::route;
use kea_core::assistant::session::{Session, SessionEvent};
use kea_core::assistant::{plan, ActionRegistry};
use kea_core::resolve::SlotResolver;
use kea_core::store::bindings::BindingRepo;
use kea_core::store::settings::SettingsRepo;
use kea_core::TtsSettingsRepo;
use kea_features::ASSISTANT_FEATURE_ID;
use kea_platform::audio::segment::{find_segment_cut, SegmentCutConfig};
use kea_platform::audio::{DictationState, PcmFrame};
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

/// Run one request end to end.
///
/// Errors are returned rather than emitted here so the caller owns how a
/// failure reaches the user; every internal step that can fail already emitted
/// the state the surface needs.
pub async fn run_session(state: &Arc<AppState>, app: &AppHandle) -> Result<(), String> {
    let mut session = Session::new();

    // Cleared on the way in rather than on the way out: a press landing just
    // after the previous session ended must not cancel this one before it
    // starts.
    state.assistant_cancel.store(false, Ordering::Relaxed);
    crate::commands::set_assistant_cancellable(state, true);
    // Escape belongs to the rest of the Mac again the moment this returns, by
    // every path including the early ones below.
    let _escape = EscapeGuard { state: state.clone() };
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

    events::emit_assistant_state(app, session.state());

    let pcm = match capture_request(state).await {
        Ok(pcm) => pcm,
        Err(error) => {
            session.apply(SessionEvent::Failed {
                message: error.clone(),
            });
            events::emit_assistant_state(app, session.state());
            return Err(error);
        }
    };

    // Cancelled while listening: the audio is dropped and nothing is sent.
    if cancelled(state) {
        return Ok(());
    }

    // Nothing was said. Closing without reaching a provider is the whole point:
    // an empty request must not become a prompt.
    if pcm.samples.is_empty() {
        return Ok(());
    }

    session.apply(SessionEvent::RequestEnded);
    events::emit_assistant_state(app, session.state());

    let result = complete_request(state, app, &mut session, pcm).await;

    // A cancel that arrived mid-flight is not a failure to report: the user
    // ended it deliberately, and an error banner for their own action reads as
    // a bug.
    if cancelled(state) {
        return Ok(());
    }

    // Leave the answer up long enough to read. Polled rather than slept in one
    // go so Escape still ends the session immediately.
    if result.is_ok() {
        for _ in 0..(PRESENT_DWELL_SECS * 10) {
            if cancelled(state) {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    if let Err(error) = &result {
        session.apply(SessionEvent::Failed {
            message: error.clone(),
        });
        events::emit_assistant_state(app, session.state());
    }
    result
}

/// Hold the microphone until the user stops talking.
async fn capture_request(state: &Arc<AppState>) -> Result<PcmFrame, String> {
    let mut audio = state.audio.lock().await;
    let mut frames = audio.start_mic().await.map_err(|e| e.to_string())?;

    let mut samples: Vec<f32> = Vec::new();
    let mut rate = 0u32;

    while let Some(frame) = frames.recv().await {
        if rate == 0 {
            rate = frame.sample_rate_hz;
        }
        samples.extend_from_slice(&frame.samples);

        if cancelled(state) {
            break;
        }
        if rate > 0 && find_segment_cut(&samples, rate, REQUEST_CUT).is_some() {
            break;
        }
        if rate > 0 && samples.len() as f32 / rate as f32 >= MAX_CAPTURE_SECS {
            break;
        }
    }

    // `stop_mic` returns the authoritative buffer — the same one dictation
    // transcribes — so the frames above are only used to decide *when* to stop.
    // Taking both keeps the endpointing honest without the buffer having two
    // owners.
    audio.stop_mic().await.map_err(|e| e.to_string())
}

async fn complete_request(
    state: &Arc<AppState>,
    app: &AppHandle,
    session: &mut Session,
    pcm: PcmFrame,
) -> Result<(), String> {
    let bindings = BindingRepo::new(state.config_pool.clone());

    let utterance = transcribe(state, &bindings, &pcm).await?;
    if utterance.trim().is_empty() {
        return Err("I didn't catch that.".into());
    }
    session.set_request(utterance.clone());

    let registry = ActionRegistry::default();
    let llm_binding = SlotResolver::new(&state.engines, &bindings)
        .require_llm(ASSISTANT_FEATURE_ID)
        .await
        .map_err(|e| e.to_string())?;
    let engine = state
        .engines
        .llm(&llm_binding.engine_id)
        .ok_or_else(|| format!("no llm engine '{}'", llm_binding.engine_id))?;

    let outcome = route(
        engine.as_ref(),
        &llm_binding,
        &registry,
        &utterance,
        session.history(),
    )
    .await
    .map_err(|e| e.to_string())?;

    match plan(outcome, &registry) {
        RequestPlan::Answer { text } => present(state, app, session, &utterance, text, None).await,

        RequestPlan::Invoke { spec, args } => {
            // The last gate before anything happens outside this process. An
            // action that has not started must not start now.
            if cancelled(state) {
                return Ok(());
            }
            let dispatcher = build_dispatcher(state);
            let ActionOutcome { text, disclosure } = dispatcher
                .invoke(spec, &args)
                .await
                .map_err(|e| e.to_string())?;
            let spoken = text.unwrap_or_else(|| format!("Done: {}.", spec.title));
            present(state, app, session, &utterance, spoken, disclosure).await
        }

        // Both of these are words, not failures: the assistant understood
        // enough to ask, and the session stays open for the answer.
        RequestPlan::Clarify { question } => {
            present(state, app, session, &utterance, question, None).await
        }

        RequestPlan::Fail { message } => Err(message),
    }
}

async fn transcribe(
    state: &Arc<AppState>,
    bindings: &BindingRepo,
    pcm: &PcmFrame,
) -> Result<String, String> {
    let binding = SlotResolver::new(&state.engines, bindings)
        .require_stt(ASSISTANT_FEATURE_ID)
        .await
        .map_err(|e| e.to_string())?;
    let engine = state
        .engines
        .stt(&binding.engine_id)
        .ok_or_else(|| format!("no stt engine '{}'", binding.engine_id))?;

    let opts = kea_engines::traits::SttOpts {
        model: binding.model.clone(),
        provider_ref: binding.provider_ref.clone(),
        ..Default::default()
    };
    let transcript = kea_features::meeting::transcribe_pcm_segment(engine.as_ref(), pcm, opts)
        .await
        .map_err(|e| e.to_string())?;
    Ok(transcript.text)
}

/// Show the answer, speak it, and move the session to `Presenting`.
async fn present(
    state: &Arc<AppState>,
    app: &AppHandle,
    session: &mut Session,
    request: &str,
    answer: String,
    disclosure: Option<kea_core::assistant::dispatch::Disclosure>,
) -> Result<(), String> {
    let settings = TtsSettingsRepo::new(SettingsRepo::new(state.config_pool.clone()))
        .get()
        .await
        .unwrap_or_default();
    let bindings = BindingRepo::new(state.config_pool.clone());

    // Speech is attempted, never required. A missing or unresolvable voice
    // leaves the answer on screen and says it could not be spoken — the same
    // stance the streaming recogniser gets.
    let spoken = match kea_features::tts::speak_text_for(
        &state.engines,
        &bindings,
        &settings,
        ASSISTANT_FEATURE_ID,
        &answer,
    )
    .await
    {
        Ok(pcm) => {
            let cancel = state.assistant_cancel.clone();
            let handle = tokio::task::spawn_blocking(move || {
                kea_platform::audio::playback::play_pcm_cancellable(&pcm, &cancel)
            });
            match handle.await {
                Ok(Ok(())) => true,
                Ok(Err(error)) => {
                    tracing::warn!(%error, "assistant answer could not be played");
                    false
                }
                Err(error) => {
                    tracing::warn!(%error, "assistant playback task failed");
                    false
                }
            }
        }
        Err(error) => {
            tracing::warn!(%error, "assistant answer could not be synthesized");
            false
        }
    };

    events::emit_assistant_answer(app, request, &answer, disclosure.as_ref());
    session.apply(SessionEvent::Answered {
        text: answer,
        spoken,
    });
    // Playback above is awaited to completion, so by the time the state is
    // emitted it is no longer speaking. Reported honestly rather than as
    // `speaking: true`, which would leave a stop control on screen for audio
    // that already finished.
    session.apply(SessionEvent::SpeechFinished);
    events::emit_assistant_state(app, session.state());
    Ok(())
}

/// Whether the user has asked for this session to stop.
fn cancelled(state: &Arc<AppState>) -> bool {
    state.assistant_cancel.load(Ordering::Relaxed)
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
fn build_dispatcher(state: &Arc<AppState>) -> Dispatcher {
    let _ = state;
    let mut d = Dispatcher::new();
    d.register(Arc::new(kea_features::assistant::ReadFocused::new(
        Arc::from(kea_platform::new_text_io()),
        Arc::from(kea_platform::screen::new_screen_reader()),
    )));
    d.register(Arc::new(kea_features::assistant::OpenApp::new(Arc::from(
        kea_platform::apps::new_app_launcher(),
    ))));
    d
}
