//! Non-macOS stub until Windows/Linux platform tasks land.
//!
//! All feature-use methods return errors indicating the platform is not yet
//! supported.  Startup paths (construction, state queries) are safe and never
//! panic.  [`StubHotkeys::on_action`] returns an immediately-closed channel so
//! the listener loop in `main.rs` exits cleanly without parking.

use super::{ActionId, HotkeyBinding, HotkeyError, Hotkeys};
use async_trait::async_trait;
use tokio::sync::mpsc;

pub struct StubHotkeys;

impl StubHotkeys {
    pub fn new() -> Self {
        Self
    }
}

#[async_trait]
impl Hotkeys for StubHotkeys {
    fn register(&mut self, _: HotkeyBinding, _: ActionId) -> Result<(), HotkeyError> {
        Err(HotkeyError::Other(
            "global hotkeys are not yet implemented on this platform".into(),
        ))
    }

    fn unregister(&mut self, binding: &HotkeyBinding) -> Result<(), HotkeyError> {
        Err(HotkeyError::NotRegistered(binding.accelerator.clone()))
    }

    /// Returns an immediately-closed channel.  The listener loop in `main.rs`
    /// sees `None` on the first `recv().await` and exits cleanly without
    /// blocking the runtime.
    fn on_action(&self) -> mpsc::Receiver<ActionId> {
        let (_, rx) = mpsc::channel(1);
        rx
    }
}
