//! macOS app-context probe: NSWorkspace for identity, AX for contents.
//!
//! The split is the point. Identity (`bundle_id`, `app_name`, the pid) comes
//! from `NSRunningApplication`, which needs no permission and answers whenever
//! there is a GUI session. Only the *contents* of that app's window come from
//! Accessibility. Keying a profile off `AXFocusedApplication` instead would
//! make bundle-id rules silently stop matching the moment the Accessibility
//! grant lapses — exactly when the user is least able to work out why.
//!
//! # Manual verification
//! 1. With Accessibility revoked, focus Slack and capture: `bundle_id` and
//!    `app_name` are populated, `window_title` and `url` are `None`.
//! 2. Grant Accessibility, enable `profiles.capture_window_title`, focus
//!    TextEdit with a saved document: `window_title` is the file name.
//! 3. Enable `profiles.capture_url` and focus a Safari/Chrome tab: see the
//!    AXDocument caveat below before trusting the result.

use super::{AppContext, AppContextProbe, CaptureOpts};
use crate::textio::macos_ax::{self, AxRef};

pub struct MacAppContextProbe;

impl MacAppContextProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacAppContextProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl AppContextProbe for MacAppContextProbe {
    fn capture(&self, opts: CaptureOpts) -> AppContext {
        let Some(app) = macos_ax::frontmost_app() else {
            // No GUI session, or AppKit is not loaded. Not an error: the
            // caller treats an empty context as "no profile applies".
            return AppContext::default();
        };
        let mut ctx = AppContext {
            bundle_id: app.bundle_id,
            app_name: app.name,
            window_title: None,
            url: None,
        };

        // Both remaining fields are opt-in and both need Accessibility, so
        // with the defaults this returns here and never crosses a process
        // boundary at all.
        if !opts.needs_accessibility() || !macos_ax::is_ax_trusted() {
            return ctx;
        }

        // SAFETY: every element below is a +1 reference from an AX
        // create/copy function, handed to `AxRef`, which releases each exactly
        // once. This runs on every dictation and every rewrite, so a
        // hand-rolled CFRelease path here is how a leak gets into a process
        // that stays open all day.
        unsafe {
            let Some(app_element) = macos_ax::app_ax_element(app.pid) else {
                return ctx;
            };
            // `app_ax_element` capped the first hop. The timeout is per AX
            // object and is not inherited by elements copied out of one, so
            // the window gets its own cap before anything is read from it —
            // otherwise a beachballed Slack holds the hotkey thread for the
            // (generous) system default on the very next line.
            let Some(window) = app_element.copy_attr("AXFocusedWindow") else {
                return ctx;
            };
            macos_ax::set_ax_timeout(&window);
            if opts.window_title {
                ctx.window_title = window.copy_string_attr("AXTitle");
            }
            if opts.url {
                ctx.url = document_url(&window);
            }
        }
        ctx
    }
}

/// The URL of the document or page in `window`, via `AXDocument`.
///
/// Two places are tried because browsers disagree about which element carries
/// it: the focused window, then the focused element (typically the web area).
///
/// **Per-browser behaviour is not guaranteed and must be probed with a real
/// binary before it is promised in the UI.** Chromium only builds its full
/// accessibility tree once an assistive client has poked it; Safari and Firefox
/// differ over whether the value sits on the window or on the web area; and
/// Arc/Brave/Edge inherit Chromium's behaviour but not necessarily its
/// version. A `None` here is an ordinary answer, not a bug.
///
/// **The AppleScript fallback is deliberately not implemented.**
/// `tell application "Google Chrome" to get URL of active tab of front window`
/// works where `AXDocument` does not, and it is the wrong default: it needs
/// Apple Events permission (`NSAppleEventsUsageDescription`, absent from
/// `Info.plist` today — without it the process is *killed* on first use rather
/// than denied), the TCC grant is per target app so every browser the user
/// ever focuses raises its own "KEA wants to control …" dialog, it is a
/// synchronous Apple Event round trip of tens to hundreds of milliseconds on
/// the hotkey path, and sending Apple Events to arbitrary apps is the kind of
/// capability that makes a notarization review question a build. If it is ever
/// added it must be per-browser and explicitly enabled, not a silent fallback.
///
/// # Safety
/// `window` must be a live AX element.
unsafe fn document_url(window: &AxRef) -> Option<String> {
    if let Some(url) = window.copy_string_attr("AXDocument") {
        return Some(url);
    }
    // The system-wide element's own timeout is left alone on purpose: it is
    // process-global. The focused element gets its own cap instead.
    let system = macos_ax::system_wide_element()?;
    let focused = system.copy_attr("AXFocusedUIElement")?;
    macos_ax::set_ax_timeout(&focused);
    focused.copy_string_attr("AXDocument")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Identity must not depend on the Accessibility grant: a bundle-id rule
    /// has to keep firing when the grant lapses, which is the whole reason
    /// identity comes from NSWorkspace and only contents come from AX.
    #[test]
    fn identity_is_unaffected_by_the_accessibility_grant() {
        let probe = MacAppContextProbe::new();
        let opts = CaptureOpts {
            window_title: true,
            url: true,
        };

        let untrusted = {
            let _trust = macos_ax::AxTrustOverride::force(false);
            probe.capture(opts)
        };
        // Whatever is frontmost on the test machine is not ours to predict,
        // so the assertion is on the invariant, not on a value.
        assert_eq!(untrusted.window_title, None);
        assert_eq!(untrusted.url, None);

        let trusted = probe.capture(opts);
        assert_eq!(trusted.bundle_id, untrusted.bundle_id);
        assert_eq!(trusted.app_name, untrusted.app_name);
    }

    #[test]
    fn opting_out_skips_accessibility_entirely() {
        // With both fields off the probe must not read AX at all, so forcing
        // trust on cannot change the answer.
        let _trust = macos_ax::AxTrustOverride::force(true);
        let ctx = MacAppContextProbe::new().capture(CaptureOpts::default());
        assert_eq!(ctx.window_title, None);
        assert_eq!(ctx.url, None);
    }

    #[test]
    fn capture_is_repeatable() {
        // Nothing here asserts a value; it exercises the AX path enough times
        // that a missing CFRelease shows up under `leaks`, and it catches a
        // panic on the second and later calls (stale element reuse).
        let probe = MacAppContextProbe::new();
        for _ in 0..8 {
            let _ = probe.capture(CaptureOpts {
                window_title: true,
                url: true,
            });
        }
    }
}
