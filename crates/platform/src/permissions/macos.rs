//! macOS permission checks via Core Graphics (Screen Recording), AVFoundation
//! (Microphone) and the Accessibility APIs (`AXIsProcessTrusted`).
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
//!
//! # Manual verification — Calendar
//! 1. Call `request(Calendar)`; macOS shows a permission dialog (requires
//!    `NSCalendarsFullAccessUsageDescription` on macOS 14+ and
//!    `NSCalendarsUsageDescription` on older systems in `Info.plist`, without
//!    which the process is killed, exactly as for the microphone below).
//! 2. Grant or deny access in the system prompt.
//! 3. `status(Calendar)` reflects the EKAuthorizationStatus:
//!    `NotDetermined` → [`PermStatus::Unknown`],
//!    `FullAccess` → [`PermStatus::Granted`],
//!    everything else — including macOS 14's `WriteOnly` — → [`PermStatus::Denied`],
//!    because reading events needs full access and nothing less will do.
//!
//! # Manual verification — Accessibility
//! 1. Call `request(Accessibility)`; macOS shows the trust dialog, which offers
//!    to open System Settings when the process is not yet trusted.
//! 2. Grant access in **System Settings → Privacy & Security → Accessibility**.
//! 3. `status(Accessibility)` should return [`PermStatus::Granted`]; the grant
//!    only takes effect for a process that is relaunched afterwards.

use super::{PermError, PermKind, PermStatus, Permissions};
use crate::textio::macos_ax;
use async_trait::async_trait;
use core_graphics::access::ScreenCaptureAccess;

use block2::RcBlock;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send, sel};
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

/// `EKEntityTypeEvent`. Reminders are entity type 1 and are never requested.
pub(crate) const EK_ENTITY_TYPE_EVENT: i64 = 0;

/// `EKAuthorizationStatus`, as of macOS 14.
///
/// Its own constants, and its own mapper below, rather than reusing
/// [`av_auth_status_to_perm`]. The two enums coincide on 0–3 today *by
/// accident*, and macOS 14 added a fifth EventKit value — `writeOnly` — that
/// AVFoundation has no counterpart for. A shared mapper would classify
/// write-only through a coincidence of integers, and write-only is exactly the
/// case that must not read as granted.
const EK_AUTH_NOT_DETERMINED: i64 = 0;
const EK_AUTH_RESTRICTED: i64 = 1;
const EK_AUTH_DENIED: i64 = 2;
/// `authorized` before macOS 14, `fullAccess` from macOS 14. Same raw value.
pub(crate) const EK_AUTH_FULL_ACCESS: i64 = 3;
/// macOS 14+. Enough to add an event, never enough to read one.
const EK_AUTH_WRITE_ONLY: i64 = 4;

/// Map an `EKAuthorizationStatus` (`NSInteger`) to [`PermStatus`].
///
/// Unit-testable without TCC interaction.
pub(crate) fn ek_auth_status_to_perm(status: i64) -> PermStatus {
    match status {
        EK_AUTH_NOT_DETERMINED => PermStatus::Unknown,
        EK_AUTH_FULL_ACCESS => PermStatus::Granted,
        // Write-only can create events and cannot read them, which is the only
        // thing KEA wants, so it is a denial for this purpose.
        EK_AUTH_WRITE_ONLY | EK_AUTH_DENIED | EK_AUTH_RESTRICTED => PermStatus::Denied,
        _ => PermStatus::Denied, // future EventKit values: fail closed
    }
}

/// The raw `EKAuthorizationStatus` for events.
///
/// `pub(crate)` so `calendar::macos` can refuse to read before it has a grant
/// without sending this message from a second place.
pub(crate) fn event_auth_status() -> i64 {
    unsafe {
        let cls = class!(EKEventStore);
        msg_send![cls, authorizationStatusForEntityType: EK_ENTITY_TYPE_EVENT]
    }
}

pub struct MacPermissions;

impl MacPermissions {
    pub fn new() -> Self {
        Self
    }

    fn screen_recording_status() -> PermStatus {
        let access = ScreenCaptureAccess;
        if access.preflight() {
            PermStatus::Granted
        } else {
            PermStatus::Denied
        }
    }

    fn microphone_status() -> PermStatus {
        av_auth_status_to_perm(microphone_auth_status())
    }

    /// Accessibility has no "not determined" state to report: a process is
    /// trusted or it is not.
    fn accessibility_status() -> PermStatus {
        if macos_ax::is_ax_trusted() {
            PermStatus::Granted
        } else {
            PermStatus::Denied
        }
    }

    fn calendar_status() -> PermStatus {
        ek_auth_status_to_perm(event_auth_status())
    }

    /// Ask for *full* calendar access.
    ///
    /// macOS 14 split calendar access in two and added
    /// `requestFullAccessToEventsWithCompletion:`; on older systems only
    /// `requestAccessToEntityType:completion:` exists. The choice is made with
    /// `respondsToSelector:` rather than a version string, because the
    /// selector is the thing that actually has to be there — a deployment
    /// target below 14 compiles either way and a version check would be a
    /// second source of truth about the same fact.
    async fn request_calendar() -> Result<PermStatus, PermError> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        // Same reason as `request_microphone`: ObjC blocks are Fn, not FnOnce.
        let tx = std::sync::Mutex::new(Some(tx));

        {
            let block = RcBlock::new(move |_granted: Bool, _error: *mut AnyObject| {
                if let Some(tx) = tx.lock().ok().and_then(|mut guard| guard.take()) {
                    let _ = tx.send(ek_auth_status_to_perm(event_auth_status()));
                }
            });

            unsafe {
                let store: *mut AnyObject = msg_send![class!(EKEventStore), alloc];
                let store: *mut AnyObject = msg_send![store, init];
                let Some(store) = Retained::from_raw(store) else {
                    return Err(PermError::Other("EKEventStore could not be created".into()));
                };

                let modern = sel!(requestFullAccessToEventsWithCompletion:);
                let responds: Bool = msg_send![&*store, respondsToSelector: modern];
                if responds.is_true() {
                    let () = msg_send![&*store, requestFullAccessToEventsWithCompletion: &*block];
                } else {
                    let () = msg_send![
                        &*store,
                        requestAccessToEntityType: EK_ENTITY_TYPE_EVENT,
                        completion: &*block
                    ];
                }
            }
        }

        rx.await
            .map_err(|_| PermError::Other("calendar request completion handler dropped".into()))
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
            PermKind::Accessibility => Self::accessibility_status(),
            PermKind::Calendar => Self::calendar_status(),
        }
    }

    async fn request(&self, kind: PermKind) -> Result<PermStatus, PermError> {
        match kind {
            PermKind::ScreenRecording => {
                let access = ScreenCaptureAccess;
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
            // Accessibility can only be granted by hand in System Settings; the
            // system prompt is the shortcut that opens it there.
            PermKind::Accessibility => {
                let _ = macos_ax::prompt_ax_trust();
                Ok(Self::accessibility_status())
            }
            PermKind::Calendar => Self::request_calendar().await,
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
        assert_eq!(av_auth_status_to_perm(AV_AUTH_DENIED), PermStatus::Denied);
    }

    #[test]
    fn av_auth_status_maps_restricted_to_denied() {
        assert_eq!(
            av_auth_status_to_perm(AV_AUTH_RESTRICTED),
            PermStatus::Denied
        );
    }

    #[test]
    fn ek_auth_status_maps_not_determined_to_unknown() {
        assert_eq!(
            ek_auth_status_to_perm(EK_AUTH_NOT_DETERMINED),
            PermStatus::Unknown
        );
    }

    #[test]
    fn ek_auth_status_maps_full_access_to_granted() {
        assert_eq!(
            ek_auth_status_to_perm(EK_AUTH_FULL_ACCESS),
            PermStatus::Granted
        );
    }

    #[test]
    fn ek_auth_status_maps_denied_and_restricted_to_denied() {
        assert_eq!(ek_auth_status_to_perm(EK_AUTH_DENIED), PermStatus::Denied);
        assert_eq!(
            ek_auth_status_to_perm(EK_AUTH_RESTRICTED),
            PermStatus::Denied
        );
    }

    /// The value that makes a shared AVFoundation mapper wrong: write-only can
    /// add an event and cannot read one, so it is not a grant for this
    /// feature.
    #[test]
    fn ek_auth_status_maps_write_only_to_denied() {
        assert_eq!(
            ek_auth_status_to_perm(EK_AUTH_WRITE_ONLY),
            PermStatus::Denied
        );
    }

    #[test]
    fn an_unknown_future_ek_value_fails_closed() {
        assert_eq!(ek_auth_status_to_perm(99), PermStatus::Denied);
        assert_eq!(ek_auth_status_to_perm(-1), PermStatus::Denied);
    }

    #[test]
    fn accessibility_status_follows_ax_trust() {
        let _trust = macos_ax::AxTrustOverride::force(true);
        assert_eq!(
            MacPermissions::new().status(PermKind::Accessibility),
            PermStatus::Granted
        );
    }

    #[test]
    fn accessibility_status_denied_when_untrusted() {
        let _trust = macos_ax::AxTrustOverride::force(false);
        assert_eq!(
            MacPermissions::new().status(PermKind::Accessibility),
            PermStatus::Denied
        );
    }
}
