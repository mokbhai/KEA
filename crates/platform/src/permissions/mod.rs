//! OS permission status and request helpers (Screen Recording, Microphone).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod stub;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermKind {
    Microphone,
    ScreenRecording,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermStatus {
    Unknown,
    Granted,
    Denied,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PermError {
    #[error("{0}")]
    Other(String),
}

/// Platform permission probe and request surface.
///
/// Screen Recording grant on macOS requires manual acceptance in System Settings
/// when the request dialog is dismissed or denied.
#[async_trait]
pub trait Permissions: Send + Sync {
    fn status(&self, kind: PermKind) -> PermStatus;
    async fn request(&self, kind: PermKind) -> Result<PermStatus, PermError>;
}

/// Construct the active platform [`Permissions`] implementation for this OS.
pub fn new_permissions() -> Box<dyn Permissions> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(macos::MacPermissions::new());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubPermissions::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_status_serializes() {
        let json = serde_json::to_string(&PermStatus::Granted).unwrap();
        assert_eq!(json, r#""Granted""#);
    }

    #[test]
    fn perm_kind_serializes() {
        let json = serde_json::to_string(&PermKind::ScreenRecording).unwrap();
        assert_eq!(json, r#""ScreenRecording""#);
    }
}
