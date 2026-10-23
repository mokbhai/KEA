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
    AppHandle, Manager, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};
#[cfg(not(target_os = "macos"))]
use tauri::PhysicalPosition;

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

static DICTATION_OVERLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static ASSISTANT_OVERLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
static STATUS_OVERLAY: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

pub fn dictation_owns_overlay() -> bool {
    DICTATION_OVERLAY.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn assistant_owns_overlay() -> bool {
    ASSISTANT_OVERLAY.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn status_owns_overlay() -> bool {
    STATUS_OVERLAY.load(std::sync::atomic::Ordering::Relaxed)
}

pub fn should_show_overlay() -> bool {
    dictation_owns_overlay() || assistant_owns_overlay() || status_owns_overlay()
}

/// Refreshes the overlay window's visibility based on whether any subsystem
/// (dictation, assistant, or status) needs it on screen.
pub fn refresh_visibility(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        return;
    };
    if should_show_overlay() {
        // Re-pin on every show: the user may have moved to another display
        // or changed resolution since the window was built.
        reposition(&window);
        #[cfg(not(target_os = "macos"))]
        let _ = window.show();
        // On macOS, `window.show()` calls `makeKeyAndOrderFront:`, which activates
        // KEA and pins the window to KEA's space. `order_front_without_activating`
        // uses `orderFrontRegardless` on an NSPanel so the HUD floats over fullscreen
        // apps and other Spaces without activating KEA.
        crate::nswindow::order_front_without_activating(&window);
    } else {
        let _ = window.hide();
    }
}

/// Shows or hides the overlay for a dictation state. No-op when the overlay
/// failed to build, so dictation still works without it.
pub fn sync_visibility(app: &AppHandle, state: DictationState) {
    DICTATION_OVERLAY.store(visible_for_state(state), std::sync::atomic::Ordering::Relaxed);
    refresh_visibility(app);
}

/// Show or hide the overlay for an assistant session.
pub fn sync_assistant_visibility(app: &AppHandle, visible: bool) {
    ASSISTANT_OVERLAY.store(visible, std::sync::atomic::Ordering::Relaxed);
    refresh_visibility(app);
}

/// Show or hide the overlay for a status notification (rewrite, tts, errors).
pub fn sync_status_visibility(app: &AppHandle, visible: bool) {
    STATUS_OVERLAY.store(visible, std::sync::atomic::Ordering::Relaxed);
    refresh_visibility(app);
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

    #[test]
    fn should_show_overlay_reflects_subsystem_flags() {
        DICTATION_OVERLAY.store(false, std::sync::atomic::Ordering::Relaxed);
        ASSISTANT_OVERLAY.store(false, std::sync::atomic::Ordering::Relaxed);
        STATUS_OVERLAY.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!should_show_overlay());

        STATUS_OVERLAY.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(should_show_overlay());
        assert!(status_owns_overlay());

        STATUS_OVERLAY.store(false, std::sync::atomic::Ordering::Relaxed);
        DICTATION_OVERLAY.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(should_show_overlay());
        assert!(dictation_owns_overlay());

        DICTATION_OVERLAY.store(false, std::sync::atomic::Ordering::Relaxed);
        ASSISTANT_OVERLAY.store(true, std::sync::atomic::Ordering::Relaxed);
        assert!(should_show_overlay());
        assert!(assistant_owns_overlay());

        ASSISTANT_OVERLAY.store(false, std::sync::atomic::Ordering::Relaxed);
        assert!(!should_show_overlay());
    }
}
