//! Global hotkey registration and accelerator parsing.

use async_trait::async_trait;
use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

pub mod hold;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "macos")]
pub mod macos_hold;
#[cfg(not(target_os = "macos"))]
pub mod stub;

pub type ActionId = String;

/// A handle on the running hold machine, for the decisions that do not come
/// from the keyboard tap.
///
/// Only one so far: a locked recording is cancelled with Escape, which is an
/// ordinary accelerator rather than another thing for the event tap to watch
/// (see [`hold`]). The machine still has to be told, or it would keep a lock
/// that no longer has a recording behind it.
///
/// Deliberately does not carry the deadline thread's wake channel: a cancelled
/// lock only removes work from that thread, and it re-reads the machine on its
/// next tick either way.
#[derive(Clone)]
pub struct HoldControl {
    machine: std::sync::Arc<std::sync::Mutex<hold::HoldToTalk>>,
}

impl HoldControl {
    pub(crate) fn new(machine: std::sync::Arc<std::sync::Mutex<hold::HoldToTalk>>) -> Self {
        Self { machine }
    }

    fn locked(&self) -> std::sync::MutexGuard<'_, hold::HoldToTalk> {
        self.machine.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn is_locked(&self) -> bool {
        self.locked().is_locked()
    }

    /// End a locked recording without transcribing it.
    pub fn cancel_lock(&self) -> hold::HoldAction {
        self.locked().cancel_lock()
    }

    /// Abandon whatever gesture is in progress, reporting the recording it had
    /// to give up. Used when a run ended by some other route, so the next
    /// chord is judged fresh.
    pub fn reset(&self) -> hold::HoldAction {
        self.locked().reset()
    }
}

/// Starts the press-and-hold ⌥⇧ listener, if this OS has one.
///
/// Separate from [`Hotkeys`] on purpose: that trait is about accelerators that
/// the OS registers and reports as presses, and a modifier-only press-and-hold
/// chord is neither — see [`hold`] for why.
///
/// `enabled` is polled by the listener rather than passed by value so the
/// settings toggle can silence it without a restart.
pub fn spawn_hold_to_talk(
    enabled: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<
    (
        HoldControl,
        tokio::sync::mpsc::UnboundedReceiver<hold::HoldAction>,
    ),
    HotkeyError,
> {
    #[cfg(target_os = "macos")]
    {
        macos_hold::spawn(enabled)
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = enabled;
        Err(HotkeyError::Other(
            "hold-to-talk is not yet implemented on this platform".into(),
        ))
    }
}

/// User-facing accelerator string (e.g. `"CommandOrControl+Shift+R"`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyBinding {
    pub accelerator: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum HotkeyError {
    #[error("invalid accelerator: {0}")]
    InvalidAccelerator(String),
    #[error("hotkey not registered: {0}")]
    NotRegistered(String),
    #[error("{0}")]
    Other(String),
}

/// OS-global hotkey provider. Implementations forward pressed bindings as [`ActionId`] values.
#[async_trait]
pub trait Hotkeys: Send + Sync {
    fn register(&mut self, binding: HotkeyBinding, action: ActionId) -> Result<(), HotkeyError>;
    fn unregister(&mut self, binding: &HotkeyBinding) -> Result<(), HotkeyError>;
    fn on_action(&self) -> mpsc::Receiver<ActionId>;
}

/// Parse a human accelerator string into a [`HotKey`] for `global-hotkey`.
///
/// Accepts common aliases such as `Cmd`, `Ctrl`, `CommandOrControl`, and single-letter keys (`R`).
pub fn parse_accelerator(accelerator: &str) -> Result<HotKey, HotkeyError> {
    let normalized = normalize_accelerator(accelerator);
    normalized
        .parse::<HotKey>()
        .map_err(|e| HotkeyError::InvalidAccelerator(e.to_string()))
}

fn normalize_accelerator(accelerator: &str) -> String {
    accelerator
        .split('+')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(|part| match part.to_ascii_lowercase().as_str() {
            "cmd" | "command" | "meta" | "super" | "win" | "windows" => "Cmd".to_string(),
            "ctrl" | "control" => "Control".to_string(),
            "alt" | "option" => "Alt".to_string(),
            "shift" => "Shift".to_string(),
            "commandorcontrol" | "commandorctrl" | "cmdorctrl" | "cmdorcontrol" => {
                "CommandOrControl".to_string()
            }
            key if key.len() == 1 && key.chars().all(|c| c.is_ascii_alphanumeric()) => {
                key.to_ascii_uppercase()
            }
            other => other.to_string(),
        })
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::Modifiers;

    #[test]
    fn binding_type_roundtrips_json() {
        let binding = HotkeyBinding {
            accelerator: "CommandOrControl+Shift+R".into(),
        };
        let json = serde_json::to_string(&binding).unwrap();
        let back: HotkeyBinding = serde_json::from_str(&json).unwrap();
        assert_eq!(binding, back);
    }

    #[test]
    fn parses_cmd_shift_r() {
        let hotkey = parse_accelerator("Cmd+Shift+R").unwrap();
        assert!(hotkey.mods.contains(Modifiers::SHIFT));
        #[cfg(target_os = "macos")]
        assert!(hotkey.mods.contains(Modifiers::SUPER));
        // Unlike CommandOrControl, Cmd is not platform-adaptive: it always
        // means the Super/Meta key, never Control.
        assert!(!hotkey.mods.contains(Modifiers::CONTROL));
        assert_eq!(
            hotkey.id(),
            parse_accelerator("Super+Shift+R").unwrap().id()
        );
    }

    #[test]
    fn parses_command_or_control_shift_r() {
        let hotkey = parse_accelerator("CommandOrControl+Shift+R").unwrap();
        assert!(hotkey.mods.contains(Modifiers::SHIFT));
        #[cfg(target_os = "macos")]
        assert!(hotkey.mods.contains(Modifiers::SUPER));
        #[cfg(not(target_os = "macos"))]
        assert!(hotkey.mods.contains(Modifiers::CONTROL));
    }

    #[test]
    fn parses_ctrl_alt_delete() {
        let hotkey = parse_accelerator("Ctrl+Alt+Delete").unwrap();
        assert!(hotkey.mods.contains(Modifiers::CONTROL));
        assert!(hotkey.mods.contains(Modifiers::ALT));
    }

    #[test]
    fn rejects_multiple_main_keys() {
        let err = parse_accelerator("Shift+R+A").unwrap_err();
        assert!(matches!(err, HotkeyError::InvalidAccelerator(_)));
    }

    #[test]
    fn normalizes_whitespace() {
        let hotkey = parse_accelerator(" cmd + shift + r ").unwrap();
        assert!(hotkey.mods.contains(Modifiers::SHIFT));
    }
}
