//! Global-hotkey wiring: startup registration, and the loop that dispatches a
//! press to its feature handler.
//!
//! The loop used to live inline in `setup`. It is here so the composition root
//! stays composition, and so the per-feature parts of a press — its busy flag
//! and its handler — sit next to [`HOTKEY_ACTIONS`], the table the rest of the
//! hotkey paths already read.

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use kea_core::dictation::DictationSettingsRepo;
use kea_core::rewrite::language::describe;
use kea_core::rewrite::RewriteMode;
use kea_core::store::hotkeys::HotkeyBindingRepo;
use kea_core::store::settings::SettingsRepo;
use kea_platform::ActionId;
use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::mpsc;

use crate::commands::trigger_tts_inner;
use crate::commands::{
    apply_dictation_settings, capture_screen_text_inner, close_palette_for, dictation_gate,
    dictation_hotkey_action, hold_dictation_action, hotkey_action_for_id, hotkey_owners,
    lock_cancel_action, meeting_hotkey_action, notify_user, open_palette_session, palette_is_open,
    record_hotkey_reg_status, register_hotkey, resolve_accelerator, run_dictation_action,
    run_selection_rewrite, start_meeting_inner, stop_meeting_inner, try_acquire_busy,
    undo_last_rewrite, BusyGuard, HotkeyAction, MeetingHotkeyAction, RewriteOverride,
    DICTATION_ACTION_ID, HOTKEY_ACTIONS, LOCK_CANCEL_ACTION_ID, MEETINGS_ACTION_ID, OCR_ACTION_ID,
    PALETTE_ACTION_ID, REWRITE_ACTION_ID, TTS_ACTION_ID, UNDO_ACTION_ID,
};
use crate::events::{
    emit_meeting_error, emit_rewrite_error, emit_rewrite_progress, emit_tts_error,
};
use crate::palette::{PaletteEvent, PaletteOrigin};
use crate::AppState;

/// What one hotkey press runs. Boxed because the handlers are `async fn` bodies
/// of different shapes held in one table.
type HandlerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
/// The press's [`BusyGuard`] is passed **by value**, not held by the dispatch
/// loop: most handlers bind it and let it drop when they return, but the
/// palette moves it into its session, because its busy window runs from open
/// to dismissal rather than for the length of its open handler.
///
/// The [`HotkeyAction`] comes along because one handler can serve a whole
/// command family: every per-language translate shortcut runs
/// [`handle_translate`], and the row is where it reads its language from. The
/// six single-command handlers ignore it.
type Handler =
    for<'a> fn(&'a Arc<AppState>, &'a AppHandle, &'a HotkeyAction, BusyGuard) -> HandlerFuture<'a>;

/// One dispatchable hotkey: the row, the flag that keeps a second press out
/// while the first is still running, and the handler.
#[derive(Clone)]
struct Dispatch {
    action: HotkeyAction,
    busy: Arc<AtomicBool>,
    handle: Handler,
}

/// The handler for an action id, or `None` when the action has no dispatch arm.
///
/// The handlers are not interchangeable: dictation gates on the meeting and
/// in-flight state before choosing start/stop/ignore, meetings reads its own
/// recording/processing pair, and the palette pair keep the press's guard
/// instead of letting it drop. Only the busy-flag plumbing is shared.
fn handler_for(action_id: &str) -> Option<Handler> {
    match action_id {
        REWRITE_ACTION_ID => Some(handle_rewrite),
        DICTATION_ACTION_ID => Some(handle_dictation),
        TTS_ACTION_ID => Some(handle_tts),
        MEETINGS_ACTION_ID => Some(handle_meetings),
        PALETTE_ACTION_ID => Some(handle_palette),
        OCR_ACTION_ID => Some(handle_ocr_capture),
        UNDO_ACTION_ID => Some(handle_undo_rewrite),
        // The per-language translate shortcuts are one family rather than a
        // row each, so whether an id names one is asked of the hotkey table's
        // own lookup instead of pattern-matched on a prefix here — same
        // validation, one place.
        other => is_translate_action(other).then_some(handle_translate as Handler),
    }
}

/// Whether `action_id` names one of the per-language translate shortcuts.
fn is_translate_action(action_id: &str) -> bool {
    hotkey_action_for_id(action_id).is_some_and(|a| a.translate_target().is_some())
}

/// The busy flag an action serialises on.
///
/// An action with no shared state gets its own, so presses queued during a
/// long handler are dropped rather than replaying as fresh starts. The two
/// shared flags live on the state instead of being minted here, and for the
/// same reason in both cases — more than one trigger reaches the same
/// mutually-exclusive machinery.
fn busy_flag(action_id: &str, state: &Arc<AppState>) -> Arc<AtomicBool> {
    match action_id {
        DICTATION_ACTION_ID => state.dictation_busy.clone(),
        // The second instance of the same rule, and the sharper one: the
        // rewrite shortcut, the palette and the screen-capture shortcut all
        // fire a synthetic Cmd+C or Cmd+V at the same app, and two of those
        // interleaved is a corrupted document rather than a race that can be
        // lost gracefully. Undo joins them for the same reason: it brings
        // another app forward and writes into it, and an undo landing between
        // a rewrite's ⌘C and its ⌘V corrupts the document just as surely as a
        // second rewrite would. One flag for all four.
        REWRITE_ACTION_ID | PALETTE_ACTION_ID | OCR_ACTION_ID | UNDO_ACTION_ID => {
            state.selection_busy.clone()
        }
        // Third instance, and it moved here for the same reason the first two
        // did: `kea://read-aloud` and `POST /v1/speak` are now triggers too,
        // and a flag minted inside this table would be invisible to them —
        // two reads at once is two voices over each other.
        TTS_ACTION_ID => state.tts_busy.clone(),
        // Fourth, and the reason the translate family shares one flag rather
        // than one per language: a translate shortcut is a selection rewrite
        // too, so two of them mashed together would fire two ⌘C/⌘V pairs at
        // the same document.
        id if is_translate_action(id) => state.selection_busy.clone(),
        _ => Arc::new(AtomicBool::new(false)),
    }
}

fn dispatch_table(state: &Arc<AppState>) -> Vec<Dispatch> {
    HOTKEY_ACTIONS
        .iter()
        .filter_map(|action| {
            let action_id = action.action_id();
            handler_for(&action_id).map(|handle| Dispatch {
                action: action.clone(),
                busy: busy_flag(&action_id, state),
                handle,
            })
        })
        .collect()
}

/// The dispatch entry for one press: the prebuilt row, or a translate row
/// built on the spot.
///
/// The table is built once at startup, but a translate shortcut recorded
/// *after* that is registered immediately by `set_hotkey` and its press
/// arrives here — with no row. Rebuilding is deliberately limited to the
/// translate family, because that family's busy flag is the shared
/// `selection_busy`: anything else would mint a fresh flag per press, which is
/// no gate at all.
fn dispatch_for(table: &[Dispatch], action_id: &str, state: &Arc<AppState>) -> Option<Dispatch> {
    if let Some(found) = table.iter().find(|e| e.action.action_id() == action_id) {
        return Some(found.clone());
    }
    let action = hotkey_action_for_id(action_id).filter(|a| a.translate_target().is_some())?;
    Some(Dispatch {
        busy: busy_flag(action_id, state),
        handle: handler_for(action_id)?,
        action,
    })
}

/// Register every global hotkey and hand back the press stream.
///
/// "Every" is the fixed table *plus* the persisted rows it cannot list — the
/// per-language translate shortcuts — which is why the bindings are read here
/// rather than only inside `resolve_accelerator`.
///
/// Registration outcomes are recorded per feature so the UI can show which
/// shortcut the OS refused.
pub fn register_all(state: &Arc<AppState>, config_pool: &SqlitePool) -> mpsc::Receiver<ActionId> {
    let bindings = tauri::async_runtime::block_on(
        HotkeyBindingRepo::new(config_pool.clone()).list(),
    )
    .unwrap_or_else(|error| {
        // The fixed table still registers from its compiled defaults; only the
        // user's own rows are lost, and they come back on the next launch.
        tracing::warn!(%error, "could not read saved hotkeys; registering the defaults only");
        Vec::new()
    });

    // Resolve every accelerator before taking the hotkeys lock: each one
    // hits the DB, and the lock is a std Mutex.
    let accelerators: Vec<(HotkeyAction, String)> = hotkey_owners(&bindings)
        .into_iter()
        .map(|action| {
            let accel = tauri::async_runtime::block_on(resolve_accelerator(config_pool, &action));
            (action, accel)
        })
        // A translate row has no compiled default, so an empty accelerator
        // means its binding vanished between the two reads. Registering "" is
        // a guaranteed failure booked against a shortcut the user never set.
        .filter(|(_, accel)| !accel.is_empty())
        .collect();
    let mut hk = state.hotkeys.lock().expect("hotkeys lock");
    {
        let mut statuses = state
            .hotkey_reg_status
            .lock()
            .expect("hotkey_reg_status lock");
        for (action, accel) in &accelerators {
            record_hotkey_reg_status(
                &mut statuses,
                action.feature,
                &action.command,
                register_hotkey(&mut hk, action, accel),
            );
        }
    }
    hk.on_action()
}

/// Spawn the dispatch loop: one press in, one spawned handler out.
///
/// The loop itself never awaits a handler — it stays free to read the next
/// press, and the busy flag (held by the spawned task until it drops) is what
/// decides whether that press runs or is dropped.
pub fn spawn_dispatch_loop(state: Arc<AppState>, app: AppHandle, mut rx: mpsc::Receiver<ActionId>) {
    tauri::async_runtime::spawn(async move {
        let table = dispatch_table(&state);

        while let Some(action_id) = rx.recv().await {
            // Escape has no [`HOTKEY_ACTIONS`] row — it is registered only
            // while a locked recording is running, and never user-rebindable —
            // but its press still arrives on this one accelerator stream, so
            // it is matched here rather than in the table.
            if action_id == LOCK_CANCEL_ACTION_ID {
                // The same busy flag as every other dictation trigger: an
                // Escape landing alongside the tap that stops the same lock
                // must not run a cancel behind a stop.
                let Some(guard) = try_acquire_busy(&state.dictation_busy) else {
                    continue;
                };
                let state = state.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    let _busy = guard;
                    // Asked of the hold machine rather than assumed: the press
                    // can still arrive after the lock ended some other way.
                    let event = lock_cancel_action(&state);
                    let (meeting_active, in_flight, current) = dictation_gate(&state).await;
                    let action = hold_dictation_action(event, current, meeting_active, in_flight);
                    run_dictation_action(action, &state, &app).await;
                });
                continue;
            }

            // The palette shortcut toggles, and the toggle has to be decided
            // *before* the busy gate: an open palette holds the shared
            // selection flag for as long as it is up, so the ordinary gate
            // would drop this press rather than act on it — and the user would
            // be left with no way to close the palette from the keyboard they
            // opened it with. Same shape as the Escape case above, and for the
            // same reason: the rule is about this one action, not about the
            // table.
            if action_id == PALETTE_ACTION_ID && palette_is_open(&state) {
                let state = state.clone();
                let app = app.clone();
                tauri::async_runtime::spawn(async move {
                    close_palette_for(&state, &app, PaletteEvent::Hotkey).await;
                });
                continue;
            }

            let Some(entry) = dispatch_for(&table, &action_id, &state) else {
                continue;
            };
            let Some(guard) = try_acquire_busy(&entry.busy) else {
                tracing::debug!(
                    "hotkey press ignored: {} handler in flight",
                    entry.action.feature
                );
                continue;
            };
            let state = state.clone();
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                (entry.handle)(&state, &app, &entry.action, guard).await;
            });
        }
    });
}

/// Put the saved dictation settings back into effect at launch: the ⌥⇧
/// listener if the user left it on, the preroll flag, and the microphone they
/// picked.
///
/// Call after the dispatch loop is up: the hold listener drives the same
/// dictation handlers, and a chord held through launch should find them ready.
pub fn spawn_saved_dictation_settings(
    state: &Arc<AppState>,
    app: &AppHandle,
    config_pool: SqlitePool,
) {
    let state = state.clone();
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        match DictationSettingsRepo::new(SettingsRepo::new(config_pool))
            .get()
            .await
        {
            Ok(settings) => apply_dictation_settings(&state, &app, &settings).await,
            Err(error) => {
                tracing::warn!(%error, "could not read the saved dictation settings")
            }
        }
    });
}

fn handle_rewrite<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        let _busy = busy;
        emit_rewrite_progress(app, "Capturing selection...");
        // The capture-and-insert path itself lives in `run_selection_rewrite`,
        // shared with `kea://rewrite` and `POST /v1/rewrite`; the shortcut
        // adds no opinion of its own, hence the default override.
        match run_selection_rewrite(state, &RewriteOverride::default()).await {
            Ok(_) => emit_rewrite_progress(app, "Done"),
            Err(error) => emit_rewrite_error(app, &error),
        }
    })
}

/// Translate the selection into this shortcut's language.
///
/// The rewrite shortcut's body with the mode and its parameter forced: a
/// translate key does not ask what the saved style is, and clearing the active
/// preset is part of forcing the mode (see `set_rewrite_mode`) — a preset
/// replaces the prompt template outright.
fn handle_translate<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        let _busy = busy;
        // Unreachable: only `dispatch_for`/`handler_for` reach this handler,
        // and both ask the same lookup for the tag first.
        let Some(tag) = action.translate_target() else {
            return;
        };
        // Naming the language out loud is the mitigation for this feature's
        // quiet failure mode — "it translated into the wrong language" is
        // otherwise unfalsifiable from outside the app.
        let language = describe(tag).unwrap_or_else(|_| tag.to_string());
        emit_rewrite_progress(app, &format!("Translating to {language}..."));
        let over = RewriteOverride {
            mode: Some(RewriteMode::Translate),
            instruction: Some(tag.to_string()),
            ..RewriteOverride::default()
        };
        match run_selection_rewrite(state, &over).await {
            Ok(_) => emit_rewrite_progress(app, "Done"),
            Err(error) => emit_rewrite_error(app, &error),
        }
    })
}

fn handle_dictation<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        let _busy = busy;
        let (meeting_active, in_flight, current) = dictation_gate(state).await;
        let action = dictation_hotkey_action(current, meeting_active, in_flight);
        run_dictation_action(action, state, app).await;
    })
}

fn handle_tts<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        let _busy = busy;
        if let Err(error) = trigger_tts_inner(state, app).await {
            emit_tts_error(app, &error);
        }
    })
}

fn handle_meetings<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        let _busy = busy;
        let recording = {
            state
                .active_meeting
                .lock()
                .map(|guard| guard.is_some())
                .unwrap_or(false)
        };
        let processing = state.meeting_processing.load(Ordering::SeqCst);
        match meeting_hotkey_action(recording, processing) {
            MeetingHotkeyAction::Ignore => {
                tracing::debug!("meeting hotkey ignored: a meeting is finishing");
            }
            MeetingHotkeyAction::Stop => {
                if let Err(error) = stop_meeting_inner(state, app).await {
                    emit_meeting_error(app, &error);
                }
            }
            MeetingHotkeyAction::Start => {
                if let Err(error) = start_meeting_inner(state, app).await {
                    emit_meeting_error(app, &error);
                }
            }
        }
    })
}

/// Opens the palette. **The guard is moved into the session**, not dropped
/// when this future returns: the shortcut stays locked out until the palette
/// closes, which is the whole point of sharing one flag with rewrite.
fn handle_palette<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        open_palette_session(state, app, PaletteOrigin::Selection, None, busy).await;
    })
}

/// Put the last rewrite back.
///
/// A notification rather than `emit_rewrite_error`, and the same on success:
/// the key is pressed from someone else's app, where KEA's settings window —
/// the only place that banner shows — is not on screen. "Nothing happened" is
/// the one outcome this feature must never produce silently, and every refusal
/// [`undo_last_rewrite`] returns names its reason.
fn handle_undo_rewrite<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        let _busy = busy;
        match undo_last_rewrite(state).await {
            Ok(_) => emit_rewrite_progress(app, "Put your text back"),
            Err(error) => notify_user(app, &error),
        }
    })
}

/// Region capture → OCR → the palette, prefilled. Also moves its guard: the
/// region selector is open for as long as the user takes, and a rewrite fired
/// during it would paste into whatever is behind the crosshair.
fn handle_ocr_capture<'a>(
    state: &'a Arc<AppState>,
    app: &'a AppHandle,
    _action: &'a HotkeyAction,
    busy: BusyGuard,
) -> HandlerFuture<'a> {
    Box::pin(async move {
        // A notification rather than `emit_rewrite_error`: the shortcut is
        // pressed from someone else's app, so the settings window — where that
        // banner lives — is very likely closed, and a capture that could not
        // start is not something to discover later in the log.
        if let Err(error) = capture_screen_text_inner(state, app, busy).await {
            notify_user(app, &error);
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hotkey_action_has_a_handler() {
        for action in HOTKEY_ACTIONS {
            assert!(
                handler_for(&action.action_id()).is_some(),
                "no dispatch handler for {}",
                action.action_id()
            );
        }
    }

    /// The family reaches the same three table-driven answers a fixed row
    /// does: a handler, the *shared* selection flag, and nothing for a command
    /// that only looks like one.
    #[test]
    fn translate_shortcuts_dispatch_as_one_family() {
        assert!(handler_for("rewrite:translate.pt-BR").is_some());
        assert!(handler_for("rewrite:translate.").is_none());
        assert!(handler_for("rewrite:translate.not a tag").is_none());
        assert!(handler_for("dictation:translate.fr").is_none());
    }

    #[test]
    fn unknown_action_ids_have_no_handler() {
        assert!(handler_for("nope:not_a_command").is_none());
    }

    #[test]
    fn the_lock_cancel_key_is_not_a_table_row() {
        // It is registered around a single locked recording, not owned by the
        // user, and `every_hotkey_action_has_a_handler` would demand a table
        // handler for it that the dispatch loop deliberately does not use.
        assert!(
            HOTKEY_ACTIONS
                .iter()
                .all(|a| a.action_id() != LOCK_CANCEL_ACTION_ID),
            "Escape must not become a rebindable hotkey"
        );
        assert!(handler_for(LOCK_CANCEL_ACTION_ID).is_none());
    }
}
