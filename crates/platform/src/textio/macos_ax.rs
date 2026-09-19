//! macOS Accessibility (AX) text insertion (D12).
//!
//! Best-effort replacement via the focused UI element's `AXSelectedText` attribute.
//! Requires **Accessibility** permission (`AXIsProcessTrusted`) — grant in
//! **System Settings → Privacy & Security → Accessibility**.
//!
//! # Manual verification
//! 1. Grant Accessibility to KEA.
//! 2. Select text in TextEdit (or another AX-aware app).
//! 3. Call `TextIo::replace_with_mode(..., ReplaceMode::Accessibility)`.
//! 4. Selection should update without clipboard round-trip when AX succeeds.
//! 5. On failure, `MacTextIo` falls back to clipboard+paste and logs a warning.

use std::ffi::c_void;
use std::ptr;
use std::sync::{Mutex, OnceLock};

use core_foundation::base::TCFType;

type AxInsertFn = Box<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

static TEST_AX_INSERT: OnceLock<Mutex<Option<AxInsertFn>>> = OnceLock::new();
static TEST_AX_TRUSTED: OnceLock<Mutex<Option<bool>>> = OnceLock::new();

fn test_ax_slot() -> &'static Mutex<Option<AxInsertFn>> {
    TEST_AX_INSERT.get_or_init(|| Mutex::new(None))
}

fn test_trust_slot() -> &'static Mutex<Option<bool>> {
    TEST_AX_TRUSTED.get_or_init(|| Mutex::new(None))
}

/// Test seam: inject a fake AX inserter (or `None` to use the real AX APIs).
#[cfg(test)]
pub fn set_ax_insert_fn_for_test(insert: Option<AxInsertFn>) {
    *test_ax_slot().lock().unwrap() = insert;
}

#[cfg(test)]
static AX_TRUST_TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Test seam: forces the answer of [`is_ax_trusted`] while it is alive.
///
/// Trust is a property of the *process*, so without a seam no test can reach
/// the untrusted branch — and the untrusted branch is the one that used to
/// swallow dictated text. The override is process-global and `cargo test` runs
/// tests in parallel in one process, so the guard also serialises its users:
/// otherwise a test forcing "untrusted" could flip the answer underneath a
/// neighbour mid-assertion.
#[cfg(test)]
pub struct AxTrustOverride {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl AxTrustOverride {
    pub fn force(trusted: bool) -> Self {
        // A poisoned lock here means some other test panicked; that is no
        // reason to fail this one on top of it.
        let serial = AX_TRUST_TEST_SERIAL
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *test_trust_slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(trusted);
        Self { _serial: serial }
    }
}

#[cfg(test)]
impl Drop for AxTrustOverride {
    fn drop(&mut self) {
        *test_trust_slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
}

/// Whether this process is trusted for Accessibility APIs.
///
/// This gates far more than the AX insertion path: macOS also refuses to
/// deliver `CGEventPost`ed keystrokes from an untrusted process, so it decides
/// whether the clipboard+Cmd+V path in [`super::macos`] can work at all.
pub fn is_ax_trusted() -> bool {
    if let Some(forced) = *test_trust_slot().lock().unwrap_or_else(|p| p.into_inner()) {
        return forced;
    }
    unsafe { AXIsProcessTrusted() }
}

/// Prompt for Accessibility trust, showing the macOS system dialog (which offers
/// to open System Settings) when the process is not yet trusted. Returns whether
/// the process is currently trusted.
pub fn prompt_ax_trust() -> bool {
    use core_foundation::boolean::CFBoolean;
    use core_foundation::dictionary::CFDictionary;
    use core_foundation::string::CFString;

    unsafe {
        let key = CFString::wrap_under_get_rule(kAXTrustedCheckOptionPrompt);
        let value = CFBoolean::true_value();
        let options = CFDictionary::from_CFType_pairs(&[(key, value)]);
        AXIsProcessTrustedWithOptions(options.as_concrete_TypeRef() as *const c_void)
    }
}

/// A one-line description of where a synthetic paste is about to land.
///
/// This is diagnostics, not control flow: the most common "paste did nothing"
/// report is a paste that went exactly where it was told, into an app or an
/// element that was not the one the user was looking at — the transcript is in
/// the search box, or the frontmost app changed while the LLM was thinking, or
/// nothing at all has keyboard focus because the user clicked the desktop.
/// None of that is visible from a `CGEventPost` that returns `void`, so it is
/// recorded before every copy and paste instead.
///
/// Returns something like `TextEdit/AXTextArea`. The two halves fail
/// independently and on purpose — `Slack/none` (the app is frontmost but
/// nothing in it has keyboard focus) is a different bug report from
/// `KEA/AXTextField` (we pasted into ourselves), and both are invisible if the
/// whole thing collapses to one "unknown".
pub fn focus_summary() -> String {
    format!("{}/{}", frontmost_app_name(), focused_element_role())
}

/// The app that will receive a synthetic keystroke, from `NSWorkspace`.
///
/// Deliberately *not* read from AX: `AXFocusedApplication` needs the
/// Accessibility grant and returns nothing at all in several ordinary
/// situations, which would blank out the diagnostic exactly when the grant is
/// the thing being diagnosed. `NSWorkspace.frontmostApplication` needs no
/// permission and answers whenever there is a GUI session.
pub fn frontmost_app_name() -> String {
    // SAFETY: `sharedWorkspace` and `frontmostApplication` return shared,
    // autoreleased objects that outlive the call, and `localizedName` is a
    // plain property read on the result. The class lookups return `None`
    // rather than a dangling pointer when AppKit is not loaded.
    unsafe {
        let Some(class) = objc2::runtime::AnyClass::get(c"NSWorkspace") else {
            return "<no AppKit>".into();
        };
        let workspace: *mut objc2::runtime::AnyObject =
            objc2::msg_send![class, sharedWorkspace];
        if workspace.is_null() {
            return "<no workspace>".into();
        }
        let app: *mut objc2::runtime::AnyObject =
            objc2::msg_send![workspace, frontmostApplication];
        if app.is_null() {
            return "<no frontmost app>".into();
        }
        let name: *mut objc2_foundation::NSString = objc2::msg_send![app, localizedName];
        if name.is_null() {
            return "<unnamed app>".into();
        }
        (*name).to_string()
    }
}

/// The AX role of whatever has keyboard focus, e.g. `AXTextArea`.
///
/// `none` is a real and common answer: it means the keystroke has nowhere to
/// go, which is what "I pressed the key and nothing happened" usually is.
fn focused_element_role() -> String {
    if !is_ax_trusted() {
        return "unknown (not trusted for Accessibility)".to_string();
    }
    // SAFETY: every raw element below is owned (the AX "Copy" functions follow
    // the Core Foundation create rule) and released exactly once by `AxRef`.
    unsafe {
        let Some(system) = AxRef::new(AXUIElementCreateSystemWide()) else {
            return "unknown (no system-wide AX element)".to_string();
        };
        let Some(focused) = system.copy_attr("AXFocusedUIElement") else {
            return "none".to_string();
        };
        focused
            .copy_string_attr("AXRole")
            .unwrap_or_else(|| "<unknown role>".into())
    }
}

/// An owned `AXUIElementRef` (or any CF value an AX copy handed back).
///
/// The AX copy functions follow Core Foundation's create rule, so each one
/// returns a +1 reference the caller must release. `focus_summary` runs on
/// every dictation and every rewrite, so leaking two elements a run is a real
/// leak in a process that stays open all day, not a rounding error.
struct AxRef(*mut c_void);

impl AxRef {
    /// Takes ownership of a +1 reference, or `None` if it is null.
    unsafe fn new(raw: *mut c_void) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// Reads one AX attribute as an owned value, or `None` on any AX error.
    unsafe fn copy_attr(&self, attribute: &str) -> Option<Self> {
        let attr = core_foundation::string::CFString::new(attribute);
        let mut out: *const c_void = ptr::null();
        let err = AXUIElementCopyAttributeValue(self.0, attr.as_concrete_TypeRef(), &mut out);
        if err != K_AX_ERROR_SUCCESS {
            return None;
        }
        Self::new(out as *mut c_void)
    }

    /// Reads one AX attribute as a `String`, or `None` when it is absent or is
    /// not a `CFString` (AXTitle is occasionally a number or an AXValue).
    unsafe fn copy_string_attr(&self, attribute: &str) -> Option<String> {
        let value = self.copy_attr(attribute)?;
        let cf_type = value.0 as core_foundation_sys::base::CFTypeRef;
        if core_foundation::base::CFGetTypeID(cf_type) != core_foundation::string::CFString::type_id()
        {
            return None;
        }
        // Get rule: `value` still owns the reference and releases it on drop.
        let s = core_foundation::string::CFString::wrap_under_get_rule(
            value.0 as core_foundation_sys::string::CFStringRef,
        );
        Some(s.to_string())
    }
}

impl Drop for AxRef {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a non-null, owned CF reference (see `new`), and
        // this is the only place it is released.
        unsafe { core_foundation_sys::base::CFRelease(self.0 as *const _) };
    }
}

/// Insert `text` into the focused element via AX (`AXSelectedText` on focused UI element).
pub fn insert_via_accessibility(text: &str) -> Result<(), String> {
    if let Some(insert) = test_ax_slot().lock().unwrap().as_ref() {
        return insert(text);
    }
    insert_via_accessibility_impl(text)
}

fn insert_via_accessibility_impl(text: &str) -> Result<(), String> {
    if !is_ax_trusted() {
        return Err("accessibility permission not granted".into());
    }

    unsafe {
        let system = AXUIElementCreateSystemWide();
        if system.is_null() {
            return Err("AXUIElementCreateSystemWide failed".into());
        }

        let focused_attr = core_foundation::string::CFString::new("AXFocusedUIElement");
        let mut focused: *const c_void = ptr::null();
        let err = AXUIElementCopyAttributeValue(
            system,
            focused_attr.as_concrete_TypeRef(),
            &mut focused,
        );
        if err != K_AX_ERROR_SUCCESS || focused.is_null() {
            return Err(format!("no focused UI element (AX error {err})"));
        }

        let text_attr = core_foundation::string::CFString::new("AXSelectedText");
        let cf_text = core_foundation::string::CFString::new(text);
        let err = AXUIElementSetAttributeValue(
            focused as *mut c_void,
            text_attr.as_concrete_TypeRef(),
            cf_text.as_concrete_TypeRef() as *const _,
        );
        if err != K_AX_ERROR_SUCCESS {
            return Err(format!("AXUIElementSetAttributeValue failed ({err})"));
        }
        Ok(())
    }
}

const K_AX_ERROR_SUCCESS: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> *mut c_void;
    fn AXUIElementCopyAttributeValue(
        element: *mut c_void,
        attribute: core_foundation_sys::string::CFStringRef,
        value: *mut *const c_void,
    ) -> i32;
    fn AXUIElementSetAttributeValue(
        element: *mut c_void,
        attribute: core_foundation_sys::string::CFStringRef,
        value: core_foundation_sys::base::CFTypeRef,
    ) -> i32;
    fn AXIsProcessTrusted() -> bool;
    static kAXTrustedCheckOptionPrompt: core_foundation_sys::string::CFStringRef;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn injectable_ax_insert_fn_is_used_when_set() {
        set_ax_insert_fn_for_test(Some(Box::new(|text| {
            if text == "ok" {
                Ok(())
            } else {
                Err("boom".into())
            }
        })));
        assert!(insert_via_accessibility("ok").is_ok());
        assert!(insert_via_accessibility("nope").is_err());
        set_ax_insert_fn_for_test(None);
    }
}
