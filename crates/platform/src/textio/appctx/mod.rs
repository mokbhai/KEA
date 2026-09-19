//! What app the text is about to land in, for per-app profiles.
//!
//! # What is captured, and what is never persisted
//!
//! An [`AppContext`] holds at most four facts about the frontmost app:
//!
//! | field | source | permission | default |
//! |---|---|---|---|
//! | `bundle_id` | `NSRunningApplication.bundleIdentifier` | none | always captured |
//! | `app_name` | `NSRunningApplication.localizedName` | none | always captured |
//! | `window_title` | AX `AXFocusedWindow` → `AXTitle` | Accessibility | **off** |
//! | `url` | AX `AXDocument` on the focused window/element | Accessibility | **off** |
//!
//! `bundle_id` and `app_name` are cheap, permissionless and dull, so they are
//! always read. The other two are opt-in per [`CaptureOpts`] because they are
//! *contents*, not identity: a window title is "Q3 layoffs — final.docx" and a
//! URL is a page the user is reading.
//!
//! **The persistence rule.** An `AppContext` lives in memory for the duration
//! of one run. `url` and `window_title` are never written to any pool and
//! never logged at `info`. [`AppContext`]'s `Debug` impl redacts the URL down
//! to its host precisely so that a `tracing::debug!("{ctx:?}")` added later
//! cannot leak the path or the query string. If app context is ever stored in
//! History that is a separate opt-in storing `bundle_id` and `app_name` only.
//!
//! Only `bundle_id` and `url` are ever matched against a profile — see
//! `kea_core::app_context::ProfileQuery`. `app_name` and `window_title` are
//! localized, volatile display strings and are not match keys.

use std::fmt;

use serde::{Deserialize, Serialize};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod stub;

/// The frontmost app at the moment a run started.
///
/// Every field is optional and an absent one is normal, not an error: with
/// Accessibility revoked the last two are always `None` and bundle-id rules
/// must keep working regardless.
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppContext {
    /// e.g. `com.tinyspeck.slackmacgap`. The only stable match key.
    pub bundle_id: Option<String>,
    /// e.g. `Slack`. Display only — localized, so never a match key.
    pub app_name: Option<String>,
    /// Focused window title. Opt-in; diagnostics and UI hint only.
    pub window_title: Option<String>,
    /// Focused document/page URL. Opt-in, browsers only, never persisted.
    pub url: Option<String>,
}

impl AppContext {
    /// The host of [`Self::url`], which is the only part of it that may be
    /// logged. `https://mail.example.com/u/0/#inbox/msg` → `mail.example.com`.
    pub fn url_host(&self) -> Option<&str> {
        let url = self.url.as_deref()?;
        let rest = match url.split_once("://") {
            Some((_scheme, rest)) => rest,
            None => url,
        };
        let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
        let host = match authority.rsplit_once('@') {
            Some((_userinfo, host)) => host,
            None => authority,
        };
        (!host.is_empty()).then_some(host)
    }

    /// Whether anything at all was learned. An all-`None` context means the
    /// probe found no GUI session; callers treat it as "no profile applies".
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Hand-written so that the URL is redacted to its host wherever a context is
/// formatted. The derived impl would print the full URL, and the first
/// `debug!("{ctx:?}")` someone adds on the hotkey path would put a private
/// page into the log file the user later attaches to a bug report.
impl fmt::Debug for AppContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AppContext")
            .field("bundle_id", &self.bundle_id)
            .field("app_name", &self.app_name)
            .field("window_title", &self.window_title)
            .field("url_host", &self.url_host())
            .finish()
    }
}

/// Which of the two opt-in fields to read. [`Default`] is both off.
///
/// Backed by two settings keys rather than a migration — one row per key, and
/// an absent row reads as the default. The app layer reads them and builds
/// this; the probe itself has no database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureOpts {
    pub window_title: bool,
    pub url: bool,
}

impl CaptureOpts {
    /// Settings key for [`Self::window_title`]. Default **false**: a window
    /// title is document contents, it is not a match key, and it is not worth
    /// capturing by default just to show a nicer UI hint.
    pub const SETTING_WINDOW_TITLE: &'static str = "profiles.capture_window_title";
    /// Settings key for [`Self::url`]. Default **false**; see the module docs.
    pub const SETTING_URL: &'static str = "profiles.capture_url";

    /// Whether any AX round trip is needed at all. When neither is on, the
    /// probe never touches Accessibility and costs two property reads.
    pub fn needs_accessibility(self) -> bool {
        self.window_title || self.url
    }
}

/// Reads the frontmost app. Implementations must be cheap and must never
/// block the caller for longer than [`CaptureOpts`] implies — this runs on the
/// hotkey path, before the HUD appears.
pub trait AppContextProbe: Send + Sync {
    fn capture(&self, opts: CaptureOpts) -> AppContext;
}

/// Construct the active platform [`AppContextProbe`] for this OS.
pub fn new_app_context_probe() -> Box<dyn AppContextProbe> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacAppContextProbe::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubAppContextProbe::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx_with_url(url: &str) -> AppContext {
        AppContext {
            url: Some(url.into()),
            ..Default::default()
        }
    }

    #[test]
    fn url_host_keeps_only_the_host() {
        assert_eq!(
            ctx_with_url("https://mail.example.com/u/0/#inbox/msg?q=secret").url_host(),
            Some("mail.example.com")
        );
        assert_eq!(
            ctx_with_url("https://user:pw@host.test/x").url_host(),
            Some("host.test")
        );
        assert_eq!(
            ctx_with_url("file:///Users/me/Q3%20layoffs.docx").url_host(),
            None
        );
        assert_eq!(AppContext::default().url_host(), None);
    }

    #[test]
    fn debug_redacts_the_url_to_its_host() {
        // The guard against a future `debug!("{ctx:?}")` leaking a page the
        // user was reading into a log file they later attach to a bug report.
        let ctx = ctx_with_url("https://bank.example/accounts/12345?token=abcdef");
        let rendered = format!("{ctx:?}");
        assert!(rendered.contains("bank.example"), "{rendered}");
        assert!(!rendered.contains("12345"), "{rendered}");
        assert!(!rendered.contains("abcdef"), "{rendered}");
    }

    #[test]
    fn default_capture_opts_touch_no_accessibility() {
        let opts = CaptureOpts::default();
        assert!(!opts.window_title);
        assert!(!opts.url);
        assert!(!opts.needs_accessibility());
    }

    #[test]
    fn empty_context_is_detectable() {
        assert!(AppContext::default().is_empty());
        assert!(!ctx_with_url("https://x.test/").is_empty());
    }

    /// The probe runs for real here — there is no seam and no fake, because
    /// the thing worth asserting is the privacy default, not the AX values
    /// (which depend on whatever is frontmost on the machine running CI).
    #[test]
    fn default_capture_never_returns_a_url_or_window_title() {
        let probe = new_app_context_probe();
        let ctx = probe.capture(CaptureOpts::default());
        assert_eq!(ctx.url, None);
        assert_eq!(ctx.window_title, None);
    }
}
