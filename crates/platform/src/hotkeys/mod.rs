//! Global hotkey registration and accelerator parsing.

use async_trait::async_trait;
use global_hotkey::hotkey::HotKey;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::sync::mpsc;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
pub mod stub;

pub type ActionId = String;

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
