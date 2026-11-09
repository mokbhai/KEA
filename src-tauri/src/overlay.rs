//! The floating dictation overlay window.
//!
//! Dictation types into *another* app, so the in-window indicator is invisible
//! exactly when it is needed. This is a second, borderless, always-on-top
//! webview pinned to the bottom of the primary monitor; the frontend branches
//! on the window label to render the HUD instead of the settings shell.
//!
//! # Manual verification (macOS)
//! 1. Run KEA, put the caret in TextEdit, press the dictation hotkey.
//! 2. The HUD appears bottom-centre and TextEdit keeps its caret and title bar
//!    highlight — KEA must not become the active app.
//! 3. Speak, stop, and confirm the transcript lands in TextEdit (not the HUD).
//! 4. Click where the HUD is: the click must reach the window underneath.
//! 5. Put TextEdit into full screen (green button) and dictate again: the HUD
//!    must appear over the full-screen window, not be stranded on the desktop
//!    Space behind it. Repeat on a second Space and with Mission Control open.

use kea_platform::DictationState;
#[cfg(not(target_os = "macos"))]
use tauri::PhysicalPosition;
use tauri::{AppHandle, Emitter, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

use crate::nswindow::float_over_fullscreen;

/// Window label the frontend matches on to render the HUD.
pub const LABEL: &str = "overlay";

/// The pill's own width, before the live transcript line.
const PILL_WIDTH: f64 = 340.0;

/// How wide the HUD's partial-transcript line is allowed to be.
///
/// **Must agree with `--hud-partial-width` in
/// `ui/src/components/DictationHud.tsx`.** There is no seam where Rust and CSS
/// meet, so a cross-reference comment in both files is the only defence: if
/// the line grows past what this window reserves, it is clipped by the window
/// rather than by its own `overflow: hidden`, and the fade at its start edge
/// stops being the thing that hides the overflow.
const PARTIAL_WIDTH: f64 = 220.0;

/// The window is wider than the pill on purpose, and the extra is *always*
/// there rather than added when a partial arrives.
///
/// Resizing a visible always-on-top window mid-run is the jump this avoids.
/// The window is `transparent(true)` and `set_ignore_cursor_events(true)`, so
/// the added area is invisible and un-hittable and costs nothing when no
/// partial is showing — the pill inside it is centred and simply narrower.
pub const WIDTH: f64 = PILL_WIDTH + PARTIAL_WIDTH;

/// Unchanged by live transcripts: the line is exactly one line at a fixed
/// line-height, so the pill's height is the same whether the text is one word
/// or forty. If it is ever allowed to grow vertically, change
/// `.kea-overlay-root`'s `align-items: center` to `flex-end` first, so growth
/// goes upward off a pinned bottom edge.
pub const HEIGHT: f64 = 96.0;
/// Gap between the HUD and the bottom of the monitor's work area.
pub const BOTTOM_MARGIN: f64 = 48.0;

/// Top-left physical position that centres a `w x h` window against the bottom
/// of a work area, kept inside the area when the window is wider than it.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn bottom_centre_position(
    area_x: i32,
    area_y: i32,
    area_w: u32,
    area_h: u32,
    w: i32,
    h: i32,
    margin: i32,
) -> (i32, i32) {
    let x = area_x + ((area_w as i32 - w) / 2).max(0);
    let y = area_y + (area_h as i32 - h - margin).max(0);
    (x, y)
}

/// Creates the overlay window, hidden.
///
/// **The overlay must never become the key window.** macOS moves key focus to
/// whatever window becomes key, which would take the caret out of the app the
/// user is dictating into and make the synthetic Cmd+V paste into the HUD.
/// `focusable(false)` is the guarantee: tao answers `canBecomeKeyWindow` and
/// `canBecomeMainWindow` with NO for it, so AppKit refuses to promote it even
/// though `show()` goes through `makeKeyAndOrderFront:`. Never call
/// `set_focus()` on this window, and never clear `focusable`.
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("KEA dictation")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(false)
        .visible(false)
        .focused(false)
        .focusable(false)
        .accept_first_mouse(false)
        .visible_on_all_workspaces(true)
        .build()?;

    // Belt and braces on top of `focusable(false)`: a click that never reaches
    // the HUD can't activate KEA by accident either.
    let _ = window.set_ignore_cursor_events(true);
    // Must come after build(): it addresses the NSWindow, which does not exist
    // until then, and it overwrites the collection behaviour that
    // `visible_on_all_workspaces(true)` set above. Shared with the palette —
    // see `crate::nswindow` for why the mask and the level live there.
    float_over_fullscreen(&window);
    reposition(&window);
    Ok(window)
}

/// Pins the overlay to the bottom centre of the target monitor's work area.
pub fn reposition(window: &WebviewWindow) {
    #[cfg(target_os = "macos")]
    {
        crate::nswindow::reposition_on_active_screen(window, WIDTH, HEIGHT, BOTTOM_MARGIN);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let Some(monitor) = crate::nswindow::target_monitor(window) else {
            return;
        };
        let scale = monitor.scale_factor();
        let area = monitor.work_area();
        let (x, y) = bottom_centre_position(
            area.position.x,
            area.position.y,
            area.size.width,
            area.size.height,
            (WIDTH * scale).round() as i32,
            (HEIGHT * scale).round() as i32,
            (BOTTOM_MARGIN * scale).round() as i32,
        );
        let _ = window.set_position(PhysicalPosition::new(x, y));
    }
}

/// True when a dictation state means the overlay should be on screen.
pub fn visible_for_state(state: DictationState) -> bool {
    // A locked recording is exactly the one the HUD matters most for: no key
    // is held, so without it there is nothing on screen saying the mic is open.
    matches!(
        state,
        DictationState::Listening | DictationState::Locked | DictationState::Processing
    )
}

/// The features that draw into the overlay window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Owner {
    Dictation,
    Assistant,
    Status,
}

impl Owner {
    fn index(self) -> usize {
        match self {
            Owner::Dictation => 0,
            Owner::Assistant => 1,
            Owner::Status => 2,
        }
    }

    /// The spelling the frontend matches on.
    pub fn wire(self) -> &'static str {
        match self {
            Owner::Dictation => "dictation",
            Owner::Assistant => "assistant",
            Owner::Status => "status",
        }
    }
}

/// Who has a claim on the overlay, and which claim is the newest.
///
/// **One window, three features.** Dictation, the voice assistant and status
/// notices share a single webview, so at any moment exactly one of them may
/// decide what it shows. This used to be three independent booleans with no
/// rule for two being true at once — the frontend drew every one of them side
/// by side, and a flag the assistant forgot to clear kept its content alive
/// under every later dictation run.
///
/// The rule now: **the most recent activity owns the content.** Every
/// `set(owner, true)` is activity and takes the front, whether or not the owner
/// already held a claim; `set(owner, false)` releases it. The newest claim is
/// shown, and when it is released the next newest is shown again — so a
/// dictation run that interrupts an open assistant session hands the overlay
/// back to the session when it finishes, rather than blanking it.
///
/// Pure — no `AppHandle`, no statics — so the rule is unit-testable.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Ownership {
    /// The tick of each owner's newest claim, `None` when it holds none.
    claims: [Option<u64>; 3],
    clock: u64,
}

impl Ownership {
    pub const fn new() -> Self {
        Self {
            claims: [None; 3],
            clock: 0,
        }
    }

    /// Claims (`true`) or releases (`false`) the overlay for `owner`.
    pub fn set(&mut self, owner: Owner, active: bool) {
        self.claims[owner.index()] = if active {
            self.clock += 1;
            Some(self.clock)
        } else {
            None
        };
    }

    /// Releases `owner` only if its claim is still the one at `claim`.
    ///
    /// For timed dismissals: a dwell that expires must not tear down a *newer*
    /// claim the same owner made while it was waiting (a new session, say).
    pub fn release_if_claim(&mut self, owner: Owner, claim: Option<u64>) -> bool {
        if claim.is_some() && self.claims[owner.index()] == claim {
            self.claims[owner.index()] = None;
            true
        } else {
            false
        }
    }

    /// The tick of `owner`'s current claim, for [`Self::release_if_claim`].
    pub fn claim(&self, owner: Owner) -> Option<u64> {
        self.claims[owner.index()]
    }

    pub fn is_active(&self, owner: Owner) -> bool {
        self.claims[owner.index()].is_some()
    }

    /// The claimant whose content is shown: the newest claim.
    pub fn owner(&self) -> Option<Owner> {
        [Owner::Dictation, Owner::Assistant, Owner::Status]
            .into_iter()
            .filter_map(|o| self.claims[o.index()].map(|tick| (tick, o)))
            .max_by_key(|(tick, _)| *tick)
            .map(|(_, o)| o)
    }

    /// Whether the window should be on screen at all.
    pub fn should_show(&self) -> bool {
        self.owner().is_some()
    }

    pub fn payload(&self) -> OverlayOwnerPayload {
        OverlayOwnerPayload {
            owner: self.owner().map(Owner::wire),
            dictation: self.is_active(Owner::Dictation),
            assistant: self.is_active(Owner::Assistant),
            status: self.is_active(Owner::Status),
        }
    }
}

/// `overlay:owner`: what the overlay webview should draw.
///
/// `owner` picks the visible HUD; the three flags say who still holds a claim,
/// which is what lets the assistant HUD tell "covered by a newer owner" (keep
/// the session) from "released" (forget it).
#[derive(Clone, Debug, serde::Serialize, PartialEq, Eq)]
pub struct OverlayOwnerPayload {
    pub owner: Option<&'static str>,
    pub dictation: bool,
    pub assistant: bool,
    pub status: bool,
}

static OWNERSHIP: std::sync::Mutex<Ownership> = std::sync::Mutex::new(Ownership::new());

fn ownership() -> std::sync::MutexGuard<'static, Ownership> {
    // The state is a few integers; a panic elsewhere cannot leave it torn.
    OWNERSHIP.lock().unwrap_or_else(|p| p.into_inner())
}

/// The current ownership, for a webview that has just (re)loaded.
pub fn current_payload() -> OverlayOwnerPayload {
    ownership().payload()
}

/// Number of times the overlay webview has reported itself loaded. More than
/// one means it reloaded — a content-process crash recovery, or a dev reload.
static WEBVIEW_LOADS: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

/// The overlay webview reporting it has loaded. Returns the ownership to draw.
pub fn webview_ready() -> OverlayOwnerPayload {
    let loads = WEBVIEW_LOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
    let payload = current_payload();
    if loads > 1 {
        tracing::warn!(loads, ?payload, "overlay: webview reloaded after its first load");
    } else {
        tracing::info!(?payload, "overlay: webview loaded");
    }
    payload
}

/// Applies a claim/release, tells the webview what to draw, then shows or
/// hides the window. In that order: the content is decided before the window
/// is ordered front, so a newly shown overlay never flashes the previous
/// owner's pill.
fn apply(app: &AppHandle, owner: Owner, active: bool) {
    let payload = {
        let mut o = ownership();
        o.set(owner, active);
        o.payload()
    };
    publish(app, &payload);
    refresh_visibility(app);
}

fn publish(app: &AppHandle, payload: &OverlayOwnerPayload) {
    let _ = app.emit("overlay:owner", payload.clone());
}

/// Refreshes the overlay window's visibility based on whether any subsystem
/// (dictation, assistant, or status) needs it on screen.
///
/// Self-healing: every show re-asserts the panel's level, collection
/// behaviour, style and frame, logs what it found (so drift is visible in the
/// log rather than inferred), and rebuilds the window if it is gone or still
/// not on screen after being ordered front.
pub fn refresh_visibility(app: &AppHandle) {
    let (show, payload) = {
        let o = ownership();
        (o.should_show(), o.payload())
    };
    let Some(window) = app.get_webview_window(LABEL) else {
        if show {
            tracing::error!(?payload, "overlay: window is gone; rebuilding it");
            recreate(app);
        }
        return;
    };
    if show {
        tracing::info!(?payload, "overlay: showing");
        // Re-pin on every show: the user may have moved to another display
        // or changed resolution since the window was built.
        reposition(&window);
        #[cfg(not(target_os = "macos"))]
        let _ = window.show();
        // On macOS, `window.show()` calls `makeKeyAndOrderFront:`, which activates
        // KEA and pins the window to KEA's space. `order_front_without_activating`
        // uses `orderFrontRegardless` on an NSPanel so the HUD floats over fullscreen
        // apps and other Spaces without activating KEA. It also re-asserts the
        // panel's properties, logs a before/after snapshot, and rebuilds the
        // window if it is still not visible.
        crate::nswindow::order_front_without_activating(&window);
    } else {
        tracing::debug!(?payload, "overlay: hiding");
        let _ = window.hide();
    }
}

static RECREATING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Destroys whatever is left of the overlay window and builds a fresh one,
/// then re-applies the current ownership.
///
/// Off the caller's thread: building a window marshals to the main thread and
/// waits, and the callers of [`refresh_visibility`] include the main thread
/// itself (sync commands). One rebuild at a time; a second request while one is
/// running is dropped, since the first re-applies the ownership when it lands.
pub fn recreate(app: &AppHandle) {
    use std::sync::atomic::Ordering;
    if RECREATING.swap(true, Ordering::SeqCst) {
        return;
    }
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        if let Some(old) = app.get_webview_window(LABEL) {
            let _ = old.destroy();
        }
        let mut rebuilt = false;
        for attempt in 1..=5u32 {
            // Destruction is asynchronous and the label is only free once the
            // old window has actually gone.
            tokio::time::sleep(std::time::Duration::from_millis(150)).await;
            if app.get_webview_window(LABEL).is_some() {
                continue;
            }
            match create(&app) {
                Ok(_) => {
                    tracing::warn!(attempt, "overlay: window rebuilt");
                    rebuilt = true;
                    break;
                }
                Err(error) => tracing::error!(%error, attempt, "overlay: rebuild failed"),
            }
        }
        RECREATING.store(false, Ordering::SeqCst);
        if rebuilt {
            // The new webview announces itself through `overlay_ready`, which
            // returns the ownership; ordering it front is all that is left.
            refresh_visibility(&app);
        }
    });
}

/// Shows or hides the overlay for a dictation state. No-op when the overlay
/// failed to build, so dictation still works without it.
pub fn sync_visibility(app: &AppHandle, state: DictationState) {
    apply(app, Owner::Dictation, visible_for_state(state));
}

/// Show or hide the overlay for an assistant session.
pub fn sync_assistant_visibility(app: &AppHandle, visible: bool) {
    apply(app, Owner::Assistant, visible);
}

/// Show or hide the overlay for a status notification (rewrite, tts, errors).
pub fn sync_status_visibility(app: &AppHandle, visible: bool) {
    apply(app, Owner::Status, visible);
}

/// Releases the assistant's claim `dwell` from now, unless the assistant has
/// claimed the overlay again in the meantime.
///
/// For a failure shown *after* its session ended: `run_session` returns its
/// error, its guard releases the overlay, and the error is then emitted by the
/// caller — which claims the overlay again with nothing left to release it. A
/// claim that is never released is a stuck owner.
pub fn release_assistant_after(app: &AppHandle, dwell: std::time::Duration) {
    let claim = ownership().claim(Owner::Assistant);
    let app = app.clone();
    tauri::async_runtime::spawn(async move {
        tokio::time::sleep(dwell).await;
        let payload = {
            let mut o = ownership();
            if !o.release_if_claim(Owner::Assistant, claim) {
                return;
            }
            o.payload()
        };
        publish(&app, &payload);
        refresh_visibility(&app);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bottom_centre_position_centres_horizontally() {
        let (x, _) = bottom_centre_position(0, 0, 1000, 800, 340, 96, 48);
        assert_eq!(x, 330);
    }

    #[test]
    fn bottom_centre_position_sits_above_the_work_area_bottom() {
        let (_, y) = bottom_centre_position(0, 0, 1000, 800, 340, 96, 48);
        assert_eq!(y, 800 - 96 - 48);
    }

    #[test]
    fn bottom_centre_position_offsets_by_the_monitor_origin() {
        // A secondary display left of the primary has a negative origin.
        let (x, y) = bottom_centre_position(-1920, 25, 1000, 800, 340, 96, 48);
        assert_eq!(x, -1920 + 330);
        assert_eq!(y, 25 + 800 - 96 - 48);
    }

    #[test]
    fn bottom_centre_position_never_leaves_the_work_area_origin() {
        // A window larger than the area would otherwise be placed off-screen
        // at a negative offset from the origin.
        let (x, y) = bottom_centre_position(0, 0, 200, 50, 340, 96, 48);
        assert_eq!((x, y), (0, 0));
    }

    /// The four `bottom_centre_position` tests above pass 340/96 as literals,
    /// so they neither break nor notice when the window changes size. This is
    /// the one that notices: the window has to hold the pill *and* the
    /// transcript line the HUD is allowed to draw.
    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_window_reserves_room_for_the_transcript_line() {
        assert!(WIDTH >= PILL_WIDTH + PARTIAL_WIDTH);
        // Still one line tall. Growth here would move the bottom edge.
        assert_eq!(HEIGHT, 96.0);
    }

    #[test]
    fn visible_for_state_covers_the_active_states_only() {
        assert!(visible_for_state(DictationState::Listening));
        assert!(visible_for_state(DictationState::Processing));
        assert!(!visible_for_state(DictationState::Idle));
    }

    fn owned(o: &Ownership) -> Option<Owner> {
        o.owner()
    }

    #[test]
    fn nobody_owns_a_fresh_overlay_and_it_stays_hidden() {
        let o = Ownership::new();
        assert_eq!(owned(&o), None);
        assert!(!o.should_show());
    }

    #[test]
    fn each_owner_alone_shows_the_overlay_and_releasing_hides_it() {
        for owner in [Owner::Dictation, Owner::Assistant, Owner::Status] {
            let mut o = Ownership::new();
            o.set(owner, true);
            assert_eq!(owned(&o), Some(owner));
            assert!(o.should_show());
            o.set(owner, false);
            assert_eq!(owned(&o), None);
            assert!(!o.should_show());
        }
    }

    /// The reported bug: the assistant answered, then plain dictation showed
    /// the assistant's popup instead of its own HUD.
    #[test]
    fn dictation_after_a_finished_assistant_session_shows_dictation() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true); // listening
        o.set(Owner::Assistant, true); // presenting
        o.set(Owner::Assistant, false); // session ended, guard released

        o.set(Owner::Dictation, true);
        assert_eq!(owned(&o), Some(Owner::Dictation));
        assert!(!o.is_active(Owner::Assistant));
        assert!(!o.payload().assistant);
    }

    /// The same bug without the release: whatever happened to the assistant's
    /// flag, dictation starting *now* is the most recent activity.
    #[test]
    fn dictation_wins_over_an_assistant_claim_that_was_never_released() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true);
        o.set(Owner::Dictation, true);
        assert_eq!(owned(&o), Some(Owner::Dictation));
        // …and the assistant, still open, is not forgotten.
        assert!(o.payload().assistant);
    }

    #[test]
    fn the_most_recent_activity_owns_the_overlay() {
        let mut o = Ownership::new();
        o.set(Owner::Dictation, true);
        o.set(Owner::Status, true);
        assert_eq!(owned(&o), Some(Owner::Status));
        o.set(Owner::Assistant, true);
        assert_eq!(owned(&o), Some(Owner::Assistant));
    }

    #[test]
    fn fresh_activity_by_a_held_claim_takes_the_front_back() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true);
        o.set(Owner::Dictation, true);
        // The dictation state moving on (listening → processing) is activity
        // and keeps it in front; the assistant emitting a new state is too.
        o.set(Owner::Dictation, true);
        assert_eq!(owned(&o), Some(Owner::Dictation));
        o.set(Owner::Assistant, true);
        assert_eq!(owned(&o), Some(Owner::Assistant));
    }

    #[test]
    fn releasing_the_newest_owner_falls_back_to_the_next_newest() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true);
        o.set(Owner::Dictation, true);
        o.set(Owner::Dictation, false);
        assert_eq!(owned(&o), Some(Owner::Assistant));
        assert!(o.should_show());
        o.set(Owner::Assistant, false);
        assert!(!o.should_show());
    }

    #[test]
    fn releasing_an_owner_that_holds_nothing_changes_nothing() {
        let mut o = Ownership::new();
        o.set(Owner::Dictation, true);
        o.set(Owner::Assistant, false);
        assert_eq!(owned(&o), Some(Owner::Dictation));
    }

    /// Error and cancel both reach the guard's `set(Assistant, false)`; neither
    /// may leave the assistant able to out-rank a later dictation run.
    #[test]
    fn an_assistant_session_released_after_error_or_cancel_does_not_linger() {
        for _ in ["error", "cancel"] {
            let mut o = Ownership::new();
            o.set(Owner::Assistant, true);
            o.set(Owner::Assistant, true); // Failed / Listening state emitted
            o.set(Owner::Assistant, false); // OverlayGuard dropped
            assert!(!o.is_active(Owner::Assistant));
            assert!(!o.should_show());
        }
    }

    /// `run_session` returns Err → its guard releases → the caller emits
    /// `Failed`, which claims again. The dwell timer releases that claim.
    #[test]
    fn a_failure_shown_after_the_session_ended_is_released_by_its_dwell() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true);
        o.set(Owner::Assistant, false);
        o.set(Owner::Assistant, true); // Failed emitted by the caller
        let claim = o.claim(Owner::Assistant);

        assert!(o.release_if_claim(Owner::Assistant, claim));
        assert!(!o.should_show());
    }

    #[test]
    fn a_dwell_does_not_release_a_newer_session() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true);
        let claim = o.claim(Owner::Assistant);
        // The user starts another session before the dwell runs out.
        o.set(Owner::Assistant, true);

        assert!(!o.release_if_claim(Owner::Assistant, claim));
        assert!(o.is_active(Owner::Assistant));
    }

    #[test]
    fn a_dwell_with_no_claim_releases_nothing() {
        let mut o = Ownership::new();
        o.set(Owner::Dictation, true);
        assert!(!o.release_if_claim(Owner::Assistant, o.claim(Owner::Assistant)));
        assert!(o.is_active(Owner::Dictation));
    }

    #[test]
    fn the_payload_names_the_owner_and_every_live_claim() {
        let mut o = Ownership::new();
        o.set(Owner::Assistant, true);
        o.set(Owner::Dictation, true);
        assert_eq!(
            o.payload(),
            OverlayOwnerPayload {
                owner: Some("dictation"),
                dictation: true,
                assistant: true,
                status: false,
            }
        );
        let json = serde_json::to_value(o.payload()).unwrap();
        assert_eq!(json["owner"], "dictation");
        assert_eq!(json["assistant"], true);
        assert_eq!(
            serde_json::to_value(Ownership::new().payload()).unwrap()["owner"],
            serde_json::Value::Null
        );
    }
}
