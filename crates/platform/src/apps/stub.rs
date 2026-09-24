//! No-op launcher for platforms without a runtime implementation.

use super::{AppLaunchError, AppLauncher};

pub struct StubAppLauncher;

impl AppLauncher for StubAppLauncher {
    /// Always [`AppLaunchError::Unsupported`] — never a panic and never a
    /// silent success, so a Windows or Linux build compiles, runs, and tells
    /// the user the truth about what it cannot do.
    fn open(&self, _name: &str) -> Result<(), AppLaunchError> {
        Err(AppLaunchError::Unsupported)
    }
}
