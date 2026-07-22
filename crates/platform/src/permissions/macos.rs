//! macOS permission checks via Core Graphics (Screen Recording) and AVFoundation (Microphone).
//!
//! # Manual verification — Screen Recording
//! 1. Call `request(ScreenRecording)` once; macOS shows a permission dialog.
//! 2. Grant access in **System Settings → Privacy & Security → Screen Recording**.
//! 3. `status(ScreenRecording)` should return [`PermStatus::Granted`].
//!
//! # Manual verification — Microphone
//! 1. Call `request(Microphone)` once; macOS shows a permission dialog (requires
//!    `NSMicrophoneUsageDescription` in `Info.plist`, without which the process crashes).
//! 2. Grant or deny access in the system prompt.
//! 3. `status(Microphone)` reflects the AVAuthorizationStatus:
//!    `NotDetermined` → [`PermStatus::Unknown`],
//!    `Authorized` → [`PermStatus::Granted`],
//!    `Denied` / `Restricted` → [`PermStatus::Denied`].

use async_trait::async_trait;
use super::{PermError, PermKind, PermStatus, Permissions};
use core_graphics::access::ScreenCaptureAccess;

use block2::RcBlock;
use objc2::runtime::Bool;
use objc2::{class, msg_send};
use objc2_foundation::ns_string;

/// AVAuthorizationStatus enum values (from AVFoundation; not a public re-export).
const AV_AUTH_NOT_DETERMINED: i64 = 0;
const AV_AUTH_RESTRICTED: i64 = 1;
const AV_AUTH_DENIED: i64 = 2;
const AV_AUTH_AUTHORIZED: i64 = 3;

/// Map an AVAuthorizationStatus (`NSInteger`) to [`PermStatus`].
///
/// Unit-testable without TCC interaction.
pub(crate) fn av_auth_status_to_perm(status: i64) -> PermStatus {
    match status {
        AV_AUTH_NOT_DETERMINED => PermStatus::Unknown,
        AV_AUTH_AUTHORIZED => PermStatus::Granted,
        AV_AUTH_DENIED | AV_AUTH_RESTRICTED => PermStatus::Denied,
        _ => PermStatus::Denied, // future AVFoundation values: fail closed
    }
}

/// Return the raw AVAuthorizationStatus for `AVMediaTypeAudio`.
///
/// Exposed as `pub(crate)` for unit tests; not part of the public platform API.
pub(crate) fn microphone_auth_status() -> i64 {
    unsafe {
        let cls = class!(AVCaptureDevice);
        let media_type = ns_string!("soun"); // AVMediaTypeAudio = @"soun"
        msg_send![cls, authorizationStatusForMediaType: media_type]
    }
}

pub struct MacPermissions;

impl MacPermissions {
    pub fn new() -> Self {
        Self
    }

    fn screen_recording_status() -> PermStatus {
        let access = ScreenCaptureAccess::default();
        if access.preflight() {
            PermStatus::Granted
        } else {
            PermStatus::Denied
        }
    }

    fn microphone_status() -> PermStatus {
        av_auth_status_to_perm(microphone_auth_status())
    }

    async fn request_microphone() -> Result<PermStatus, PermError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Blocks are Fn, not FnOnce, so the oneshot sender is taken out of a
        // Mutex<Option<_>> on the (single) completion invocation.
        let tx = std::sync::Mutex::new(Some(tx));

        // Scoped so the (non-Send) block is dropped before the await; the
        // ObjC runtime copies completion handlers it stores.
        {
            let block = RcBlock::new(move |_granted: Bool| {
                if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
                    let _ = tx.send(av_auth_status_to_perm(microphone_auth_status()));
                }
            });

            unsafe {
                let cls = class!(AVCaptureDevice);
                let media_type = ns_string!("soun");
                let () = msg_send![cls, requestAccessForMediaType: media_type, completionHandler: &*block];
            }
        }

        rx.await
            .map_err(|_| PermError::Other("microphone request completion handler dropped".into()))
    }
}

#[async_trait]
impl Permissions for MacPermissions {
    fn status(&self, kind: PermKind) -> PermStatus {
        match kind {
            PermKind::ScreenRecording => Self::screen_recording_status(),
            PermKind::Microphone => Self::microphone_status(),
        }
    }

    async fn request(&self, kind: PermKind) -> Result<PermStatus, PermError> {
        match kind {
            PermKind::ScreenRecording => {
                let access = ScreenCaptureAccess::default();
                if access.preflight() {
                    return Ok(PermStatus::Granted);
                }
                Ok(if access.request() {
                    PermStatus::Granted
                } else {
                    PermStatus::Denied
                })
            }
            PermKind::Microphone => Self::request_microphone().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn av_auth_status_maps_not_determined_to_unknown() {
        assert_eq!(
            av_auth_status_to_perm(AV_AUTH_NOT_DETERMINED),
            PermStatus::Unknown
        );
    }

    #[test]
    fn av_auth_status_maps_authorized_to_granted() {
        assert_eq!(
            av_auth_status_to_perm(AV_AUTH_AUTHORIZED),
            PermStatus::Granted
        );
    }

    #[test]
    fn av_auth_status_maps_denied_to_denied() {
        assert_eq!(
            av_auth_status_to_perm(AV_AUTH_DENIED),
            PermStatus::Denied
        );
    }

    #[test]
    fn av_auth_status_maps_restricted_to_denied() {
        assert_eq!(
            av_auth_status_to_perm(AV_AUTH_RESTRICTED),
            PermStatus::Denied
        );
    }
}
