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
use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

/// Window label the frontend matches on to render the HUD.
pub const LABEL: &str = "overlay";

/// `NSWindowCollectionBehavior` bits, from AppKit's `NSWindow.h`.
///
/// Spelled out rather than taken from objc2-app-kit: pulling that crate in for
/// two constants and two selectors costs a large compile-time dependency, and
/// these values are ABI, so they cannot drift.
#[cfg(target_os = "macos")]
mod collection_behavior {
    /// The window appears on every Space. This is the one — and the only one —
    /// that tao's `visible_on_all_workspaces` sets.
    pub const CAN_JOIN_ALL_SPACES: usize = 1 << 0;
    /// The window does not slide away with its Space during a Space switch.
    pub const STATIONARY: usize = 1 << 4;
    /// Keeps the HUD out of Cmd+` window cycling. It is not a window the user
    /// can focus, so offering it as a cycle target is noise.
    pub const IGNORES_CYCLE: usize = 1 << 6;
    /// **The bit that fixes full screen.** Lets the window join the Space of an
    /// app that is already full-screen instead of being left behind on the
    /// desktop Space. Without it, `CAN_JOIN_ALL_SPACES` covers ordinary Spaces
    /// only, which is exactly the reported symptom: the HUD showed on the
    /// desktop and nowhere else.
    pub const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
}

/// `NSStatusWindowLevel`. `always_on_top` gives the window `NSFloatingWindowLevel`
/// (3), which a full-screen Space still covers; 25 is the level the menu bar
/// extras use and sits above it. Deliberately not `NSScreenSaverWindowLevel`
/// (1000) — the HUD has no business painting over a screen saver or a security
/// prompt.
#[cfg(target_os = "macos")]
const STATUS_WINDOW_LEVEL: isize = 25;

/// The collection behaviour the overlay needs, as one mask.
#[cfg(target_os = "macos")]
pub const fn overlay_collection_behavior() -> usize {
    use collection_behavior::*;
    CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY
}

/// Raises the overlay so it floats over full-screen apps.
///
/// Must run on the main thread — AppKit is not thread-safe — which is why this
/// is called from `create` during setup and never from `sync_visibility`, whose
/// caller is a Tokio worker. Neither `show()` nor `set_position()` resets the
/// level or the behaviour, so applying it once at build time holds.
///
/// Every failure here is swallowed: an overlay at the wrong window level is a
/// cosmetic problem, and dictation itself must not stop working over it.
#[cfg(target_os = "macos")]
fn raise_above_fullscreen(window: &WebviewWindow) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let ns_window = match window.ns_window() {
        Ok(ptr) if !ptr.is_null() => ptr as *mut AnyObject,
        Ok(_) => {
            tracing::warn!("overlay ns_window was null; HUD will not float over full-screen apps");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not reach the overlay NSWindow");
            return;
        }
    };

    // SAFETY: `ns_window` is a live NSWindow owned by tao for as long as the
    // window exists, and both selectors are plain setters on it. Called on the
    // main thread; see the doc comment.
    unsafe {
        let _: () = msg_send![ns_window, setCollectionBehavior: overlay_collection_behavior()];
        let _: () = msg_send![ns_window, setLevel: STATUS_WINDOW_LEVEL];
    }
}

#[cfg(not(target_os = "macos"))]
fn raise_above_fullscreen(_window: &WebviewWindow) {}

const WIDTH: f64 = 340.0;
const HEIGHT: f64 = 96.0;
/// Gap between the HUD and the bottom of the monitor's work area.
const BOTTOM_MARGIN: f64 = 48.0;

/// Top-left physical position that centres a `w x h` window against the bottom
/// of a work area, kept inside the area when the window is wider than it.
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
    // `visible_on_all_workspaces(true)` set above.
    raise_above_fullscreen(&window);
    reposition(&window);
    Ok(window)
}

/// Pins the overlay to the bottom centre of the primary monitor's work area.
pub fn reposition(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
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

/// True when a dictation state means the overlay should be on screen.
pub fn visible_for_state(state: DictationState) -> bool {
    // A locked recording is exactly the one the HUD matters most for: no key
    // is held, so without it there is nothing on screen saying the mic is open.
    matches!(
        state,
        DictationState::Listening | DictationState::Locked | DictationState::Processing
    )
}

/// Shows or hides the overlay for a dictation state. No-op when the overlay
/// failed to build, so dictation still works without it.
pub fn sync_visibility(app: &AppHandle, state: DictationState) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if visible_for_state(state) {
        // Re-pin on every show: the user may have changed displays or
        // resolution since the window was built.
        reposition(&window);
        let _ = window.show();
    } else {
        let _ = window.hide();
    }
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

    /// The regression guard for "the HUD only shows on the desktop".
    ///
    /// CAN_JOIN_ALL_SPACES alone is what tao gives us and what shipped, and it
    /// is not enough: an app already in full screen owns its own Space, and a
    /// window without FULL_SCREEN_AUXILIARY cannot join it. Dropping that bit
    /// would restore the bug silently, since everything still works on the
    /// desktop Space where it is usually tested.
    #[cfg(target_os = "macos")]
    #[test]
    fn collection_behavior_lets_the_hud_join_a_full_screen_space() {
        let mask = overlay_collection_behavior();
        assert_eq!(mask & (1 << 8), 1 << 8, "FULL_SCREEN_AUXILIARY must be set");
        assert_eq!(mask & (1 << 0), 1 << 0, "CAN_JOIN_ALL_SPACES must be set");
        assert_eq!(mask & (1 << 4), 1 << 4, "STATIONARY must be set");
        assert_eq!(mask, 0b1_0101_0001);
    }

    /// The level has to clear NSFloatingWindowLevel (3), which `always_on_top`
    /// sets and a full-screen Space still covers, without reaching the
    /// screen-saver level.
    #[cfg(target_os = "macos")]
    #[test]
    // The point of the test is to pin the constant, so a constant assertion is
    // exactly what it is.
    #[allow(clippy::assertions_on_constants)]
    fn window_level_is_above_floating_and_below_the_screen_saver() {
        assert!(STATUS_WINDOW_LEVEL > 3);
        assert!(STATUS_WINDOW_LEVEL < 1000);
    }

    #[test]
    fn visible_for_state_covers_the_active_states_only() {
        assert!(visible_for_state(DictationState::Listening));
        assert!(visible_for_state(DictationState::Processing));
        assert!(!visible_for_state(DictationState::Idle));
    }
}
