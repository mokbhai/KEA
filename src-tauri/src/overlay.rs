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

use tauri::{AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder};

/// Window label the frontend matches on to render the HUD.
pub const LABEL: &str = "overlay";

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

/// True when a `dictation:state` value means the overlay should be on screen.
pub fn visible_for_state(state: &str) -> bool {
    matches!(state, "listening" | "processing")
}

/// Shows or hides the overlay for a dictation state. No-op when the overlay
/// failed to build, so dictation still works without it.
pub fn sync_visibility(app: &AppHandle, state: &str) {
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

    #[test]
    fn visible_for_state_covers_the_active_states_only() {
        assert!(visible_for_state("listening"));
        assert!(visible_for_state("processing"));
        assert!(!visible_for_state("idle"));
        assert!(!visible_for_state(""));
    }
}
