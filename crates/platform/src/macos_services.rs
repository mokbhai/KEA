//! macOS Services menu integration (NSServices).
//!
//! Registers a "Rewrite with KEA" entry in the Services menu that extracts the
//! selected text from the pasteboard and dispatches it through a Tokio channel
//! into the existing rewrite pipeline.
//!
//! # Threading
//! The ObjC service callback fires on the main thread. Text extraction from
//! NSPasteboard is fast (no I/O), and the channel send is non-blocking
//! (unbounded), so we handle it inline without spawning.

use std::ffi::CStr;
use std::sync::OnceLock;

use objc2::runtime::{AnyClass, AnyObject, Bool, Sel};
use objc2::{class, msg_send, sel, ClassType};
use objc2_foundation::ns_string;
use tokio::sync::mpsc;

static REWRITE_TX: OnceLock<mpsc::UnboundedSender<String>> = OnceLock::new();

unsafe extern "C-unwind" fn rewrite_with_kea(
    _this: *mut AnyObject,
    _cmd: Sel,
    pboard: *mut AnyObject,
    _user_data: *mut AnyObject,
    _error: *mut *mut AnyObject,
) {
    if pboard.is_null() {
        tracing::warn!("macos_services: null pasteboard");
        return;
    }
    let pboard: &AnyObject = unsafe { &*pboard };
    let pb_type = ns_string!("public.utf8-plain-text");
    let text: *mut AnyObject = unsafe { msg_send![pboard, stringForType: pb_type] };
    if text.is_null() {
        tracing::warn!("macos_services: pasteboard contained no UTF-8 plain text");
        return;
    }
    let len: usize = unsafe { msg_send![text, length] };
    if len == 0 {
        return;
    }
    let utf8: *const std::ffi::c_char = unsafe { msg_send![text, UTF8String] };
    let s = match unsafe { CStr::from_ptr(utf8) }.to_str() {
        Ok(s) => s.to_owned(),
        Err(e) => {
            tracing::warn!(error = %e, "macos_services: pasteboard text is not valid UTF-8");
            return;
        }
    };
    if let Some(tx) = REWRITE_TX.get() {
        let _ = tx.send(s);
    }
}

pub fn register_rewrite_service(tx: mpsc::UnboundedSender<String>) {
    REWRITE_TX
        .set(tx)
        .expect("register_rewrite_service called more than once");

    let ns_object: &AnyClass = <objc2::runtime::NSObject as ClassType>::class();

    let cls: &'static AnyClass = unsafe {
        let name = std::ffi::CString::new("KeaServiceProvider").unwrap();
        let cls_raw: *mut AnyClass =
            objc2::ffi::objc_allocateClassPair(ns_object as *const AnyClass, name.as_ptr(), 0);
        assert!(!cls_raw.is_null(), "objc_allocateClassPair failed");

        let sel = sel!(rewriteWithKEA:userData:error:);
        let fn_ptr: unsafe extern "C-unwind" fn() =
            std::mem::transmute(rewrite_with_kea as *const ());
        let types_str = std::ffi::CString::new("v@:@@^@").unwrap();
        let ok: Bool =
            objc2::ffi::class_addMethod(cls_raw, sel, fn_ptr, types_str.as_ptr());
        assert!(ok.is_true(), "class_addMethod failed");

        objc2::ffi::objc_registerClassPair(cls_raw);
        &*(cls_raw as *const AnyClass)
    };

    let instance: *mut AnyObject = unsafe { msg_send![cls, new] };
    let provider: &AnyObject = unsafe { &*instance };

    let app: *mut AnyObject = unsafe { msg_send![class!(NSApplication), sharedApplication] };
    unsafe {
        let _: () = msg_send![app, setServicesProvider: provider];
        // Refresh the system Services cache. This is the free AppKit C
        // function `NSUpdateDynamicServices()`, NOT an ObjC method — the
        // previous `msg_send![cls, updateDynamicServices]` sent an
        // unrecognized selector to the class object, raising an ObjC
        // exception that unwound through msg_send and aborted the app at
        // launch.
        NSUpdateDynamicServices();
    }
}

extern "C" {
    /// AppKit: refreshes the Services menu from the on-disk service definitions.
    fn NSUpdateDynamicServices();
}
