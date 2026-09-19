//! The prompt palette window, and the rules that decide what a key press does
//! to it.
//!
//! # The focus problem, which is the whole feature
//!
//! The dictation HUD (`crate::overlay`) sidesteps focus entirely: it is built
//! `focusable(false)` so AppKit refuses to promote it, which is what keeps the
//! caret — and the synthetic ⌘V — in the app the user is dictating into. The
//! palette cannot take that escape, because the user has to *type* into it. So
//! showing it deactivates the app they came from, and every ordering rule
//! below follows from that one fact:
//!
//! * The selection is read **before** the window appears. Once KEA is active,
//!   `capture_selection`'s synthetic ⌘C goes to the palette's own text field.
//!   The plan's Risks section suggests showing the window immediately with a
//!   "Reading selection…" state and capturing behind it; that is wrong at
//!   HEAD and would capture nothing at best. The ~150 ms of `COPY_SETTLE`
//!   before the window paints is the price of the feature working at all.
//! * The target app's pid is recorded before the window appears, for the same
//!   reason.
//! * Delivery re-activates the target app and **waits for the activation to
//!   land** (`crate::macfocus`) before anything is posted.
//! * The window is shown only once the webview says it has rendered the
//!   session (`palette_ready`), so it never appears holding the previous run's
//!   text.
//!
//! A non-activating `NSPanel` would avoid all of this, and Tauri 2 cannot
//! express one: `WebviewWindowBuilder` has no style mask, and the
//! non-activating bit is a style mask on `NSPanel`, not on tao's `NSWindow`.
//! The explicit restore below is deterministic and needs no private API.
//!
//! # Manual verification (macOS)
//! 1. Select a sentence in TextEdit, press the palette shortcut. The window
//!    appears centred near the top; TextEdit's selection is still highlighted
//!    in the secondary colour; the preview shows the sentence.
//! 2. Type an instruction, press Return: the window disappears, TextEdit comes
//!    forward, and the selection is replaced.
//! 3. Repeat with ⌘Return (inserts at the caret) and ⇧⌘Return (clipboard only,
//!    nothing typed).
//! 4. Press the shortcut with nothing selected: the preview says so and Return
//!    inserts the answer at the caret.
//! 5. Press Escape before typing, and again mid-request: nothing is inserted,
//!    the clipboard is untouched, and TextEdit is frontmost again.
//! 6. Click into another app while the palette is up: it closes and that app
//!    keeps focus — it must NOT be yanked back to the original one.
//! 7. Open the palette from a full-screen app and from a second Space.

use serde::{Deserialize, Serialize};
use tauri::{
    AppHandle, Manager, PhysicalPosition, WebviewUrl, WebviewWindow, WebviewWindowBuilder,
};

use crate::nswindow::{float_over_fullscreen, top_centre_position};

/// Window label the frontend matches on to render the palette.
pub const LABEL: &str = "palette";

/// Wide enough for a line of prose at the body size without wrapping every
/// few words, narrow enough not to read as a dialog.
const WIDTH: f64 = 640.0;

/// The window is a fixed size and `transparent(true)`, so the unused part is
/// invisible. Fixed rather than grown to fit: resizing a visible always-on-top
/// window as the preview or an error appears is a jump, and the palette is on
/// screen for seconds.
const HEIGHT: f64 = 300.0;

/// How far down the work area the window's top edge sits, as a fraction.
///
/// Not bottom-centre like the HUD and not vertically centred: a palette is
/// looked at, and the eye goes to the upper third. This is where Spotlight and
/// every tool modelled on it put theirs, and matching that is worth more than
/// any argument from first principles.
const TOP_FRACTION: f64 = 0.24;

// ---------------------------------------------------------------------------
// Session shape
// ---------------------------------------------------------------------------

/// Where a session's source text came from. Absent source text is not a third
/// origin — a selection session with nothing selected is the ordinary "ask KEA
/// anything" case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteOrigin {
    /// Whatever was selected in the frontmost app when the shortcut was hit.
    Selection,
    /// Text recognised from a screen region (item 20). There is nothing to
    /// replace, so [`PaletteDelivery::Replace`] is not offered.
    ScreenCapture,
}

impl PaletteOrigin {
    pub fn as_str(self) -> &'static str {
        match self {
            PaletteOrigin::Selection => "selection",
            PaletteOrigin::ScreenCapture => "screen_capture",
        }
    }
}

/// Where the answer goes. Chosen by *which key submits*, so the user never
/// pays a second focus round trip to be asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PaletteDelivery {
    /// Over the selection the session captured, after verifying it is still
    /// there.
    Replace,
    /// At the caret, replacing nothing.
    Insert,
    /// Onto the clipboard, typing nothing anywhere. The only delivery that
    /// needs neither Accessibility nor a live target app.
    Copy,
}

impl PaletteDelivery {
    pub fn as_str(self) -> &'static str {
        match self {
            PaletteDelivery::Replace => "replace",
            PaletteDelivery::Insert => "insert",
            PaletteDelivery::Copy => "copy",
        }
    }

    // Not `FromStr`: the caller wants an `Option`, not a `Result`.
    #[allow(clippy::should_implement_trait)]
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "replace" => Some(PaletteDelivery::Replace),
            "insert" => Some(PaletteDelivery::Insert),
            "copy" => Some(PaletteDelivery::Copy),
            _ => None,
        }
    }
}

/// Which deliveries a session can offer, and which one Return picks.
///
/// Derived once, in one pure function, because three places need the same
/// answer and must not disagree: the footer hint row, the key handler, and the
/// backend's own validation of whatever the frontend asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeliveryOptions {
    pub can_replace: bool,
    pub can_insert: bool,
    pub default: PaletteDelivery,
}

impl DeliveryOptions {
    /// `has_source` is whether the session captured any text to act on;
    /// `can_type` is whether KEA can post synthetic keystrokes at all —
    /// Accessibility granted and secure input off.
    pub fn resolve(origin: PaletteOrigin, has_source: bool, can_type: bool) -> Self {
        // Replace needs something to replace *and* a selection that replacing
        // is meaningful for. An OCR result came off the screen, not out of a
        // text field: the app that is frontmost at the end may be a PDF viewer
        // with no editable field at all, so typing over "the selection" there
        // would destroy whatever happens to be selected.
        let can_replace = can_type && has_source && origin == PaletteOrigin::Selection;
        let default = match origin {
            _ if can_replace => PaletteDelivery::Replace,
            // A screen capture has no origin to return to. The app in front
            // when the answer lands is whatever was behind the crosshair — a
            // PDF viewer, a video call — and typing a paragraph into it is a
            // worse default than a clipboard the user has to paste from.
            // Insert stays on Cmd+Return for when they do want it.
            PaletteOrigin::ScreenCapture => PaletteDelivery::Copy,
            // Nothing selected, but the user was typing somewhere: the caret
            // is exactly where they asked from.
            PaletteOrigin::Selection if can_type => PaletteDelivery::Insert,
            // Nothing can be typed anywhere. The clipboard always works and
            // never destroys anything, so it is the floor.
            PaletteOrigin::Selection => PaletteDelivery::Copy,
        };
        Self {
            can_replace,
            can_insert: can_type,
            default,
        }
    }

    /// Whether this session may deliver `delivery` at all. The frontend picks
    /// from the same options, so a rejection here means a bug or a stale
    /// window rather than user error.
    pub fn allows(&self, delivery: PaletteDelivery) -> bool {
        match delivery {
            PaletteDelivery::Replace => self.can_replace,
            PaletteDelivery::Insert => self.can_insert,
            PaletteDelivery::Copy => true,
        }
    }
}

// ---------------------------------------------------------------------------
// The state machine
// ---------------------------------------------------------------------------

/// What the palette is doing, as far as the rules below care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteState {
    Closed,
    /// On screen, waiting for an instruction.
    Open,
    /// On screen with a request in flight.
    Running,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteEvent {
    /// The global shortcut fired.
    Hotkey,
    /// Escape, from the webview's own key handler — never a global
    /// accelerator, which would take Escape away from every other app.
    Escape,
    /// The window lost focus.
    Blur,
    Submit {
        instruction_empty: bool,
    },
    /// A request finished. `stale` means the session it belonged to is gone.
    Completed {
        stale: bool,
    },
}

impl PaletteEvent {
    /// Whether a dismissal caused by this event should hand focus back to the
    /// app the palette took it from.
    ///
    /// **Blur must not.** The user clicked into some other app: that app is
    /// where they want to be, and reactivating the original one would yank
    /// them out of it. Escape and the toggle shortcut are the opposite — the
    /// user never left, and KEA is holding focus it borrowed.
    pub fn restores_focus(self) -> bool {
        !matches!(self, PaletteEvent::Blur)
    }
}

/// What the app layer should do about an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaletteReaction {
    /// Capture context and selection, then open.
    Open,
    /// Close, deliver nothing. No request was in flight.
    Dismiss,
    /// Close, deliver nothing, and discard the in-flight result when it lands.
    Cancel,
    /// Start the request.
    Run,
    /// Put the finished text where the user asked.
    Deliver,
    /// Throw the finished text away — including *not* putting it on the
    /// clipboard. A cancelled request that silently replaces the clipboard is
    /// a surprise, and the clipboard is the one piece of state this feature
    /// borrows and has to give back.
    Discard,
    /// Nothing to do.
    Ignore,
}

/// The whole of the palette's control flow, as a pure function.
///
/// Every awkward case this feature has is a row here rather than a condition
/// buried in an async handler: a second shortcut press, Escape mid-request, a
/// result arriving after a dismissal, an empty submit. Same reason
/// `kea_platform::hotkeys::hold` keeps the double-tap rules in one testable
/// file.
pub fn palette_event(state: PaletteState, event: PaletteEvent) -> PaletteReaction {
    use PaletteEvent as E;
    use PaletteReaction as R;
    use PaletteState as S;

    match (state, event) {
        // A result whose session is gone is discarded from any state — that is
        // what "stale" means, and checking it first keeps the rest readable.
        (_, E::Completed { stale: true }) => R::Discard,
        (S::Running, E::Completed { stale: false }) => R::Deliver,
        // Not running but a live result arrived: the session was replaced
        // between the check and here. Never deliver on a guess.
        (_, E::Completed { stale: false }) => R::Discard,

        (S::Closed, E::Hotkey) => R::Open,
        // The shortcut toggles. A second session while one is open would
        // capture KEA's own (empty) selection and strand the first one's
        // busy guard.
        (S::Open, E::Hotkey) => R::Dismiss,
        (S::Running, E::Hotkey) => R::Cancel,

        (S::Open, E::Escape | E::Blur) => R::Dismiss,
        (S::Running, E::Escape | E::Blur) => R::Cancel,
        (S::Closed, E::Escape | E::Blur) => R::Ignore,

        // An empty instruction is a stray Return, not a request: Ask KEA
        // cannot render its template without one, so running would spend a
        // provider call to be told so.
        (S::Open, E::Submit { instruction_empty }) => {
            if instruction_empty {
                R::Ignore
            } else {
                R::Run
            }
        }
        (S::Running | S::Closed, E::Submit { .. }) => R::Ignore,
    }
}

// ---------------------------------------------------------------------------
// The window
// ---------------------------------------------------------------------------

/// Creates the palette window, hidden.
///
/// Built at startup beside the overlay so opening it is a `show()` and not a
/// page load — the shortcut is on the critical path of someone's attention.
///
/// `focusable(true)` is the one line that differs from
/// [`crate::overlay::create`], and it is the reason this module's header is as
/// long as it is.
pub fn create(app: &AppHandle) -> tauri::Result<WebviewWindow> {
    let window = WebviewWindowBuilder::new(app, LABEL, WebviewUrl::App("index.html".into()))
        .title("KEA prompt palette")
        .inner_size(WIDTH, HEIGHT)
        .decorations(false)
        .transparent(true)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .shadow(true)
        .visible(false)
        .focused(false)
        .focusable(true)
        // The first click has to land on the control it was aimed at: the
        // palette is up for a couple of seconds and a swallowed click on the
        // input would read as the window being dead.
        .accept_first_mouse(true)
        // The palette displays whatever the user had selected. A screen
        // recorder or a shared Zoom window should not capture it.
        .content_protected(true)
        .visible_on_all_workspaces(true)
        .build()?;

    // Must come after build(): it addresses the NSWindow, which does not exist
    // until then, and it overwrites the collection behaviour that
    // `visible_on_all_workspaces(true)` set above.
    float_over_fullscreen(&window);
    reposition(&window);
    Ok(window)
}

/// Pins the palette near the top centre of the primary monitor's work area.
///
/// Re-run on every open: the user may have changed displays, resolution or
/// dock position since the window was built.
pub fn reposition(window: &WebviewWindow) {
    let Ok(Some(monitor)) = window.primary_monitor() else {
        return;
    };
    let scale = monitor.scale_factor();
    let area = monitor.work_area();
    let (x, y) = top_centre_position(
        area.position.x,
        area.position.y,
        area.size.width,
        area.size.height,
        (WIDTH * scale).round() as i32,
        (HEIGHT * scale).round() as i32,
        (area.size.height as f64 * TOP_FRACTION).round() as i32,
    );
    let _ = window.set_position(PhysicalPosition::new(x, y));
}

/// Brings the palette on screen and gives it the keyboard.
///
/// `set_focus` is not redundant with `show`: `show` orders the window front,
/// and KEA may not be the active application at all — the shortcut was pressed
/// while the user was in someone else's app. Without it the palette appears
/// and swallows nothing the user types.
pub fn show(app: &AppHandle) {
    let Some(window) = app.get_webview_window(LABEL) else {
        tracing::warn!("the palette window does not exist; nothing to show");
        return;
    };
    reposition(&window);
    let _ = window.show();
    let _ = window.set_focus();
}

/// Takes the palette off screen. Called before any synthetic keystroke, so the
/// window is gone before the target app comes forward.
pub fn hide(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(LABEL) {
        let _ = window.hide();
    }
}

#[cfg(test)]
mod tests {
    use super::PaletteDelivery as D;
    use super::PaletteEvent as E;
    use super::PaletteOrigin as O;
    use super::PaletteReaction as R;
    use super::PaletteState as S;
    use super::*;

    #[test]
    fn a_second_shortcut_press_closes_rather_than_opening_a_second_session() {
        assert_eq!(palette_event(S::Closed, E::Hotkey), R::Open);
        assert_eq!(palette_event(S::Open, E::Hotkey), R::Dismiss);
        assert_eq!(palette_event(S::Running, E::Hotkey), R::Cancel);
    }

    #[test]
    fn escape_while_running_cancels_and_delivers_nothing() {
        assert_eq!(palette_event(S::Running, E::Escape), R::Cancel);
        // ...and the result that lands afterwards is thrown away, not pasted
        // into whatever the user moved on to.
        assert_eq!(
            palette_event(S::Closed, E::Completed { stale: true }),
            R::Discard
        );
    }

    #[test]
    fn a_stale_result_is_discarded_from_every_state() {
        for state in [S::Closed, S::Open, S::Running] {
            assert_eq!(
                palette_event(state, E::Completed { stale: true }),
                R::Discard,
                "{state:?}"
            );
        }
    }

    #[test]
    fn a_live_result_is_delivered_only_while_running() {
        assert_eq!(
            palette_event(S::Running, E::Completed { stale: false }),
            R::Deliver
        );
        // Not running and not stale means the session changed underneath;
        // delivering on that guess is how text lands in the wrong document.
        assert_eq!(
            palette_event(S::Open, E::Completed { stale: false }),
            R::Discard
        );
    }

    #[test]
    fn blur_dismisses_and_escape_dismisses() {
        assert_eq!(palette_event(S::Open, E::Blur), R::Dismiss);
        assert_eq!(palette_event(S::Open, E::Escape), R::Dismiss);
        // Hiding the window fires a blur of its own; with no session open that
        // must be a no-op rather than a second dismissal.
        assert_eq!(palette_event(S::Closed, E::Blur), R::Ignore);
    }

    #[test]
    fn an_empty_instruction_is_ignored() {
        assert_eq!(
            palette_event(
                S::Open,
                E::Submit {
                    instruction_empty: true
                }
            ),
            R::Ignore
        );
        assert_eq!(
            palette_event(
                S::Open,
                E::Submit {
                    instruction_empty: false
                }
            ),
            R::Run
        );
    }

    #[test]
    fn a_second_submit_while_running_is_ignored() {
        assert_eq!(
            palette_event(
                S::Running,
                E::Submit {
                    instruction_empty: false
                }
            ),
            R::Ignore
        );
    }

    #[test]
    fn only_blur_gives_up_the_focus_restore() {
        // Clicking into another app is a choice about where to be; Escape is
        // not. Getting this backwards yanks the user out of the app they just
        // switched to.
        assert!(!E::Blur.restores_focus());
        assert!(E::Escape.restores_focus());
        assert!(E::Hotkey.restores_focus());
    }

    #[test]
    fn a_selection_defaults_to_replacing_it() {
        let opts = DeliveryOptions::resolve(O::Selection, true, true);
        assert_eq!(opts.default, D::Replace);
        assert!(opts.can_replace && opts.can_insert);
        assert!(opts.allows(D::Copy));
    }

    #[test]
    fn nothing_selected_defaults_to_inserting_at_the_caret() {
        let opts = DeliveryOptions::resolve(O::Selection, false, true);
        assert_eq!(opts.default, D::Insert);
        assert!(!opts.can_replace);
        assert!(!opts.allows(D::Replace));
    }

    #[test]
    fn a_screen_capture_never_offers_replace() {
        // There is no selection behind an OCR result; the app in front at the
        // end may not even have a text field.
        let opts = DeliveryOptions::resolve(O::ScreenCapture, true, true);
        assert!(!opts.can_replace);
        assert!(!opts.allows(D::Replace));
        // And Return copies rather than typing: the app in front at the end is
        // whatever was behind the crosshair, which may not take text at all.
        assert_eq!(opts.default, D::Copy);
        // Insert is still on the menu, just not on the default key.
        assert!(opts.can_insert);
        assert!(opts.allows(D::Insert));
    }

    #[test]
    fn without_the_ability_to_type_everything_falls_back_to_the_clipboard() {
        // No Accessibility grant, or secure input is on somewhere. Copy is the
        // one delivery that still works, and it destroys nothing.
        let opts = DeliveryOptions::resolve(O::Selection, true, false);
        assert_eq!(opts.default, D::Copy);
        assert!(!opts.can_replace && !opts.can_insert);
        assert!(opts.allows(D::Copy));
    }

    #[test]
    fn copy_is_always_allowed() {
        for origin in [O::Selection, O::ScreenCapture] {
            for has_source in [true, false] {
                for can_type in [true, false] {
                    assert!(DeliveryOptions::resolve(origin, has_source, can_type).allows(D::Copy));
                }
            }
        }
    }

    #[test]
    fn delivery_round_trips_through_its_string() {
        for delivery in [D::Replace, D::Insert, D::Copy] {
            assert_eq!(D::from_str(delivery.as_str()), Some(delivery));
        }
        assert_eq!(D::from_str("paste"), None);
    }

    #[test]
    fn origin_strings_match_the_wire_format() {
        // The frontend switches its badge on these, and serde derives them
        // from the same variant names.
        assert_eq!(O::Selection.as_str(), "selection");
        assert_eq!(
            serde_json::to_string(&O::ScreenCapture).unwrap(),
            format!("\"{}\"", O::ScreenCapture.as_str())
        );
    }

    #[test]
    #[allow(clippy::assertions_on_constants)]
    fn the_window_sits_in_the_upper_third() {
        // Centred or lower would put it where the user is looking *at* their
        // document rather than where they look for a command bar.
        assert!(TOP_FRACTION > 0.1 && TOP_FRACTION < 0.35);
    }
}
