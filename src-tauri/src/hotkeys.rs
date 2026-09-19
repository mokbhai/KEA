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
use kea_core::store::settings::SettingsRepo;
use kea_platform::ActionId;
use sqlx::SqlitePool;
use tauri::AppHandle;
use tokio::sync::mpsc;

use crate::commands::trigger_tts_inner;
use crate::commands::{
    apply_dictation_settings, default_rewrite_input, dictation_gate, dictation_hotkey_action,
    execute_rewrite, hold_dictation_action, lock_cancel_action, meeting_hotkey_action,
    record_hotkey_reg_status, register_hotkey, resolve_accelerator, run_dictation_action,
    start_meeting_inner, stop_meeting_inner, try_acquire_busy, HotkeyAction, MeetingHotkeyAction,
    DICTATION_ACTION_ID, HOTKEY_ACTIONS, LOCK_CANCEL_ACTION_ID, MEETINGS_ACTION_ID,
    REWRITE_ACTION_ID, TTS_ACTION_ID,
};
use crate::events::{
    emit_meeting_error, emit_rewrite_error, emit_rewrite_progress, emit_tts_error,
};
use crate::AppState;

/// What one hotkey press runs. Boxed because the handlers are `async fn` bodies
/// of different shapes held in one table.
type HandlerFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;
type Handler = for<'a> fn(&'a Arc<AppState>, &'a AppHandle) -> HandlerFuture<'a>;

/// One dispatchable hotkey: the [`HOTKEY_ACTIONS`] row, the flag that keeps a
/// second press out while the first is still running, and the handler.
struct Dispatch {
    action: HotkeyAction,
    busy: Arc<AtomicBool>,
    handle: Handler,
}

/// The handler for an action id, or `None` when the action has no dispatch arm.
///
/// The four handlers are not interchangeable: dictation gates on the meeting
/// and in-flight state before choosing start/stop/ignore, and meetings reads
/// its own recording/processing pair. Only the busy-flag plumbing is shared.
fn handler_for(action_id: &str) -> Option<Handler> {
    match action_id {
        REWRITE_ACTION_ID => Some(handle_rewrite),
        DICTATION_ACTION_ID => Some(handle_dictation),
        TTS_ACTION_ID => Some(handle_tts),
        MEETINGS_ACTION_ID => Some(handle_meetings),
        _ => None,
    }
}

/// The busy flag an action serialises on.
///
/// Every feature gets its own, so presses queued during a long handler are
/// dropped rather than replaying as fresh starts. Dictation's lives on the
/// state instead of being minted here: hold-to-talk drives the same handlers
/// from its own task, and a chord and a Cmd+Shift+D arriving together must
/// contend for one flag, not one each.
fn busy_flag(action_id: &str, state: &Arc<AppState>) -> Arc<AtomicBool> {
    if action_id == DICTATION_ACTION_ID {
        state.dictation_busy.clone()
    } else {
        Arc::new(AtomicBool::new(false))
    }
}

fn dispatch_table(state: &Arc<AppState>) -> Vec<Dispatch> {
    HOTKEY_ACTIONS
        .iter()
        .filter_map(|action| {
            handler_for(action.action_id).map(|handle| Dispatch {
                action: *action,
                busy: busy_flag(action.action_id, state),
                handle,
            })
        })
        .collect()
}

/// Register every hotkey in [`HOTKEY_ACTIONS`] and hand back the press stream.
///
/// Registration outcomes are recorded per feature so the UI can show which
/// shortcut the OS refused.
pub fn register_all(state: &Arc<AppState>, config_pool: &SqlitePool) -> mpsc::Receiver<ActionId> {
    // Resolve every accelerator before taking the hotkeys lock: each one
    // hits the DB, and the lock is a std Mutex.
    let accelerators: Vec<(HotkeyAction, String)> = HOTKEY_ACTIONS
        .iter()
        .map(|action| {
            let accel = tauri::async_runtime::block_on(resolve_accelerator(config_pool, action));
            (*action, accel)
        })
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
                action.command,
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

            let Some(entry) = table.iter().find(|e| e.action.action_id == action_id) else {
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
            let handle = entry.handle;
            tauri::async_runtime::spawn(async move {
                let _busy = guard;
                handle(&state, &app).await;
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

fn handle_rewrite<'a>(state: &'a Arc<AppState>, app: &'a AppHandle) -> HandlerFuture<'a> {
    Box::pin(async move {
        emit_rewrite_progress(app, "Capturing selection...");
        let input = default_rewrite_input(&state.config_pool).await;
        match execute_rewrite(state, input).await {
            Ok(_) => emit_rewrite_progress(app, "Done"),
            Err(error) => emit_rewrite_error(app, &error),
        }
    })
}

fn handle_dictation<'a>(state: &'a Arc<AppState>, app: &'a AppHandle) -> HandlerFuture<'a> {
    Box::pin(async move {
        let (meeting_active, in_flight, current) = dictation_gate(state).await;
        let action = dictation_hotkey_action(current, meeting_active, in_flight);
        run_dictation_action(action, state, app).await;
    })
}

fn handle_tts<'a>(state: &'a Arc<AppState>, app: &'a AppHandle) -> HandlerFuture<'a> {
    Box::pin(async move {
        if let Err(error) = trigger_tts_inner(state, app).await {
            emit_tts_error(app, &error);
        }
    })
}

fn handle_meetings<'a>(state: &'a Arc<AppState>, app: &'a AppHandle) -> HandlerFuture<'a> {
    Box::pin(async move {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_hotkey_action_has_a_handler() {
        for action in HOTKEY_ACTIONS {
            assert!(
                handler_for(action.action_id).is_some(),
                "no dispatch handler for {}",
                action.action_id
            );
        }
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
                .all(|a| a.action_id != LOCK_CANCEL_ACTION_ID),
            "Escape must not become a rebindable hotkey"
        );
        assert!(handler_for(LOCK_CANCEL_ACTION_ID).is_none());
    }
}
