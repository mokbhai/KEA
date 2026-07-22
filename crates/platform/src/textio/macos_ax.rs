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

fn test_ax_slot() -> &'static Mutex<Option<AxInsertFn>> {
    TEST_AX_INSERT.get_or_init(|| Mutex::new(None))
}

/// Test seam: inject a fake AX inserter (or `None` to use the real AX APIs).
#[cfg(test)]
pub fn set_ax_insert_fn_for_test(insert: Option<AxInsertFn>) {
    *test_ax_slot().lock().unwrap() = insert;
}

/// Whether this process is trusted for Accessibility APIs.
pub fn is_ax_trusted() -> bool {
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
