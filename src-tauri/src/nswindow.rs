//! The AppKit window chrome KEA's borderless windows share.
//!
//! Two windows now need the same two setters — the dictation HUD
//! (`crate::overlay`) and the prompt palette (`crate::palette`) — and both
//! need them for the same reason: a window at the default level, with the
//! collection behaviour tao gives it, is stranded on the desktop Space when
//! the user is in a full-screen app. That was a real bug in the HUD, and the
//! palette would have reproduced it exactly. So the mask, the level and the
//! test that guards them live here once rather than being copied a second
//! time.
//!
//! Nothing here is palette- or HUD-specific; what differs between the two
//! windows (focusable, transparent, position) stays in their own modules.

use tauri::WebviewWindow;

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
    /// Keeps the window out of Cmd+` cycling. Neither the HUD (which cannot be
    /// focused at all) nor the palette (which is dismissed rather than cycled
    /// back to) is a sensible cycle target.
    pub const IGNORES_CYCLE: usize = 1 << 6;
    /// **The bit that fixes full screen.** Lets the window join the Space of an
    /// app that is already full-screen instead of being left behind on the
    /// desktop Space. Without it, `CAN_JOIN_ALL_SPACES` covers ordinary Spaces
    /// only, which is exactly the reported symptom: the HUD showed on the
    /// desktop and nowhere else.
    pub const FULL_SCREEN_AUXILIARY: usize = 1 << 8;
}

/// `NSStatusWindowLevel`. `always_on_top` gives a window
/// `NSFloatingWindowLevel` (3), which a full-screen Space still covers; 25 is
/// the level the menu bar extras use and sits above it. Deliberately not
/// `NSScreenSaverWindowLevel` (1000) — neither window has any business
/// painting over a screen saver or a security prompt.
#[cfg(target_os = "macos")]
pub const STATUS_WINDOW_LEVEL: isize = 25;

/// The collection behaviour a floating KEA window needs, as one mask.
#[cfg(target_os = "macos")]
pub const fn floating_collection_behavior() -> usize {
    use collection_behavior::*;
    CAN_JOIN_ALL_SPACES | STATIONARY | IGNORES_CYCLE | FULL_SCREEN_AUXILIARY
}

/// Raises `window` so it floats over full-screen apps.
///
/// Must run on the main thread — AppKit is not thread-safe — which is why both
/// callers call it from `create` during setup and never from a Tokio worker.
/// Neither `show()` nor `set_position()` resets the level or the behaviour, so
/// applying it once at build time holds.
///
/// Every failure here is swallowed: a window at the wrong level is a cosmetic
/// problem, and neither dictation nor the palette may stop working over it.
///
/// # MAIN THREAD ONLY
///
/// Unlike [`order_front_without_activating`], this does not marshal. Both of
/// its call sites are window construction during `setup`, which is the main
/// thread, and keeping it synchronous means the flags are set before the
/// window can be shown.
///
/// Calling it from anywhere else is a crash, not a glitch: AppKit window calls
/// off the main thread SIGTRAP, and that is exactly how the async version of
/// the sibling function below was found. If you need this from an async path,
/// call the sibling or wrap this in `run_on_main_thread` — do not simply move
/// the call.
#[cfg(target_os = "macos")]
pub fn float_over_fullscreen(window: &WebviewWindow) {
    use objc2::msg_send;
    use objc2::runtime::AnyObject;

    let label = window.label().to_string();
    let ns_window = match window.ns_window() {
        Ok(ptr) if !ptr.is_null() => ptr as *mut AnyObject,
        Ok(_) => {
            tracing::warn!(window = %label, "ns_window was null; it will not float over full-screen apps");
            return;
        }
        Err(e) => {
            tracing::warn!(error = %e, window = %label, "could not reach the NSWindow");
            return;
        }
    };

    // SAFETY: `ns_window` is a live NSWindow owned by tao for as long as the
    // window exists, and both selectors are plain setters on it. Called on the
    // main thread; see the doc comment.
    unsafe {
        let _: () = msg_send![ns_window, setCollectionBehavior: floating_collection_behavior()];
        let _: () = msg_send![ns_window, setLevel: STATUS_WINDOW_LEVEL];
    }
}

/// Orders `window` onto the active space without activating KEA.
///
/// Tauri's `show()` reaches tao's `set_visible(true)`, which calls
/// `makeKeyAndOrderFront:`. That is the right call for an ordinary window and
/// the wrong one here. Our overlay is built `focusable(false)`, so AppKit
/// refuses to make it key — and from a *background* application, ordering a
/// window front that way does not reliably put it on another app's full-screen
/// space. The reported symptom was exactly that: the HUD appears over ordinary
/// windows and never over a full-screen app.
///
/// `orderFrontRegardless` is the documented call for showing a window without
/// activating its application, which is precisely what a dictation HUD is.
///
/// The collection behaviour and level are re-applied here rather than trusted
/// from build time. They are cheap setters, they are idempotent, and the
/// alternative is depending on the claim that nothing else ever touches them —
/// a claim this file used to make in a comment and could not prove.
///
/// # Why this marshals to the main thread, at the cost of a crash
///
/// Ordering a window is an AppKit operation and AppKit is main-thread only.
/// The first version of this function called `orderFrontRegardless` on
/// whatever thread reached it, and the callers are async: `sync_visibility`
/// runs from `emit_dictation_state`, which runs on a tokio worker. That
/// SIGTRAPped inside `-[NSWindow _doOrderWindow:]` on the first ⌥⇧ press.
///
/// The reason it was not obvious is that the neighbouring code is safe for a
/// reason that does not extend here: `window.show()` looks like the same kind
/// of call, but Tauri marshals it internally. A raw `msg_send!` does not, so
/// every AppKit selector sent from app code has to do it explicitly.
#[cfg(target_os = "macos")]
pub fn order_front_without_activating(window: &WebviewWindow) {
    let window = window.clone();
    // Failure here means the main thread is gone, i.e. the app is shutting
    // down. Nothing to do about that, and the HUD is cosmetic either way.
    let _ = window.clone().run_on_main_thread(move || {
        use objc2::msg_send;
        use objc2::runtime::AnyObject;

        let ns_window = match window.ns_window() {
            Ok(ptr) if !ptr.is_null() => ptr as *mut AnyObject,
            _ => return,
        };

        // SAFETY: a live NSWindow owned by tao, three plain selectors on it,
        // and we are on the main thread — which is the whole point of the
        // closure this runs inside.
        unsafe {
            let _: () = msg_send![ns_window, setCollectionBehavior: floating_collection_behavior()];
            let _: () = msg_send![ns_window, setLevel: STATUS_WINDOW_LEVEL];
            let _: () = msg_send![ns_window, orderFrontRegardless];
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn order_front_without_activating(_window: &WebviewWindow) {}

#[cfg(not(target_os = "macos"))]
pub fn float_over_fullscreen(_window: &WebviewWindow) {}

/// Top-left physical position that centres a `w x h` window horizontally in a
/// work area and puts its top edge `top` down from the area's top.
///
/// The palette's counterpart to [`crate::overlay::bottom_centre_position`];
/// both clamp to the work-area origin so a window larger than the display is
/// placed on screen rather than at a negative offset.
pub fn top_centre_position(
    area_x: i32,
    area_y: i32,
    area_w: u32,
    area_h: u32,
    w: i32,
    h: i32,
    top: i32,
) -> (i32, i32) {
    let x = area_x + ((area_w as i32 - w) / 2).max(0);
    // Clamped against the *bottom* too: on a short display (or with a tall
    // window) a fixed fraction from the top would push the input field off
    // the bottom edge, which is worse than sitting higher than asked.
    let y = area_y + top.min((area_h as i32 - h).max(0)).max(0);
    (x, y)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression guard for "the floating window only shows on the
    /// desktop", now covering both windows because both read this one mask.
    ///
    /// CAN_JOIN_ALL_SPACES alone is what tao gives us and what shipped, and it
    /// is not enough: an app already in full screen owns its own Space, and a
    /// window without FULL_SCREEN_AUXILIARY cannot join it. Dropping that bit
    /// would restore the bug silently, since everything still works on the
    /// desktop Space where it is usually tested.
    #[cfg(target_os = "macos")]
    #[test]
    fn collection_behavior_lets_a_window_join_a_full_screen_space() {
        let mask = floating_collection_behavior();
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
    fn top_centre_position_centres_horizontally() {
        let (x, _) = top_centre_position(0, 0, 1000, 800, 600, 120, 224);
        assert_eq!(x, 200);
    }

    #[test]
    fn top_centre_position_places_the_top_edge() {
        let (_, y) = top_centre_position(0, 0, 1000, 800, 600, 120, 224);
        assert_eq!(y, 224);
    }

    #[test]
    fn top_centre_position_offsets_by_the_monitor_origin() {
        // A secondary display left of the primary has a negative origin.
        let (x, y) = top_centre_position(-1920, 25, 1000, 800, 600, 120, 224);
        assert_eq!(x, -1920 + 200);
        assert_eq!(y, 25 + 224);
    }

    #[test]
    fn top_centre_position_keeps_a_too_large_window_on_screen() {
        let (x, y) = top_centre_position(0, 0, 400, 100, 600, 120, 224);
        assert_eq!((x, y), (0, 0));
    }

    #[test]
    fn top_centre_position_pulls_the_window_up_on_a_short_display() {
        // 800 - 120 = 680 of room; asking for 900 down would put the input
        // field below the bottom edge.
        let (_, y) = top_centre_position(0, 0, 1000, 800, 600, 120, 900);
        assert_eq!(y, 680);
    }
}
