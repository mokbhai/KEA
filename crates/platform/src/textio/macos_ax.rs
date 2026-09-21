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

#[cfg(test)]
static AX_INSERT_TEST_SERIAL: Mutex<()> = Mutex::new(());

/// Test seam: injects a fake AX inserter while it is alive.
///
/// Same reasoning as [`AxTrustOverride`] — the slot is process-global and
/// `cargo test` runs tests in parallel in one process, so the guard serialises
/// its users and puts the real AX APIs back on drop even if a test panics.
///
/// Callers that also force trust take [`AxTrustOverride`] *first*; the two
/// locks are always acquired in that order.
#[cfg(test)]
pub struct AxInsertOverride {
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl AxInsertOverride {
    pub fn force(insert: AxInsertFn) -> Self {
        // A poisoned lock here means some other test panicked; that is no
        // reason to fail this one on top of it.
        let serial = AX_INSERT_TEST_SERIAL
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        *test_ax_slot().lock().unwrap_or_else(|p| p.into_inner()) = Some(insert);
        Self { _serial: serial }
    }
}

#[cfg(test)]
impl Drop for AxInsertOverride {
    fn drop(&mut self) {
        *test_ax_slot().lock().unwrap_or_else(|p| p.into_inner()) = None;
    }
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
    unsafe {
        let app = match frontmost_running_app() {
            Ok(app) => app,
            // The `Err` is already the diagnostic string; see
            // `frontmost_running_app` for why each one is distinct.
            Err(why) => return why.into(),
        };
        ns_string(objc2::msg_send![app, localizedName]).unwrap_or_else(|| "<unnamed app>".into())
    }
}

/// The frontmost `NSRunningApplication`, or the reason there is not one.
///
/// The three failures stay distinct because they are three different bug
/// reports: no AppKit at all, a workspace that would not vend itself, and a
/// GUI session with nothing frontmost.
///
/// # Safety
/// The returned pointer is autoreleased and borrowed, not owned — do not
/// release it, and do not hold it across an autorelease pool drain.
unsafe fn frontmost_running_app() -> Result<*mut objc2::runtime::AnyObject, &'static str> {
    // SAFETY: `sharedWorkspace` and `frontmostApplication` return shared,
    // autoreleased objects that outlive the call. The class lookup returns
    // `None` rather than a dangling pointer when AppKit is not loaded.
    let Some(class) = objc2::runtime::AnyClass::get(c"NSWorkspace") else {
        return Err("<no AppKit>");
    };
    let workspace: *mut objc2::runtime::AnyObject = objc2::msg_send![class, sharedWorkspace];
    if workspace.is_null() {
        return Err("<no workspace>");
    }
    let app: *mut objc2::runtime::AnyObject = objc2::msg_send![workspace, frontmostApplication];
    if app.is_null() {
        return Err("<no frontmost app>");
    }
    Ok(app)
}

/// The pid of the frontmost GUI application. This deliberately uses
/// `NSWorkspace`, not AX, so it tells the truth when Accessibility is a
/// broken prerequisite.
pub fn frontmost_pid() -> Option<i32> {
    unsafe {
        frontmost_running_app().ok().and_then(|app| {
            let pid: i32 = objc2::msg_send![app, processIdentifier];
            if pid > 0 {
                Some(pid)
            } else {
                None
            }
        })
    }
}

/// Copies a borrowed `NSString` property into a `String`, or `None` if null.
///
/// # Safety
/// `ptr` must be null or a valid `NSString` the caller does not own.
unsafe fn ns_string(ptr: *mut objc2_foundation::NSString) -> Option<String> {
    if ptr.is_null() {
        None
    } else {
        Some((*ptr).to_string())
    }
}

/// Identity of the app that is about to receive text.
///
/// This is the *matchable* half of a per-app profile lookup, and it is read
/// from `NSRunningApplication` rather than AX for the reason spelled out on
/// [`frontmost_app_name`]: it must keep answering with Accessibility revoked,
/// or a bundle-id rule would silently stop firing exactly when Accessibility
/// is the thing that broke.
pub(crate) struct FrontmostApp {
    /// For `AXUIElementCreateApplication`. From `processIdentifier`, not AX.
    pub(crate) pid: i32,
    /// `NSRunningApplication.bundleIdentifier`, e.g. `com.tinyspeck.slackmacgap`.
    /// `None` for the handful of processes that have none (some helper tools).
    pub(crate) bundle_id: Option<String>,
    /// `NSRunningApplication.localizedName` — display only, never a match key.
    pub(crate) name: Option<String>,
}

pub(crate) fn frontmost_app() -> Option<FrontmostApp> {
    // SAFETY: `app` is borrowed for the duration of these property reads; each
    // selector is a plain getter on `NSRunningApplication`.
    unsafe {
        let app = frontmost_running_app().ok()?;
        Some(FrontmostApp {
            pid: objc2::msg_send![app, processIdentifier],
            bundle_id: ns_string(objc2::msg_send![app, bundleIdentifier]),
            name: ns_string(objc2::msg_send![app, localizedName]),
        })
    }
}

/// How long an AX read into another process may block before it gives up.
///
/// An AX attribute read is a synchronous round trip into the target app's run
/// loop, and the caller here is the dictation hotkey. A beachballed Slack
/// would otherwise hold that thread for the system default, which is generous.
/// Confirmed against the SDK header: `AXError AXUIElementSetMessagingTimeout
/// (AXUIElementRef element, float timeoutInSeconds)` — seconds, per element,
/// and set on a non-system-wide element it applies to that element only.
const AX_MESSAGING_TIMEOUT_SECS: f32 = 0.25;

/// Caps how long messages sent *to this element* may block.
///
/// Per the SDK header, the timeout is set on one object and is explicitly
/// **not** shared with other objects — not even ones copied out of it. So
/// every element a caller is about to read from needs its own call; setting it
/// once on the application element and assuming the focused window inherits it
/// is the mistake that leaves the second read on the system default.
///
/// The system-wide element is deliberately not used for this: its timeout is
/// process-global, and quietly changing the global for every other AX caller
/// in KEA (the insertion path included) is not this function's business.
///
/// A failure only means the default timeout stays in force, so it is ignored.
pub(crate) fn set_ax_timeout(element: &AxRef) {
    // SAFETY: `element` owns a live AX reference for the duration of the call.
    unsafe {
        let _ = AXUIElementSetMessagingTimeout(element.as_ptr(), AX_MESSAGING_TIMEOUT_SECS);
    }
}

/// The AX element for a process, with the messaging timeout already set.
pub(crate) fn app_ax_element(pid: i32) -> Option<AxRef> {
    // SAFETY: `AXUIElementCreateApplication` follows the CF create rule, so
    // the +1 reference is handed straight to `AxRef`, which releases it once.
    let element = unsafe { AxRef::new(AXUIElementCreateApplication(pid))? };
    set_ax_timeout(&element);
    Some(element)
}

/// The process-wide AX root.
///
/// # Safety
/// Returns an owned reference; `AxRef` releases it.
pub(crate) unsafe fn system_wide_element() -> Option<AxRef> {
    AxRef::new(AXUIElementCreateSystemWide())
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
        let Some(system) = system_wide_element() else {
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
///
/// `pub(crate)` for the app-context probe next door: it walks two more AX
/// attributes on every dictation, and a second hand-rolled `CFRelease` path is
/// precisely how the leak the design review found got introduced.
pub(crate) struct AxRef(*mut c_void);

impl AxRef {
    /// Takes ownership of a +1 reference, or `None` if it is null.
    pub(crate) unsafe fn new(raw: *mut c_void) -> Option<Self> {
        if raw.is_null() {
            None
        } else {
            Some(Self(raw))
        }
    }

    /// The borrowed raw reference, for the AX calls that take an element and
    /// are not attribute reads. Ownership stays with this `AxRef`.
    pub(crate) fn as_ptr(&self) -> *mut c_void {
        self.0
    }

    /// Reads one AX attribute as an owned value, or `None` on any AX error.
    pub(crate) unsafe fn copy_attr(&self, attribute: &str) -> Option<Self> {
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
    pub(crate) unsafe fn copy_string_attr(&self, attribute: &str) -> Option<String> {
        let value = self.copy_attr(attribute)?;
        let cf_type = value.0 as core_foundation_sys::base::CFTypeRef;
        if core_foundation::base::CFGetTypeID(cf_type)
            != core_foundation::string::CFString::type_id()
        {
            return None;
        }
        // Get rule: `value` still owns the reference and releases it on drop.
        let s = core_foundation::string::CFString::wrap_under_get_rule(
            value.0 as core_foundation_sys::string::CFStringRef,
        );
        Some(s.to_string())
    }

    /// Writes one AX attribute from a `&str`, naming the AX error code on
    /// failure — `-25204` (attribute unsupported) and `-25205` (read-only) are
    /// the ordinary answers from an element that simply cannot take text, and
    /// they are what the caller's clipboard fallback is for.
    unsafe fn set_string_attr(&self, attribute: &str, value: &str) -> Result<(), String> {
        let attr = core_foundation::string::CFString::new(attribute);
        let cf_value = core_foundation::string::CFString::new(value);
        let err = AXUIElementSetAttributeValue(
            self.0,
            attr.as_concrete_TypeRef(),
            cf_value.as_concrete_TypeRef() as *const _,
        );
        if err != K_AX_ERROR_SUCCESS {
            return Err(format!("AXUIElementSetAttributeValue failed ({err})"));
        }
        Ok(())
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

    // SAFETY: as in `focused_element_role` — the system-wide element and the
    // focused element are both +1 references from AX create/copy functions,
    // and `AxRef` releases each exactly once. This runs on every dictation, so
    // doing it by hand here is how the references used to leak.
    unsafe {
        let Some(system) = system_wide_element() else {
            return Err("AXUIElementCreateSystemWide failed".into());
        };
        let Some(focused) = system.copy_attr("AXFocusedUIElement") else {
            return Err("no focused UI element".into());
        };
        focused.set_string_attr("AXSelectedText", text)
    }
}

/// Put `to` back where `from` is now, inside the focused element.
///
/// # Why the whole value, and not a selection
///
/// The undo this serves runs *after* the rewrite: nothing is selected any
/// more, so `AXSelectedText` — the attribute
/// [`insert_via_accessibility`] writes — has nothing to act on. What the
/// focused element still has is `AXValue`, its entire text. So the swap reads
/// that, finds the one occurrence of what KEA wrote (see
/// [`super::swap_once`] for why exactly one), and writes the whole value back.
///
/// Two consequences worth stating rather than discovering:
///
/// * The caret and the app's own undo stack are not preserved. Setting
///   `AXValue` is a whole-field write as far as the app is concerned.
/// * Plenty of elements refuse it — anything that is not a real text field,
///   and most web and Electron surfaces, answer `-25204` (unsupported) or
///   `-25205` (read-only). That is an ordinary outcome here, reported as an
///   error the user sees, not a bug: the alternative would be typing over
///   text the app did not agree to hand back.
///
/// # Manual verification (macOS)
/// 1. Rewrite a sentence in TextEdit, then press the undo shortcut: the
///    original sentence is back, character for character.
/// 2. Type something after the rewrite, then undo: the original comes back and
///    what was typed afterwards is still there.
/// 3. Delete the rewritten sentence, then undo: KEA refuses and says the text
///    is no longer there. Nothing else in the document changes.
/// 4. Rewrite in Slack or a browser text box: the swap is likely refused by
///    the element; the message says so and nothing is overwritten.
pub fn swap_in_focused_element(from: &str, to: &str) -> Result<(), String> {
    if !is_ax_trusted() {
        return Err("accessibility permission not granted".into());
    }

    // SAFETY: as in `insert_via_accessibility_impl` — the system-wide element
    // and the focused element are both +1 references from AX create/copy
    // functions, released exactly once by `AxRef`.
    unsafe {
        let Some(system) = system_wide_element() else {
            return Err("AXUIElementCreateSystemWide failed".into());
        };
        let Some(focused) = system.copy_attr("AXFocusedUIElement") else {
            return Err("no focused UI element".into());
        };
        let Some(value) = focused.copy_string_attr("AXValue") else {
            return Err("that app will not let KEA read the text back".into());
        };
        let swapped = super::swap_once(&value, from, to).map_err(|e| e.to_string())?;
        focused.set_string_attr("AXValue", &swapped)
    }
}

const K_AX_ERROR_SUCCESS: i32 = 0;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXUIElementCreateSystemWide() -> *mut c_void;
    fn AXUIElementCreateApplication(pid: i32) -> *mut c_void;
    fn AXUIElementSetMessagingTimeout(element: *mut c_void, timeout_in_seconds: f32) -> i32;
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
        let _insert = AxInsertOverride::force(Box::new(|text| {
            if text == "ok" {
                Ok(())
            } else {
                Err("boom".into())
            }
        }));
        assert!(insert_via_accessibility("ok").is_ok());
        assert!(insert_via_accessibility("nope").is_err());
    }
}
