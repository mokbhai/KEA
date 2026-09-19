//! Keeping the process responsive while it is in the background.
//!
//! # Why this exists
//!
//! macOS App Nap throttles an application that has no visible windows and no
//! user interaction — which is KEA's normal state, because the whole product
//! is a background utility driven by global hotkeys. A napped process gets its
//! timers coalesced and its run loops starved.
//!
//! That is fatal to a `CGEventTap`. The tap's callback has a deadline; miss it
//! and the system does not slow the tap down, it *disables* it
//! (`kCGEventTapDisabledByTimeout`) and the app stops seeing key events until
//! something re-enables it. The symptom is precise and was reported exactly
//! this way: the ⌥⇧ hold chord does nothing while another app is focused,
//! while `Cmd+Shift+D` keeps working — because that one is a Carbon
//! `RegisterEventHotKey`, which the OS delivers without running our code.
//!
//! The log from a real session shows the tap being disabled and re-enabled
//! every thirty to sixty seconds, which is the shape of a process being napped
//! and woken rather than of a callback that is slow on its own.
//!
//! # What is asserted, and what deliberately is not
//!
//! `NSActivityUserInitiatedAllowingIdleSystemSleep` is the narrowest option
//! that disables App Nap. The plain `NSActivityUserInitiated` would also
//! prevent the *system* from idle-sleeping, which a dictation utility has no
//! business doing — the user closing their laptop should still close it.
//!
//! `NSActivityLatencyCritical` is deliberately NOT used. It additionally
//! disables timer coalescing, which is a real battery cost for a process that
//! is idle almost all of the time. If tap timeouts survive this change, that
//! is the next lever, and it should be pulled with the log as evidence rather
//! than pre-emptively.
//!
//! # Manual verification
//! 1. Launch KEA and switch to another application.
//! 2. Leave it for a few minutes, then press and hold ⌥⇧.
//! 3. Dictation should start. Before this change, the log would show
//!    "the system disabled the event tap" and the chord would be swallowed.

#[cfg(target_os = "macos")]
mod imp {
    use objc2::msg_send;
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2_foundation::NSString;

    /// `NSActivityUserInitiatedAllowingIdleSystemSleep`.
    ///
    /// `NSActivityUserInitiated` is this bit set plus
    /// `NSActivityIdleSystemSleepDisabled`; we want the former without the
    /// latter, which is what this constant is for.
    const USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP: u64 = 0x00FF_FFFF;

    /// Asks macOS not to nap this process. Returns whether the activity began.
    ///
    /// The returned token is deliberately LEAKED rather than handed back as a
    /// guard. The activity has to last for the life of the process, and
    /// `NSProcessInfo` ends it when the token deallocates — so a guard is a
    /// liability here, not a safety feature: any caller that stored it
    /// somewhere droppable, or any refactor that stopped holding it, would
    /// silently reintroduce a bug whose only symptom is a hotkey that stops
    /// working after a few minutes in the background. Leaking one object once
    /// is the honest encoding of "this lasts forever".
    ///
    /// It is also why this returns `bool` rather than a token the caller must
    /// remember to keep: there is nothing for them to get wrong.
    ///
    /// `false` is not fatal — it means the tap may be disabled more often, and
    /// it re-enables itself when it sees the next event.
    pub fn disable_app_nap() -> bool {
        // SAFETY: NSProcessInfo is present on every supported macOS, and
        // `beginActivityWithOptions:reason:` returns an autoreleased object we
        // retain for the life of the process.
        unsafe {
            let Some(class) = AnyClass::get(c"NSProcessInfo") else {
                return false;
            };
            let process_info: *mut AnyObject = msg_send![class, processInfo];
            if process_info.is_null() {
                return false;
            }
            let reason = NSString::from_str(
                "KEA watches for its global hold-to-talk chord with a CGEventTap, \
                 which the system disables if this process is throttled.",
            );
            let token: *mut AnyObject = msg_send![
                process_info,
                beginActivityWithOptions: USER_INITIATED_ALLOWING_IDLE_SYSTEM_SLEEP,
                reason: &*reason,
            ];
            match Retained::retain(token) {
                // Into the void on purpose — see the doc comment.
                Some(token) => {
                    std::mem::forget(token);
                    true
                }
                None => false,
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    /// App Nap is a macOS concept; elsewhere there is nothing to suppress.
    pub fn disable_app_nap() -> bool {
        false
    }
}

pub use imp::disable_app_nap;
