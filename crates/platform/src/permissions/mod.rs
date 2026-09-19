//! OS permission status and request helpers (Screen Recording, Microphone,
//! Accessibility, Calendar).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// `pub(crate)` so `calendar::macos` can ask this module for the EventKit
// authorization status instead of sending the same `EKEventStore` message from
// a second place. TCC is this module's concern; reading events is that one's.
#[cfg(target_os = "macos")]
pub(crate) mod macos;
#[cfg(not(target_os = "macos"))]
mod stub;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum PermKind {
    Microphone,
    ScreenRecording,
    Accessibility,
    /// Read access to the user's calendar events, for naming a meeting after
    /// the event it happened during. Read-only, and off unless the user turns
    /// the feature on.
    Calendar,
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
/// Screen Recording and Accessibility grants on macOS require manual acceptance
/// in System Settings when the request dialog is dismissed or denied.
#[async_trait]
pub trait Permissions: Send + Sync {
    fn status(&self, kind: PermKind) -> PermStatus;
    async fn request(&self, kind: PermKind) -> Result<PermStatus, PermError>;
}

/// Construct the active platform [`Permissions`] implementation for this OS.
pub fn new_permissions() -> Box<dyn Permissions> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacPermissions::new())
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
        let json = serde_json::to_string(&PermKind::Calendar).unwrap();
        assert_eq!(json, r#""Calendar""#);
    }
}
