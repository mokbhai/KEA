//! Giving the keyboard back to the app the user came from.
//!
//! # Why this exists in the app layer
//!
//! The prompt palette is the one KEA window that *takes* focus — it has a text
//! field, so it must — and everything it does afterwards depends on being able
//! to put focus back. A name is not enough to do that with: two Chrome
//! profiles are both "Google Chrome". The handle that works is the pid, and
//! `NSRunningApplication(processIdentifier:)` is the only reliable way to
//! reactivate one.
//!
//! **This belongs in `kea_platform`, next to `AppContext`.** Item 11 shipped
//! `AppContext` with `bundle_id`, `app_name`, `window_title` and `url` but no
//! pid: `macos_ax::FrontmostApp` reads one and keeps it `pub(crate)`. Rather
//! than change a struct other crates construct, the palette reads the pid
//! again here, in the same idiom as
//! `crates/platform/src/textio/macos_ax.rs::frontmost_running_app`. When
//! `AppContext` grows a pid and an `activate()`, this module deletes.
//!
//! # Manual verification (macOS)
//! 1. Select text in TextEdit, open the palette, press Escape: TextEdit is
//!    frontmost again and the selection is still highlighted.
//! 2. Repeat from a full-screen Safari window and from a second Space.
//! 3. Open the palette from TextEdit, quit TextEdit while the palette is up,
//!    then submit: activation fails, and the result lands on the clipboard
//!    with a message saying so rather than being typed into whatever is now
//!    frontmost.
//! 4. With two Chrome windows open, run a rewrite from the second one: the
//!    window that had the selection is the one that comes forward (see the
//!    known limitation on [`activate_and_wait`]).

use std::time::{Duration, Instant};

/// How long to wait for an activation to actually land.
///
/// A poll, not a sleep. `PASTE_SETTLE` in the platform layer is 400 ms for the
/// same reason — synthetic events and activations are both asynchronous — and
/// a fixed sleep is either too short under load or wasted every single time.
pub const ACTIVATE_DEADLINE: Duration = Duration::from_millis(400);

/// Gap between activation checks. Short enough that the usual case (a few
/// milliseconds) costs one or two iterations.
const ACTIVATE_POLL: Duration = Duration::from_millis(10);

/// What happened when the palette tried to give focus back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reactivation {
    /// The target app is frontmost again. Safe to post synthetic keystrokes.
    Active,
    /// It was asked and never became active: it quit, it is hung, or the
    /// machine slept. **Callers must not paste** — the keystroke would land in
    /// whatever *is* frontmost.
    Failed,
    /// There was nothing to reactivate (no pid captured, or not macOS).
    Unknown,
}

impl Reactivation {
    /// Whether it is safe to deliver text into the frontmost app.
    pub fn can_deliver(self) -> bool {
        matches!(self, Reactivation::Active)
    }
}

/// The pid of the frontmost application, from `NSWorkspace`.
///
/// Deliberately *not* read from AX: `AXFocusedApplication` needs the
/// Accessibility grant, and the palette has to be able to hand focus back even
/// when it could not read the selection in the first place.
#[cfg(target_os = "macos")]
pub fn frontmost_pid() -> Option<i32> {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    // SAFETY: `sharedWorkspace` and `frontmostApplication` return shared,
    // autoreleased objects that outlive the call, and `processIdentifier` is a
    // plain `pid_t` getter on `NSRunningApplication`. The class lookup answers
    // `None` rather than a dangling pointer when AppKit is not loaded, which
    // is what keeps a headless `cargo test` run from faulting here.
    unsafe {
        let class = AnyClass::get(c"NSWorkspace")?;
        let workspace: *mut AnyObject = msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return None;
        }
        let app: *mut AnyObject = msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return None;
        }
        let pid: i32 = msg_send![app, processIdentifier];
        // pid 0 is the kernel and is never an app; treat it as "no answer"
        // rather than handing out a pid nothing can be done with.
        (pid > 0).then_some(pid)
    }
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_pid() -> Option<i32> {
    None
}

/// Activates `pid` and blocks until it is actually frontmost, or the deadline
/// passes.
///
/// Blocking on purpose: every caller is already on a blocking task, and the
/// alternative — an async sleep loop — buys nothing when the whole thing is
/// bounded at [`ACTIVATE_DEADLINE`] and the next step cannot start regardless.
///
/// **Known limitation.** This activates the *application*. In a multi-window
/// app the window that comes forward is whichever was ordered front, which is
/// usually but not always the one that had the selection. The palette's
/// verify-before-replace step is the backstop for when it is not: it compares
/// the re-read selection against what it captured and refuses to replace text
/// it does not recognise.
#[cfg(target_os = "macos")]
pub fn activate_and_wait(pid: i32, deadline: Duration) -> Reactivation {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    /// `NSApplicationActivateIgnoringOtherApps`. Deprecated since macOS 14 in
    /// favour of `activate(from:)`, which requires the *calling* app to be
    /// active — which KEA is here, because the palette took focus, so this is
    /// the well-behaved case rather than the blocked one. Kept because it is
    /// the one spelling that works on every supported version; the deprecation
    /// is a documentation state, not a removal.
    const ACTIVATE_IGNORING_OTHER_APPS: usize = 1 << 1;

    let Some(class) = AnyClass::get(c"NSRunningApplication") else {
        return Reactivation::Unknown;
    };

    let started = Instant::now();
    loop {
        // Re-fetched each iteration rather than held: the object is autoreleased,
        // and a pool drain between polls would leave a dangling pointer.
        // `NSRunningApplication` is documented thread-safe, which is what lets
        // this run off the main thread alongside the rest of the paste path.
        let state = unsafe {
            let app: *mut AnyObject =
                msg_send![class, runningApplicationWithProcessIdentifier: pid];
            if app.is_null() {
                // The app quit while the palette was up.
                return Reactivation::Failed;
            }
            let active: bool = msg_send![app, isActive];
            if active {
                Some(true)
            } else {
                let _: bool = msg_send![app, activateWithOptions: ACTIVATE_IGNORING_OTHER_APPS];
                None
            }
        };
        if state == Some(true) {
            return Reactivation::Active;
        }
        if started.elapsed() >= deadline {
            return Reactivation::Failed;
        }
        std::thread::sleep(ACTIVATE_POLL);
    }
}

#[cfg(not(target_os = "macos"))]
pub fn activate_and_wait(_pid: i32, _deadline: Duration) -> Reactivation {
    Reactivation::Unknown
}

/// Reactivates the app the palette took focus from, if one was recorded.
pub fn restore_focus(pid: Option<i32>) -> Reactivation {
    match pid {
        Some(pid) => activate_and_wait(pid, ACTIVATE_DEADLINE),
        None => Reactivation::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_active_target_may_receive_text() {
        // The whole point of the type: `Unknown` must not be optimistically
        // treated as "probably fine", because a paste then lands wherever the
        // user happens to be.
        assert!(Reactivation::Active.can_deliver());
        assert!(!Reactivation::Failed.can_deliver());
        assert!(!Reactivation::Unknown.can_deliver());
    }

    #[test]
    fn no_pid_is_not_an_activation() {
        assert_eq!(restore_focus(None), Reactivation::Unknown);
    }

    /// A pid nothing owns must fail fast rather than spin for the deadline;
    /// `runningApplicationWithProcessIdentifier:` answers nil immediately.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_dead_pid_fails_without_waiting() {
        let started = Instant::now();
        // Max pid + 1 on macOS is not assignable, so nothing can own it.
        assert_eq!(
            activate_and_wait(i32::MAX, ACTIVATE_DEADLINE),
            Reactivation::Failed
        );
        assert!(started.elapsed() < ACTIVATE_DEADLINE);
    }

    /// Reads whatever is frontmost on the machine running the test. Nothing
    /// asserts a value — the point is that the AppKit round trip is safe from
    /// a test thread and returns either a usable pid or nothing at all.
    #[test]
    fn frontmost_pid_is_a_real_pid_or_nothing() {
        if let Some(pid) = frontmost_pid() {
            assert!(pid > 0);
        }
    }
}
