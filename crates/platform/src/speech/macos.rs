//! `SFSpeechRecognizer`, reached by runtime lookup.
//!
//! See the parent module for the evidence that `SpeechAnalyzer` cannot be
//! reached this way and for the three external requirements (Info.plist key,
//! TCC grant, framework load).
//!
//! # Manual verification (not exercised by `cargo test`)
//!
//! 1. With no grant yet, open Settings; the status reads "not determined" and
//!    **no dialog appears** — only [`MacSpeechRecognition::request_authorization`]
//!    may prompt.
//! 2. Ask for the grant. The dialog shows the
//!    `NSSpeechRecognitionUsageDescription` string. Without that key in
//!    Info.plist the process *aborts here*; that is Apple's behaviour, not
//!    something this module can trap.
//! 3. Dictate a short sentence with Wi-Fi **off**. It must still transcribe:
//!    `requiresOnDeviceRecognition` is set, so a result proves nothing left
//!    the machine.
//! 4. Dictate for more than a minute. Apple stops the task; the error text
//!    surfaces verbatim rather than as an empty transcript.
//! 5. Revoke the grant in System Settings mid-session. The next call must
//!    report `NotAuthorized`, not hang.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::path::Path;
use std::sync::mpsc;
use std::sync::OnceLock;
use std::time::Duration;

use block2::RcBlock;
use objc2::msg_send;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2_foundation::NSString;

use super::{
    SpeechAuth, SpeechError, SpeechOpts, SpeechRecognition, SpeechSegment, SpeechTranscript,
};

const SPEECH_FRAMEWORK: &CStr = c"/System/Library/Frameworks/Speech.framework/Speech";

// dlopen's own constants; declared here rather than taking a `libc`
// dependency for two integers — the same call this crate already makes for
// Vision.
const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
}

/// `SFSpeechRecognizerAuthorizationStatus` values, in declaration order.
const AUTH_NOT_DETERMINED: i64 = 0;
const AUTH_DENIED: i64 = 1;
const AUTH_RESTRICTED: i64 = 2;
const AUTH_AUTHORIZED: i64 = 3;

/// How long to wait for the recognizer's result block.
///
/// Apple caps a task at one minute of *audio*; the wall clock can be longer
/// on a cold model load, and a task that is never going to answer — a
/// revoked grant mid-flight, a wedged daemon — would otherwise block the
/// worker thread forever. Generous enough that a real recognition never
/// trips it, finite so a broken one cannot hang the app.
const RESULT_TIMEOUT: Duration = Duration::from_secs(180);

/// Load Speech once per process.
///
/// The handle is deliberately never closed: the classes looked up below have
/// to keep resolving. A Tauri process does **not** have this framework loaded
/// — measured — and `class!` *panics* on an unregistered class, so every
/// entry point here goes through [`speech_class`] rather than `class!`.
fn speech_available() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        // SAFETY: a constant, NUL-terminated absolute path. `dlopen` returns
        // null rather than trapping when the framework is absent.
        let handle = unsafe { dlopen(SPEECH_FRAMEWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
        if handle.is_null() {
            tracing::warn!("speech: Speech.framework could not be loaded");
        }
        !handle.is_null()
    })
}

fn speech_class(name: &CStr) -> Option<&'static AnyClass> {
    if !speech_available() {
        return None;
    }
    AnyClass::get(name)
}

pub struct MacSpeechRecognition;

impl MacSpeechRecognition {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacSpeechRecognition {
    fn default() -> Self {
        Self::new()
    }
}

fn auth_from_status(status: i64) -> SpeechAuth {
    match status {
        AUTH_AUTHORIZED => SpeechAuth::Granted,
        // "Restricted" is a device policy the user cannot lift from here, so
        // it is the same actionable state as a denial: the engine is not
        // usable and asking again will not help.
        AUTH_DENIED | AUTH_RESTRICTED => SpeechAuth::Denied,
        AUTH_NOT_DETERMINED => SpeechAuth::NotDetermined,
        other => {
            tracing::warn!(status = other, "speech: unknown authorization status");
            SpeechAuth::Unavailable
        }
    }
}

/// Builds an `SFSpeechRecognizer` for `locale`, or `None`.
///
/// `initWithLocale:` is documented nullable and returns nil for a language
/// the system cannot recognize, which is why every caller here handles the
/// null rather than unwrapping it.
///
/// # Safety
/// Sends Objective-C messages; the class is checked for registration first
/// and every returned pointer is null-checked.
unsafe fn make_recognizer(locale: Option<&str>) -> Option<Retained<AnyObject>> {
    let class = speech_class(c"SFSpeechRecognizer")?;
    let recognizer: *mut AnyObject = msg_send![class, alloc];
    let recognizer: *mut AnyObject = match locale {
        Some(tag) => {
            let identifier = NSString::from_str(tag);
            let locale: *mut AnyObject = msg_send![AnyClass::get(c"NSLocale")?, alloc];
            let locale: *mut AnyObject = msg_send![locale, initWithLocaleIdentifier: &*identifier];
            if locale.is_null() {
                return None;
            }
            let locale = Retained::from_raw(locale)?;
            msg_send![recognizer, initWithLocale: &*locale]
        }
        // The no-argument initializer follows the user's own language
        // settings, which is the right default for somebody who has chosen
        // nothing.
        None => msg_send![recognizer, init],
    };
    Retained::from_raw(recognizer)
}

impl SpeechRecognition for MacSpeechRecognition {
    fn authorization(&self) -> SpeechAuth {
        let Some(class) = speech_class(c"SFSpeechRecognizer") else {
            return SpeechAuth::Unavailable;
        };
        // SAFETY: `+authorizationStatus` takes no arguments, returns an
        // NSInteger, and never prompts.
        let status: i64 = unsafe { msg_send![class, authorizationStatus] };
        auth_from_status(status)
    }

    fn request_authorization(&self) -> SpeechAuth {
        let current = self.authorization();
        // Only `NotDetermined` can produce a dialog, and asking again in any
        // other state is a round-trip that cannot change the answer.
        if current != SpeechAuth::NotDetermined {
            return current;
        }
        let Some(class) = speech_class(c"SFSpeechRecognizer") else {
            return SpeechAuth::Unavailable;
        };

        let (tx, rx) = mpsc::channel::<i64>();
        let block = RcBlock::new(move |status: i64| {
            // The receiver may already be gone if the wait below timed out;
            // a failed send is the normal shape of that and not an error.
            let _ = tx.send(status);
        });
        // SAFETY: `+requestAuthorization:` takes one block of `(NSInteger) ->
        // void`, which is what `block` is. Apple's header: this call aborts
        // the process when NSSpeechRecognitionUsageDescription is missing
        // from Info.plist — see the trait's doc comment.
        unsafe {
            let _: () = msg_send![class, requestAuthorization: RcBlock::as_ptr(&block)];
        }
        // The dialog is modal to the user, not to us, so this waits as long
        // as somebody might take to read it — but not forever.
        match rx.recv_timeout(Duration::from_secs(120)) {
            Ok(status) => auth_from_status(status),
            Err(_) => {
                tracing::warn!("speech: authorization prompt did not answer");
                self.authorization()
            }
        }
    }

    fn supports_on_device(&self, locale: Option<&str>) -> bool {
        // SAFETY: see `make_recognizer`; the result is null-checked there.
        autoreleasepool(|_| unsafe {
            let Some(recognizer) = make_recognizer(locale) else {
                return false;
            };
            let supports: Bool = msg_send![&*recognizer, supportsOnDeviceRecognition];
            supports.is_true()
        })
    }

    fn transcribe_file(
        &self,
        path: &Path,
        opts: &SpeechOpts,
    ) -> Result<SpeechTranscript, SpeechError> {
        // Checked before touching the recognizer so a denied grant is a clear
        // error rather than a task that answers nothing, and so nothing here
        // can trigger a prompt the user did not ask for.
        match self.authorization() {
            SpeechAuth::Granted => {}
            SpeechAuth::Unavailable => return Err(SpeechError::Unavailable),
            _ => return Err(SpeechError::NotAuthorized),
        }
        autoreleasepool(|_| unsafe { run_recognition(path, opts) })
    }
}

/// # Safety
/// Sends Objective-C messages; every class is checked for registration and
/// every pointer for null before use.
unsafe fn run_recognition(path: &Path, opts: &SpeechOpts) -> Result<SpeechTranscript, SpeechError> {
    let recognizer =
        make_recognizer(opts.locale.as_deref()).ok_or_else(|| match opts.locale.as_deref() {
            Some(tag) => SpeechError::Failed(format!("no speech recognizer for locale {tag}")),
            None => SpeechError::Unavailable,
        })?;

    // `isAvailable` false means the service cannot run right now — a model
    // still installing, a locale that needs the network we are refusing to
    // use. Saying so beats a task that returns an error minutes later.
    let available: Bool = msg_send![&*recognizer, isAvailable];
    if !available.is_true() {
        return Err(SpeechError::Failed(
            "the speech recognizer is not available right now".into(),
        ));
    }

    let request_class =
        speech_class(c"SFSpeechURLRecognitionRequest").ok_or(SpeechError::Unavailable)?;
    let url_string = NSString::from_str(&path.to_string_lossy());
    let url_class = AnyClass::get(c"NSURL").ok_or(SpeechError::Unavailable)?;
    let url: *mut AnyObject = msg_send![url_class, fileURLWithPath: &*url_string];
    if url.is_null() {
        return Err(SpeechError::Failed(format!(
            "could not address {}",
            path.display()
        )));
    }

    let request: *mut AnyObject = msg_send![request_class, alloc];
    let request: *mut AnyObject = msg_send![request, initWithURL: url];
    let request = Retained::from_raw(request)
        .ok_or_else(|| SpeechError::Failed("could not build a recognition request".into()))?;

    // The whole point of this engine: nothing leaves the machine. An
    // unsupported system fails the task rather than quietly uploading, which
    // is why `supports_on_device` exists as a separate question a picker can
    // ask first.
    let _: () = msg_send![&*request, setRequiresOnDeviceRecognition: Bool::YES];
    // One result, at the end. Partial results would call the handler many
    // times and this is a file, not a live stream.
    let _: () = msg_send![&*request, setShouldReportPartialResults: Bool::NO];
    // Dictated text without punctuation is not usable output.
    let _: () = msg_send![&*request, setAddsPunctuation: Bool::YES];

    if !opts.vocabulary.is_empty() {
        if let Some(strings) = string_array(&opts.vocabulary) {
            let _: () = msg_send![&*request, setContextualStrings: &*strings];
        }
    }

    let (tx, rx) = mpsc::channel::<Result<SpeechTranscript, SpeechError>>();
    let handler = RcBlock::new(move |result: *mut AnyObject, error: *mut AnyObject| {
        // Both null would be a framework contract violation; treat it as the
        // error it is rather than as an empty transcript.
        let outcome = if !error.is_null() {
            Err(SpeechError::Failed(ns_error_message(error)))
        } else if result.is_null() {
            Err(SpeechError::Failed(
                "the recognizer returned neither a result nor an error".into(),
            ))
        } else {
            // `shouldReportPartialResults` is off, so the first call is the
            // final one. Checking `isFinal` anyway costs one message send and
            // means a future change to that flag cannot silently start
            // delivering half a sentence.
            let is_final: Bool = msg_send![result, isFinal];
            if is_final.is_true() {
                Ok(read_result(result))
            } else {
                return;
            }
        };
        // The receiver is gone if the wait timed out; that is the normal
        // shape of a give-up, not an error.
        let _ = tx.send(outcome);
    });

    let _: *mut AnyObject = msg_send![
        &*recognizer,
        recognitionTaskWithRequest: &*request,
        resultHandler: RcBlock::as_ptr(&handler)
    ];

    match rx.recv_timeout(RESULT_TIMEOUT) {
        Ok(outcome) => outcome,
        Err(_) => Err(SpeechError::Failed(
            "the speech recognizer did not answer".into(),
        )),
    }
}

/// An `NSArray<NSString *>` from a Rust slice.
///
/// # Safety
/// Sends Objective-C messages.
unsafe fn string_array(values: &[String]) -> Option<Retained<AnyObject>> {
    let class = AnyClass::get(c"NSMutableArray")?;
    let array: *mut AnyObject = msg_send![class, alloc];
    let array: *mut AnyObject = msg_send![array, init];
    let array = Retained::from_raw(array)?;
    for value in values {
        let string = NSString::from_str(value);
        let _: () = msg_send![&*array, addObject: &*string];
    }
    Some(array)
}

/// Reads `bestTranscription` and its segments off an `SFSpeechRecognitionResult`.
///
/// # Safety
/// `result` must be a non-null `SFSpeechRecognitionResult`.
unsafe fn read_result(result: *mut AnyObject) -> SpeechTranscript {
    let transcription: *mut AnyObject = msg_send![result, bestTranscription];
    if transcription.is_null() {
        return SpeechTranscript::default();
    }
    let text = ns_string_to_rust(msg_send![transcription, formattedString]).unwrap_or_default();

    let segments: *mut AnyObject = msg_send![transcription, segments];
    let mut out = Vec::new();
    if !segments.is_null() {
        let count: usize = msg_send![segments, count];
        for index in 0..count {
            let segment: *mut AnyObject = msg_send![segments, objectAtIndex: index];
            if segment.is_null() {
                continue;
            }
            let Some(substring) = ns_string_to_rust(msg_send![segment, substring]) else {
                continue;
            };
            let substring = substring.trim().to_string();
            if substring.is_empty() {
                continue;
            }
            // Seconds as a double on the wire, milliseconds everywhere here —
            // the same conversion every other backend's timing goes through.
            let timestamp: f64 = msg_send![segment, timestamp];
            let duration: f64 = msg_send![segment, duration];
            let start_ms = seconds_to_ms(timestamp);
            out.push(SpeechSegment {
                start_ms,
                end_ms: start_ms.saturating_add(seconds_to_ms(duration)),
                text: substring,
            });
        }
    }

    SpeechTranscript {
        text,
        segments: out,
    }
}

fn seconds_to_ms(seconds: f64) -> u64 {
    if seconds.is_finite() && seconds > 0.0 {
        (seconds * 1000.0).round() as u64
    } else {
        0
    }
}

/// # Safety
/// `error` must be an `NSError` or null.
unsafe fn ns_error_message(error: *mut AnyObject) -> String {
    ns_string_to_rust(msg_send![error, localizedDescription])
        .unwrap_or_else(|| "speech recognition failed".into())
}

/// # Safety
/// `string` must be an `NSString` or null.
unsafe fn ns_string_to_rust(string: *mut AnyObject) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![string, UTF8String];
    if utf8.is_null() {
        return None;
    }
    CStr::from_ptr(utf8).to_str().ok().map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The status mapping, which is the one piece of this module that is pure
    /// and therefore testable without a grant. "Restricted" folding into
    /// "denied" is deliberate — see the comment there.
    #[test]
    fn authorization_statuses_map_to_actionable_states() {
        assert_eq!(auth_from_status(AUTH_AUTHORIZED), SpeechAuth::Granted);
        assert_eq!(auth_from_status(AUTH_DENIED), SpeechAuth::Denied);
        assert_eq!(auth_from_status(AUTH_RESTRICTED), SpeechAuth::Denied);
        assert_eq!(
            auth_from_status(AUTH_NOT_DETERMINED),
            SpeechAuth::NotDetermined
        );
        // A value from a future OS is unavailable, not silently authorized.
        assert_eq!(auth_from_status(99), SpeechAuth::Unavailable);
    }

    #[test]
    fn segment_timing_converts_seconds_to_milliseconds() {
        assert_eq!(seconds_to_ms(1.2345), 1_235);
        // A negative or NaN timestamp from a misbehaving framework becomes
        // zero rather than an enormous or nonsensical offset.
        assert_eq!(seconds_to_ms(-1.0), 0);
        assert_eq!(seconds_to_ms(f64::NAN), 0);
    }

    /// The whole reason this module uses `dlopen` instead of `class!`: in a
    /// process that has not loaded Speech.framework, `class!(SFSpeechRecognizer)`
    /// panics. After the load, the class resolves. This asserts the second
    /// half — the first half was measured and is recorded in the module doc.
    #[test]
    fn the_framework_loads_and_registers_its_classes() {
        assert!(speech_available(), "Speech.framework should load on macOS");
        assert!(speech_class(c"SFSpeechRecognizer").is_some());
        assert!(speech_class(c"SFSpeechURLRecognitionRequest").is_some());
        // And the Swift-only analyzer is still not there, which is the
        // finding the parent module's doc comment rests on. If this ever
        // starts passing, the better API has become reachable.
        assert!(
            AnyClass::get(c"SpeechAnalyzer").is_none(),
            "SpeechAnalyzer is now an Objective-C class; revisit the module doc"
        );
    }
}
