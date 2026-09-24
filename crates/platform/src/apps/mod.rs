//! Bringing another application to the front.
//!
//! Platform rather than features because it is an OS service and nothing else:
//! the whole implementation is a framework call, and the interesting part is
//! what the OS does when the name does not resolve.
//!
//! **Naming is the hard part, not launching.** The user says "mail" and means
//! whatever their Mail application is; they say "chrome" and mean Google
//! Chrome. Resolving that is the platform's job — macOS already does it, with
//! the user's own installed applications and their localised names — so this
//! takes a display name and lets `NSWorkspace` decide, rather than carrying a
//! table of bundle identifiers that goes stale the first time someone installs
//! a different browser.

use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
pub mod stub;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AppLaunchError {
    /// No application of that name is installed, or the name resolved to
    /// nothing. Distinct from [`Self::Failed`] because it is the user's
    /// wording that needs changing, not the machine's state — and after a
    /// speech-to-text pass, a name that does not resolve is more often a
    /// mishearing than a missing application.
    #[error("no application named {name:?} was found")]
    NotFound { name: String },
    /// Found, but the OS refused to start it.
    #[error("could not open {name:?}: {reason}")]
    Failed { name: String, reason: String },
    #[error("this platform cannot open applications")]
    Unsupported,
}

/// Opens applications by display name.
pub trait AppLauncher: Send + Sync {
    /// Bring the named application to the front, launching it if it is not
    /// already running.
    ///
    /// Already-running is not an error and not a distinct outcome: "open Mail"
    /// when Mail is open means put it in front of me, which is what the OS
    /// does.
    fn open(&self, name: &str) -> Result<(), AppLaunchError>;
}

/// Construct the active platform [`AppLauncher`] for this OS.
pub fn new_app_launcher() -> Box<dyn AppLauncher> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacAppLauncher)
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubAppLauncher)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_name_is_not_found_rather_than_launching_something() {
        // A blank name reaching the OS is how "open" becomes "open whatever".
        // Caught here so every platform behaves the same way.
        let err = new_app_launcher().open("   ").unwrap_err();
        assert!(matches!(
            err,
            AppLaunchError::NotFound { .. } | AppLaunchError::Unsupported
        ));
    }

    #[test]
    fn a_name_that_cannot_exist_is_reported_as_not_found() {
        let err = new_app_launcher()
            .open("Kea Nonexistent Application 9f3a")
            .unwrap_err();
        assert!(matches!(
            err,
            AppLaunchError::NotFound { .. } | AppLaunchError::Unsupported
        ));
    }
}
