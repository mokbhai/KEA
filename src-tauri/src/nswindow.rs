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

#[cfg(target_os = "macos")]
use tauri::Manager;
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
    /// Allows the window to join Spaces associated with all other applications,
    /// including another application's fullscreen Space.
    pub const CAN_JOIN_ALL_APPLICATIONS: usize = 1 << 18;
}

/// AppKit `NSWindowStyleMask` bits.
#[cfg(target_os = "macos")]
pub mod style_mask {
    /// Identifies the window as a secondary utility panel.
    pub const UTILITY_WINDOW: usize = 1 << 4;
    /// **The bit that allows floating over other applications' fullscreen Spaces.**
    ///
    /// macOS requires that a panel displayed over another application's Space
    /// is nonactivating so that showing it does not pull focus away from the
    /// active application or trigger a Space switch.
    pub const NONACTIVATING_PANEL: usize = 1 << 7;
    /// Allows the content view to span the full window dimensions.
    pub const FULLSIZE_CONTENT_VIEW: usize = 1 << 15;
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct CGPoint {
    pub x: f64,
    pub y: f64,
}

#[cfg(target_os = "macos")]
unsafe impl objc2::encode::Encode for CGPoint {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGPoint", &[f64::ENCODING, f64::ENCODING]);
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct CGSize {
    pub width: f64,
    pub height: f64,
}

#[cfg(target_os = "macos")]
unsafe impl objc2::encode::Encode for CGSize {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGSize", &[f64::ENCODING, f64::ENCODING]);
}

#[cfg(target_os = "macos")]
#[repr(C)]
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct CGRect {
    pub origin: CGPoint,
    pub size: CGSize,
}

#[cfg(target_os = "macos")]
unsafe impl objc2::encode::Encode for CGRect {
    const ENCODING: objc2::encode::Encoding =
        objc2::encode::Encoding::Struct("CGRect", &[CGPoint::ENCODING, CGSize::ENCODING]);
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
    CAN_JOIN_ALL_SPACES
        | STATIONARY
        | IGNORES_CYCLE
        | FULL_SCREEN_AUXILIARY
        | CAN_JOIN_ALL_APPLICATIONS
}

/// Converts a Tao `NSWindow` to an AppKit `NSPanel` with the necessary
/// collection behaviour, style mask, and level to float over fullscreen apps
/// and across all desktop spaces.
///
/// An ordinary `NSWindow` rejects `NSWindowStyleMaskNonactivatingPanel` and
/// cannot join another application's fullscreen Space when KEA is running in the
/// background. Converting it to `NSPanel` and applying `NONACTIVATING_PANEL`
/// allows the macOS Window Server to treat the window as an auxiliary panel.
#[cfg(target_os = "macos")]
unsafe fn convert_to_panel(ns_window: *mut objc2::runtime::AnyObject, nonactivating: bool) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    if let Some(panel_class) =
        AnyClass::get(core::ffi::CStr::from_bytes_with_nul(b"NSPanel\0").unwrap())
    {
        let _: *const AnyClass = objc2::ffi::object_setClass(ns_window, panel_class);
    }

    let current_mask: usize = msg_send![ns_window, styleMask];
    let mut new_mask =
        current_mask | style_mask::UTILITY_WINDOW | style_mask::FULLSIZE_CONTENT_VIEW;
    if nonactivating {
        new_mask |= style_mask::NONACTIVATING_PANEL;
    }
    let _: () = msg_send![ns_window, setStyleMask: new_mask];
    let _: () = msg_send![ns_window, setCollectionBehavior: floating_collection_behavior()];
    // `setFloatingPanel: true` resets window level to NSFloatingWindowLevel (3),
    // so `setLevel: STATUS_WINDOW_LEVEL` (25) MUST be called after it.
    let _: () = msg_send![ns_window, setFloatingPanel: true];
    let _: () = msg_send![ns_window, setLevel: STATUS_WINDOW_LEVEL];
    let _: () = msg_send![ns_window, setHidesOnDeactivate: false];
    let _: () = msg_send![ns_window, setCanHide: false];
    let _: () = msg_send![ns_window, setWorksWhenModal: true];
}

/// Raises `window` so it floats over full-screen apps.
///
/// Must run on the main thread — AppKit is not thread-safe — which is why both
/// callers call it from `create` during setup and never from a Tokio worker.
/// Applied once at build time, and again on every show by
/// [`order_front_without_activating`] — which also logs whether the level or
/// the collection behaviour had drifted in between, so the claim that nothing
/// resets them is checked rather than assumed.
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

    // The overlay is click-through and non-activating; the prompt palette is focusable.
    let is_overlay = label == crate::overlay::LABEL;
    unsafe {
        convert_to_panel(ns_window, is_overlay);
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

        let label = window.label().to_string();
        let ns_window = match window.ns_window() {
            Ok(ptr) if !ptr.is_null() => ptr as *mut AnyObject,
            other => {
                // The window object exists but its NSWindow does not: nothing
                // we can order front. Only the overlay is rebuilt — the
                // palette has its own lifecycle.
                tracing::error!(window = %label, result = ?other.map(|_| ()), "ns_window unavailable");
                if label == crate::overlay::LABEL {
                    crate::overlay::recreate(window.app_handle());
                }
                return;
            }
        };

        let is_overlay = label == crate::overlay::LABEL;
        unsafe {
            // What the panel looks like *before* we touch it. Anything other
            // than the values we set at build time means something else
            // changed the window since, which is the evidence a "stopped
            // appearing" report needs.
            let before = if is_overlay { Some(panel_snapshot(ns_window)) } else { None };
            if let Some(before) = &before {
                let drift = before.drift_before_show();
                if !drift.is_empty() {
                    tracing::warn!(window = %label, ?drift, snapshot = ?before, "panel drifted since the last show; re-asserting");
                }
            }

            convert_to_panel(ns_window, is_overlay);
            if is_overlay {
                reposition_ns_window_on_active_screen(
                    ns_window,
                    crate::overlay::WIDTH,
                    crate::overlay::HEIGHT,
                    crate::overlay::BOTTOM_MARGIN,
                );
            }
            let _: () = msg_send![ns_window, orderFrontRegardless];

            if !is_overlay {
                return;
            }
            let mut after = panel_snapshot(ns_window);
            if !after.visible {
                // `orderFrontRegardless` did not take. Fall back to the
                // ordinary show (the panel is non-focusable, so it cannot take
                // key), re-assert, and look again.
                tracing::warn!(snapshot = ?after, "overlay not visible after orderFrontRegardless; retrying with show()");
                let _ = window.show();
                convert_to_panel(ns_window, true);
                let _: () = msg_send![ns_window, orderFrontRegardless];
                after = panel_snapshot(ns_window);
            }
            let problems = after.problems();
            if after.visible && problems.is_empty() {
                tracing::info!(snapshot = ?after, "overlay shown");
            } else if after.visible {
                tracing::warn!(?problems, snapshot = ?after, "overlay shown with unexpected properties");
            } else {
                tracing::error!(?problems, snapshot = ?after, "overlay still not visible; rebuilding the window");
                crate::overlay::recreate(window.app_handle());
            }
        }
    });
}

#[cfg(not(target_os = "macos"))]
pub fn order_front_without_activating(_window: &WebviewWindow) {}

#[cfg(not(target_os = "macos"))]
pub fn float_over_fullscreen(_window: &WebviewWindow) {}

/// Positions an AppKit `NSWindow` bottom-centred on the screen containing the mouse cursor
/// (or the main screen if the cursor is outside all displays).
///
/// # Safety
/// Must be called on the AppKit main thread with a valid `ns_window` pointer.
#[cfg(target_os = "macos")]
pub unsafe fn reposition_ns_window_on_active_screen(
    ns_window: *mut objc2::runtime::AnyObject,
    width: f64,
    height: f64,
    bottom_margin: f64,
) {
    use objc2::msg_send;
    use objc2::runtime::AnyClass;

    let event_cls = match AnyClass::get(core::ffi::CStr::from_bytes_with_nul(b"NSEvent\0").unwrap())
    {
        Some(cls) => cls,
        None => return,
    };
    let screen_cls =
        match AnyClass::get(core::ffi::CStr::from_bytes_with_nul(b"NSScreen\0").unwrap()) {
            Some(cls) => cls,
            None => return,
        };

    let pointer: CGPoint = msg_send![event_cls, mouseLocation];
    let screens: *mut objc2::runtime::AnyObject = msg_send![screen_cls, screens];
    let count: usize = msg_send![screens, count];

    let mut matched_screen: *mut objc2::runtime::AnyObject = std::ptr::null_mut();
    for i in 0..count {
        let s: *mut objc2::runtime::AnyObject = msg_send![screens, objectAtIndex: i];
        let f: CGRect = msg_send![s, frame];
        if pointer.x >= f.origin.x
            && pointer.x < f.origin.x + f.size.width
            && pointer.y >= f.origin.y
            && pointer.y < f.origin.y + f.size.height
        {
            matched_screen = s;
            break;
        }
    }
    if matched_screen.is_null() {
        matched_screen = msg_send![screen_cls, mainScreen];
    }
    if matched_screen.is_null() {
        return;
    }

    let visible: CGRect = msg_send![matched_screen, visibleFrame];
    // The whole frame, not just the origin: size is re-asserted too, so a
    // window that was resized or collapsed since build time comes back at its
    // designed dimensions on the next show.
    let frame = panel_frame(visible, width, height, bottom_margin);
    let _: () = msg_send![ns_window, setFrame: frame, display: true];
}

/// Where a `width x height` panel goes on a screen whose usable area is
/// `visible`: horizontally centred, `bottom_margin` above its bottom edge.
///
/// AppKit's origin is bottom-left, so `y` counts up from the area's bottom.
/// Clamped so a panel larger than the area stays inside it rather than being
/// placed past the area's origin.
#[cfg(target_os = "macos")]
pub fn panel_frame(visible: CGRect, width: f64, height: f64, bottom_margin: f64) -> CGRect {
    let x = visible.origin.x + ((visible.size.width - width) / 2.0).max(0.0);
    let room = (visible.size.height - height).max(0.0);
    let y = visible.origin.y + bottom_margin.min(room).max(0.0);
    CGRect {
        origin: CGPoint { x, y },
        size: CGSize { width, height },
    }
}

/// A read-back of the AppKit state that decides whether the overlay can be
/// seen: logged on every show, and checked against what we set at build time.
#[cfg(target_os = "macos")]
#[derive(Clone, Debug, PartialEq)]
pub struct PanelSnapshot {
    pub level: isize,
    pub visible: bool,
    pub on_active_space: bool,
    pub collection_behavior: usize,
    pub style_mask: usize,
    pub alpha: f64,
    pub frame: CGRect,
    /// The frame of the screen the panel is on, `None` when AppKit says it is
    /// on no screen at all — which is what an off-screen panel looks like.
    pub screen_frame: Option<CGRect>,
    /// `NSWindowOcclusionStateVisible` (1 << 1) set means some part is on screen.
    pub occlusion_visible: bool,
}

#[cfg(target_os = "macos")]
impl PanelSnapshot {
    /// What differs from the design *before* a show. The panel is hidden
    /// between runs on purpose, so "not visible" here is the normal state,
    /// not drift: flagging it made every first show after a hide log a
    /// warning and buried the real evidence for a "stopped appearing" report.
    pub fn drift_before_show(&self) -> Vec<&'static str> {
        self.problems()
            .into_iter()
            .filter(|p| *p != "not visible")
            .collect()
    }

    /// What differs from the overlay's designed configuration. Empty when the
    /// panel is as built. `on_active_space` is deliberately not a problem: it
    /// is false for a moment during a Space switch and means nothing then.
    pub fn problems(&self) -> Vec<&'static str> {
        let mut found = Vec::new();
        if self.level != STATUS_WINDOW_LEVEL {
            found.push("level changed");
        }
        if self.collection_behavior != floating_collection_behavior() {
            found.push("collection behaviour changed");
        }
        if self.style_mask & style_mask::NONACTIVATING_PANEL == 0 {
            found.push("not a non-activating panel");
        }
        if !self.visible {
            found.push("not visible");
        }
        if self.alpha < 1.0 {
            found.push("alpha below 1");
        }
        match self.screen_frame {
            None => found.push("on no screen"),
            Some(screen) if !rects_intersect(self.frame, screen) => {
                found.push("frame outside its screen")
            }
            Some(_) => {}
        }
        found
    }
}

#[cfg(target_os = "macos")]
fn rects_intersect(a: CGRect, b: CGRect) -> bool {
    a.origin.x < b.origin.x + b.size.width
        && b.origin.x < a.origin.x + a.size.width
        && a.origin.y < b.origin.y + b.size.height
        && b.origin.y < a.origin.y + a.size.height
}

/// Reads the panel back.
///
/// # Safety
/// Main thread, valid `ns_window`.
#[cfg(target_os = "macos")]
pub unsafe fn panel_snapshot(ns_window: *mut objc2::runtime::AnyObject) -> PanelSnapshot {
    use objc2::msg_send;

    let level: isize = msg_send![ns_window, level];
    let visible: bool = msg_send![ns_window, isVisible];
    let on_active_space: bool = msg_send![ns_window, isOnActiveSpace];
    let collection_behavior: usize = msg_send![ns_window, collectionBehavior];
    let style_mask: usize = msg_send![ns_window, styleMask];
    let alpha: f64 = msg_send![ns_window, alphaValue];
    let frame: CGRect = msg_send![ns_window, frame];
    let occlusion: usize = msg_send![ns_window, occlusionState];
    let screen: *mut objc2::runtime::AnyObject = msg_send![ns_window, screen];
    let screen_frame = if screen.is_null() {
        None
    } else {
        Some(msg_send![screen, frame])
    };
    PanelSnapshot {
        level,
        visible,
        on_active_space,
        collection_behavior,
        style_mask,
        alpha,
        frame,
        screen_frame,
        occlusion_visible: occlusion & (1 << 1) != 0,
    }
}

/// Positions `window` bottom-centred on the screen containing the mouse cursor
/// (or the main screen if the cursor is outside all displays).
#[cfg(target_os = "macos")]
pub fn reposition_on_active_screen(
    window: &WebviewWindow,
    width: f64,
    height: f64,
    bottom_margin: f64,
) {
    use objc2::runtime::AnyObject;

    let Ok(ptr) = window.ns_window() else {
        return;
    };
    if ptr.is_null() {
        return;
    }
    let ns_window = ptr as *mut AnyObject;

    unsafe {
        reposition_ns_window_on_active_screen(ns_window, width, height, bottom_margin);
    }
}

/// Finds the monitor that contains the user's mouse cursor, falling back to
/// the window's current monitor or the primary monitor.
pub fn target_monitor(window: &WebviewWindow) -> Option<tauri::Monitor> {
    if let Ok(cursor) = window.cursor_position() {
        if let Ok(monitors) = window.available_monitors() {
            for m in monitors {
                let pos = m.position();
                let size = m.size();
                if cursor.x >= pos.x as f64
                    && cursor.x < (pos.x + size.width as i32) as f64
                    && cursor.y >= pos.y as f64
                    && cursor.y < (pos.y + size.height as i32) as f64
                {
                    return Some(m);
                }
            }
        }
    }
    window
        .current_monitor()
        .ok()
        .flatten()
        .or_else(|| window.primary_monitor().ok().flatten())
}

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
        assert_eq!(mask & (1 << 6), 1 << 6, "IGNORES_CYCLE must be set");
        assert_eq!(
            mask & (1 << 18),
            1 << 18,
            "CAN_JOIN_ALL_APPLICATIONS must be set"
        );
        assert_eq!(mask, (1 << 0) | (1 << 4) | (1 << 6) | (1 << 8) | (1 << 18));
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

    #[cfg(target_os = "macos")]
    fn rect(x: f64, y: f64, w: f64, h: f64) -> CGRect {
        CGRect {
            origin: CGPoint { x, y },
            size: CGSize {
                width: w,
                height: h,
            },
        }
    }

    #[cfg(target_os = "macos")]
    fn healthy() -> PanelSnapshot {
        PanelSnapshot {
            level: STATUS_WINDOW_LEVEL,
            visible: true,
            on_active_space: true,
            collection_behavior: floating_collection_behavior(),
            style_mask: style_mask::NONACTIVATING_PANEL | style_mask::UTILITY_WINDOW,
            alpha: 1.0,
            frame: rect(100.0, 48.0, 560.0, 96.0),
            screen_frame: Some(rect(0.0, 0.0, 1440.0, 900.0)),
            occlusion_visible: true,
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_panel_as_built_has_no_problems() {
        assert!(healthy().problems().is_empty());
    }

    /// Each of these is a way the overlay can silently stop being seen. The
    /// snapshot has to name every one, because the log line it feeds is the
    /// only evidence a "stopped appearing until restart" report will have.
    #[cfg(target_os = "macos")]
    #[test]
    fn drift_in_any_visibility_property_is_named() {
        type Break = Box<dyn Fn(&mut PanelSnapshot)>;
        let cases: Vec<(&str, Break)> = vec![
            ("level changed", Box::new(|s| s.level = 3)),
            (
                "collection behaviour changed",
                // tao's own `visible_on_all_workspaces` mask: the one the
                // build-time comment says gets overwritten.
                Box::new(|s| s.collection_behavior = 1 << 0),
            ),
            ("not a non-activating panel", Box::new(|s| s.style_mask = 0)),
            ("not visible", Box::new(|s| s.visible = false)),
            ("alpha below 1", Box::new(|s| s.alpha = 0.0)),
            ("on no screen", Box::new(|s| s.screen_frame = None)),
            (
                "frame outside its screen",
                Box::new(|s| s.frame = rect(5000.0, 5000.0, 560.0, 96.0)),
            ),
        ];
        for (expected, break_it) in cases {
            let mut snapshot = healthy();
            break_it(&mut snapshot);
            assert!(
                snapshot.problems().contains(&expected),
                "expected {expected:?} in {:?}",
                snapshot.problems()
            );
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_panel_hidden_between_runs_is_not_drift_before_a_show() {
        let mut snapshot = healthy();
        snapshot.visible = false;
        assert!(snapshot.drift_before_show().is_empty());
        snapshot.level = 3;
        assert_eq!(snapshot.drift_before_show(), vec!["level changed"]);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn being_off_the_active_space_is_not_reported_as_drift() {
        let mut snapshot = healthy();
        snapshot.on_active_space = false;
        assert!(snapshot.problems().is_empty());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn panel_frame_is_centred_above_the_bottom_edge_at_full_size() {
        let f = panel_frame(rect(0.0, 0.0, 1440.0, 900.0), 560.0, 96.0, 48.0);
        assert_eq!(f, rect(440.0, 48.0, 560.0, 96.0));
    }

    /// A display change must put the panel on the *new* screen's coordinates,
    /// including a screen left of or below the primary (negative origin).
    #[cfg(target_os = "macos")]
    #[test]
    fn panel_frame_follows_a_screen_with_a_negative_origin() {
        let f = panel_frame(rect(-1920.0, -200.0, 1920.0, 1080.0), 560.0, 96.0, 48.0);
        assert_eq!(
            f.origin,
            CGPoint {
                x: -1920.0 + 680.0,
                y: -200.0 + 48.0
            }
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn panel_frame_keeps_an_oversized_panel_inside_the_area() {
        let f = panel_frame(rect(0.0, 0.0, 400.0, 80.0), 560.0, 96.0, 48.0);
        assert_eq!(f.origin, CGPoint { x: 0.0, y: 0.0 });
        assert_eq!(
            f.size,
            CGSize {
                width: 560.0,
                height: 96.0
            }
        );
    }

    /// Size is part of the frame: a panel that came back collapsed would be
    /// "visible" and invisible at once.
    #[cfg(target_os = "macos")]
    #[test]
    fn panel_frame_always_restores_the_designed_size() {
        let f = panel_frame(rect(0.0, 0.0, 1440.0, 900.0), 560.0, 96.0, 48.0);
        assert_eq!(
            f.size,
            CGSize {
                width: 560.0,
                height: 96.0
            }
        );
    }
}
