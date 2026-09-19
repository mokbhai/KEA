//! Carrying out a [`KeaAction`], through the same gates a hotkey press takes.
//!
//! # The gate
//!
//! The audio layer admits exactly one recorder, and several busy flags guard
//! work that is already in flight. An API call is a *third* trigger source
//! beside the accelerator and the hold chord, so it claims the same flags they
//! do — `state.dictation_busy` for anything that opens the microphone,
//! `state.selection_busy` for anything that fires a synthetic ⌘C or ⌘V at the
//! frontmost app, `state.tts_busy` for read-aloud — and calls the same
//! `*_inner` functions. Nothing here reimplements a refusal: every check
//! inside `start_dictation_run` is load-bearing and is reached by calling it.
//!
//! A failed acquire is **409, never a wait**. An API caller queued behind a
//! hotkey-started recording would stop that recording at a moment the user
//! never asked for.

use std::sync::atomic::Ordering;
use std::sync::Arc;

use kea_core::rewrite::RewriteMode;
use tauri::{AppHandle, Manager};

use crate::commands::{
    capture_selection_text, dictation_gate, dictation_hotkey_action, preview_rewrite_inner,
    run_selection_rewrite, start_dictation_inner, stop_dictation_inner, transcribe_file_run,
    trigger_tts_inner, try_acquire_busy, RewriteOverride,
};
use crate::events::{dictation_state_wire, meeting_state_wire};
use kea_platform::MeetingState;

use super::actions::{transcribe_path_allowed, DictationVerb, KeaAction, RewriteRequest};

/// What a finished action hands back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionOutcome {
    /// Nothing to report but success — a start, a window shown.
    Done,
    /// The rewritten, dictated or transcribed text.
    Text(String),
    Status(StatusReport),
}

/// `GET /v1/status`.
///
/// The two lifecycle values go through the existing wire mappers rather than a
/// third spelling of "listening": the HUD, the frontend and this endpoint all
/// read the same words.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatusReport {
    pub version: String,
    pub dictation_state: String,
    pub meeting_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActionError {
    /// Something is already using the machinery this action needs.
    Busy(String),
    /// The request cannot mean anything, or asked for something refused by
    /// policy (a path outside the home, KEA being frontmost).
    Refused(String),
    /// The action ran and failed.
    Failed(String),
}

impl ActionError {
    pub fn status(&self) -> u16 {
        match self {
            ActionError::Busy(_) => 409,
            ActionError::Refused(_) => 400,
            ActionError::Failed(_) => 500,
        }
    }
}

impl std::fmt::Display for ActionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ActionError::Busy(m) | ActionError::Refused(m) | ActionError::Failed(m) => {
                write!(f, "{m}")
            }
        }
    }
}

/// Whether an action costs an LLM call, and so draws on the rate limiter.
pub fn spends_llm_credits(action: &KeaAction) -> bool {
    matches!(action, KeaAction::Rewrite(_))
}

/// The message a selection verb refuses with when KEA itself is frontmost.
///
/// Opening a URL activates the handler app, and every selection verb depends
/// on the *user's* app being frontmost — activating KEA destroys exactly that.
/// Rewriting KEA's own UI text is a worse outcome than refusing, so this
/// refuses, and names the flag that avoids it.
const FOCUS_REFUSAL: &str =
    "KEA is frontmost, so there is no selection to work on. Call it with `open -g kea://…` \
     (-g leaves the handler in the background) or from a script that does not activate KEA.";

/// What a caller gets when a guard is already held.
///
/// **409, never a wait.** An API caller queued behind a hotkey-started
/// recording would stop it at a moment the user never asked for, and a request
/// that blocks until the microphone is free is a request whose timeout is the
/// length of somebody's sentence.
fn busy_refusal(message: &str) -> ActionError {
    ActionError::Busy(message.to_string())
}

/// Is KEA the app the selection would be read from?
///
/// Asked of the same probe the profile system uses, and compared against the
/// bundle identifier from the app config rather than a literal, so a rename
/// cannot silently turn this check off.
async fn kea_is_frontmost(state: &Arc<crate::AppState>, app: &AppHandle) -> bool {
    let Some(ctx) = crate::commands::capture_app_context_now(state).await else {
        // Nothing identifiable up front. Not KEA as far as we can tell, and
        // refusing on an unknown would break the case the guard exists for.
        return false;
    };
    ctx.bundle_id.as_deref() == Some(app.config().identifier.as_str())
}

pub async fn execute(
    state: &Arc<crate::AppState>,
    app: &AppHandle,
    action: KeaAction,
) -> Result<ActionOutcome, ActionError> {
    if action.needs_the_users_app() && kea_is_frontmost(state, app).await {
        return Err(ActionError::Refused(FOCUS_REFUSAL.into()));
    }

    match action {
        KeaAction::Rewrite(request) => rewrite(state, request).await,
        KeaAction::Dictation(verb) => dictation(state, app, verb).await,
        KeaAction::ReadAloud => read_aloud(state, app).await,
        KeaAction::Transcribe { path } => {
            let home = home_dir().ok_or_else(|| {
                ActionError::Failed("could not determine the home directory".into())
            })?;
            if !transcribe_path_allowed(&path, &home) {
                return Err(ActionError::Refused(format!(
                    "{} is not an absolute path inside {}",
                    path.display(),
                    home.display()
                )));
            }
            // The one verb that never touches the microphone, so it is also
            // the only one that may run alongside a recording. Its own flag
            // (`file_transcribe_busy`) is claimed inside the run.
            transcribe_file_run(state, app, &path.to_string_lossy())
                .await
                .map(|_| ActionOutcome::Done)
                .map_err(classify)
        }
        KeaAction::Open { page } => open_page(app, &page),
        KeaAction::Status => Ok(ActionOutcome::Status(status(state).await)),
    }
}

/// A `Busy`/`Failed` split over the string errors the command layer returns.
///
/// The `*_inner` functions predate this module and answer with a `String`.
/// Rather than reshape their signatures — the hotkey paths and the Tauri
/// commands both construct those calls — the two refusals that mean "come back
/// later" are recognised here, in one place, and everything else is a failure.
fn classify(message: String) -> ActionError {
    let already = message.contains("already")
        || message.contains("wait for it")
        || message.contains("in flight");
    if already {
        ActionError::Busy(message)
    } else {
        ActionError::Failed(message)
    }
}

async fn rewrite(
    state: &Arc<crate::AppState>,
    request: RewriteRequest,
) -> Result<ActionOutcome, ActionError> {
    request
        .validate()
        .map_err(|e| ActionError::Refused(e.to_string()))?;

    let over = RewriteOverride {
        mode: request.mode,
        preset_id: request.preset_id.clone(),
        instruction: request.instruction.clone(),
    };

    // Supplied text: no capture, no insertion, no history row — the same deal
    // the settings window's "try it" box gets.
    if let Some(text) = request.text {
        let (mode, instruction) = resolve_preview_mode(state, &over).await;
        return preview_rewrite_inner(state, text, mode, over.preset_id, instruction)
            .await
            .map(ActionOutcome::Text)
            .map_err(classify);
    }

    // From here on a synthetic ⌘C or ⌘V is in play, so this takes the same
    // flag the rewrite shortcut, the palette and the screen capture share.
    let Some(_busy) = try_acquire_busy(&state.selection_busy) else {
        return Err(busy_refusal(
            "KEA is already working on the selection (a rewrite, the palette or a capture is open)",
        ));
    };

    if request.insert {
        return run_selection_rewrite(state, &over)
            .await
            .map(ActionOutcome::Text)
            .map_err(classify);
    }

    // Capture but do not insert: the caller wants the rewritten text back and
    // will decide what to do with it.
    let text = capture_selection_text()
        .await
        .map_err(ActionError::Refused)?;
    let (mode, instruction) = resolve_preview_mode(state, &over).await;
    preview_rewrite_inner(state, text, mode, over.preset_id, instruction)
        .await
        .map(ActionOutcome::Text)
        .map_err(classify)
}

/// The mode and parameter a non-inserting rewrite runs with.
///
/// It has no app profile to consult (nothing is being written back into an
/// app), so it falls back to the saved defaults — and the parameter is
/// re-derived from whichever mode wins, because a mode's parameter comes from
/// a key chosen by that mode.
async fn resolve_preview_mode(
    state: &Arc<crate::AppState>,
    over: &RewriteOverride,
) -> (RewriteMode, Option<String>) {
    let mut input = crate::commands::default_rewrite_input(&state.config_pool).await;
    over.apply(&mut input, &state.config_pool).await;
    (input.mode, input.custom_instruction)
}

async fn dictation(
    state: &Arc<crate::AppState>,
    app: &AppHandle,
    verb: DictationVerb,
) -> Result<ActionOutcome, ActionError> {
    // The same flag the accelerator loop and the hold-to-talk task claim, and
    // for the same reason: two triggers must not each start a run.
    let Some(_busy) = try_acquire_busy(&state.dictation_busy) else {
        return Err(busy_refusal("a dictation trigger is already being handled"));
    };

    let (meeting_active, in_flight, current) = dictation_gate(state).await;
    tracing::debug!(
        verb = verb.as_str(),
        state = dictation_state_wire(current),
        meeting_active,
        in_flight,
        "local API: dictation"
    );
    // A read, not a claim: it reports *why* a directed start or stop cannot
    // run without attempting one. The toggle still goes through the shared
    // decision function.
    let toggle = dictation_hotkey_action(current, meeting_active, in_flight);

    use crate::commands::DictationHotkeyAction as Decision;
    match verb {
        DictationVerb::Toggle => match toggle {
            Decision::Start => start_dictation_inner(state, app)
                .await
                .map(|()| ActionOutcome::Done)
                .map_err(classify),
            Decision::Stop => stop_dictation_inner(state, app)
                .await
                .map(ActionOutcome::Text)
                .map_err(classify),
            // StartLocked and Cancel are hold-chord transitions; a toggle
            // never produces them, and Ignore means the gate said no.
            _ => Err(ActionError::Busy(format!(
                "dictation cannot be toggled right now (state: {})",
                dictation_state_wire(current)
            ))),
        },
        DictationVerb::Start => {
            if toggle != Decision::Start {
                return Err(ActionError::Busy(format!(
                    "dictation cannot start right now (state: {})",
                    dictation_state_wire(current)
                )));
            }
            start_dictation_inner(state, app)
                .await
                .map(|()| ActionOutcome::Done)
                .map_err(classify)
        }
        DictationVerb::Stop => {
            if toggle != Decision::Stop {
                return Err(ActionError::Busy(format!(
                    "nothing is being dictated (state: {})",
                    dictation_state_wire(current)
                )));
            }
            stop_dictation_inner(state, app)
                .await
                .map(ActionOutcome::Text)
                .map_err(classify)
        }
    }
}

async fn read_aloud(
    state: &Arc<crate::AppState>,
    app: &AppHandle,
) -> Result<ActionOutcome, ActionError> {
    let Some(_busy) = try_acquire_busy(&state.tts_busy) else {
        return Err(busy_refusal("KEA is already reading something"));
    };
    trigger_tts_inner(state, app)
        .await
        .map(|()| ActionOutcome::Done)
        .map_err(classify)
}

/// The only verb that shows a window.
///
/// Three lines, the same three the tray's "Open KEA" item uses. Every other
/// verb stays headless, so a cold `kea://rewrite` does not flash the settings
/// window on its way to the selection.
fn open_page(app: &AppHandle, page: &str) -> Result<ActionOutcome, ActionError> {
    let Some(window) = app.get_webview_window("main") else {
        return Err(ActionError::Failed("the main window is gone".into()));
    };
    let _ = window.show();
    let _ = window.set_focus();
    // The frontend owns routing; it listens for this and switches pages.
    let _ = tauri::Emitter::emit(app, "api:open-page", page);
    Ok(ActionOutcome::Done)
}

async fn status(state: &Arc<crate::AppState>) -> StatusReport {
    // Deliberately the gate's reading of dictation rather than the raw audio
    // state: it is the one that can tell a locked run from a held one.
    let (_, _, dictation) = dictation_gate(state).await;
    let meeting = {
        let recording = state
            .active_meeting
            .lock()
            .map(|guard| guard.is_some())
            .unwrap_or(false);
        if state.meeting_processing.load(Ordering::SeqCst) {
            MeetingState::Processing
        } else if recording {
            MeetingState::Recording
        } else {
            MeetingState::Idle
        }
    };
    StatusReport {
        version: env!("CARGO_PKG_VERSION").to_string(),
        dictation_state: dictation_state_wire(dictation).to_string(),
        meeting_state: meeting_state_wire(meeting).to_string(),
    }
}

fn home_dir() -> Option<std::path::PathBuf> {
    std::env::var_os("HOME").map(std::path::PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::actions::parse_kea_url;

    #[test]
    fn only_the_llm_verbs_draw_on_the_rate_limiter() {
        assert!(spends_llm_credits(
            &parse_kea_url("kea://rewrite?text=hi").unwrap()
        ));
        assert!(!spends_llm_credits(
            &parse_kea_url("kea://dictation/start").unwrap()
        ));
        assert!(!spends_llm_credits(&KeaAction::Status));
    }

    #[test]
    fn busy_refusals_are_409_and_everything_else_is_not() {
        assert_eq!(
            classify("a file is already being transcribed; wait for it to finish".into()).status(),
            409
        );
        assert_eq!(
            classify("a meeting is finishing; wait for it to complete".into()).status(),
            409
        );
        assert_eq!(classify("no stt engine 'whisper'".into()).status(), 500);
        assert_eq!(ActionError::Refused("nope".into()).status(), 400);
    }

    #[test]
    fn a_held_guard_refuses_with_409_instead_of_queueing() {
        use std::sync::atomic::AtomicBool;

        // Stands in for a hotkey-started run holding `dictation_busy`.
        let flag = Arc::new(AtomicBool::new(false));
        let held = try_acquire_busy(&flag).expect("the hotkey claims it first");

        // This is the claim the API call makes. Asserted on the flag rather
        // than on a sleep.
        assert!(try_acquire_busy(&flag).is_none());
        assert_eq!(busy_refusal("dictation is in flight").status(), 409);

        drop(held);
        assert!(
            try_acquire_busy(&flag).is_some(),
            "the guard did not release the flag"
        );
    }

    #[test]
    fn selection_verbs_are_the_ones_that_need_the_users_app() {
        assert!(parse_kea_url("kea://rewrite?mode=concise")
            .unwrap()
            .needs_the_users_app());
        assert!(parse_kea_url("kea://read-aloud")
            .unwrap()
            .needs_the_users_app());
        // Supplied text needs nothing from the frontmost app.
        assert!(!parse_kea_url("kea://rewrite?text=hi")
            .unwrap()
            .needs_the_users_app());
        assert!(!parse_kea_url("kea://open/general")
            .unwrap()
            .needs_the_users_app());
    }
}
