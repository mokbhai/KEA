//! macOS region capture (`/usr/sbin/screencapture -i`) and OCR (Vision).
//!
//! The FFI edge of this module. Everything decidable without a screen —
//! argument construction, cancel detection, the coordinate flip, reading
//! order — lives in [`super`] as plain functions with tests; what is left here
//! is process spawning and Objective-C message sends.
//!
//! # Why Vision is reached by `dlopen` and not by a typed crate
//!
//! The repo's precedent is runtime lookup: `textio/macos_pasteboard.rs` spells
//! out why NSPasteboard is looked up by name rather than linking
//! `objc2-app-kit`, and `src-tauri/Cargo.toml` says the same about AppKit. The
//! same argument holds here — `objc2`, `objc2-foundation` and `block2` are
//! already dependencies, and the surface used below is six selectors.
//!
//! One thing does differ, and it is the reason for `dlopen` rather than a bare
//! `AnyClass::get`. AppKit is already loaded in any process that has a
//! pasteboard; Vision is not loaded in an ordinary process at all. Measured on
//! this machine: `objc_getClass("VNRecognizeTextRequest")` returns NULL before
//! the framework is opened and a valid class immediately after. So the lookup
//! has to load the framework first, and a missing framework becomes a typed
//! error rather than a link failure at build time.
//!
//! # Manual verification — region capture
//! 1. Call `capture_region()`. The standard crosshair appears; space switches
//!    to window selection, and the shutter sound must **not** play (`-x`).
//! 2. Drag a region. The call returns [`CaptureOutcome::Captured`] and
//!    `path()` points at a PNG inside `$TMPDIR/kea-screen/capture-*/`.
//! 3. Drop the [`CapturedImage`]; that directory must be gone. Check with
//!    `ls $TMPDIR/kea-screen` — it should be empty between captures. A file
//!    left there is the worst bug this feature can have.
//! 4. Repeat and press **Esc**. The call must return
//!    [`CaptureOutcome::Cancelled`], not an error, and leave nothing behind.
//! 5. Repeat on a second display, and on a Retina and a non-Retina display.
//!    The PNG's pixel size should match the display's backing scale — a
//!    1000x200 selection on a Retina display is a 2000x400 PNG. Do not resize
//!    it; a downscaled image is the usual cause of bad recognition.
//! 6. **Open gate:** whether macOS 15+ prompts for Screen Recording on the
//!    first interactive capture. Run once from a build that has never held the
//!    permission and watch for the dialog. If it does prompt, the settings
//!    page should request `PermKind::ScreenRecording` up front like the
//!    meeting path does, rather than letting the prompt land mid-selection.
//!
//! # Manual verification — OCR
//! 1. Capture a region of a PDF in Preview; the recognised text should match.
//! 2. Capture a terminal full of code with `language_correction: false`;
//!    identifiers such as `snake_case_name` must survive. Repeat with it on
//!    and watch them get "corrected" — that is the setting earning its place.
//! 3. Capture a two-column web page. Each column must read down before the
//!    next begins; text alternating between columns means `reading_order`
//!    regressed.
//! 4. Capture a non-Latin-script page (Japanese, Russian) with `languages`
//!    set to the matching tag from [`MacTextRecognizer::supported_languages`].
//! 5. Capture a region with no text at all; the result must be an empty
//!    `Vec`, not an error.

use std::ffi::{c_char, c_int, c_void, CStr};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use async_trait::async_trait;
use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyClass, AnyObject, Bool};
use objc2::{class, msg_send};
use objc2_foundation::{NSRect, NSString};

use super::{
    capture_parent_dir, classify_capture, png_dimensions, screencapture_args, CaptureOutcome,
    CaptureSlot, CaptureVerdict, NormalizedRect, Observation, OcrOptions, ScreenCapture,
    ScreenError, TextRecognizer,
};

/// Always the absolute path: a capture must never resolve through `PATH`,
/// where anything could be shadowing the name.
const SCREENCAPTURE: &str = "/usr/sbin/screencapture";

/// The file name inside the slot's private directory. The extension matters —
/// `screencapture` picks its format from it as well as from `-tpng`.
const CAPTURE_FILE_NAME: &str = "region.png";

pub struct MacScreenCapture {
    tool: PathBuf,
    staging: PathBuf,
}

impl MacScreenCapture {
    pub fn new() -> Self {
        Self {
            tool: PathBuf::from(SCREENCAPTURE),
            staging: capture_parent_dir(),
        }
    }
}

impl Default for MacScreenCapture {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ScreenCapture for MacScreenCapture {
    fn availability(&self) -> Result<(), ScreenError> {
        let metadata = std::fs::metadata(&self.tool).map_err(|e| {
            ScreenError::Unavailable(format!("{} is not present ({e})", self.tool.display()))
        })?;
        if !metadata.is_file() || metadata.permissions().mode() & 0o111 == 0 {
            return Err(ScreenError::Unavailable(format!(
                "{} is not an executable file",
                self.tool.display()
            )));
        }
        Ok(())
    }

    async fn capture_region(&self) -> Result<CaptureOutcome, ScreenError> {
        // Checked before spawning so a missing tool is one clear error rather
        // than a confusing "no such file" from the process layer.
        self.availability()?;

        let tool = self.tool.clone();
        let staging = self.staging.clone();
        // The selection UI runs for as long as the user takes — seconds to
        // minutes. That belongs on the blocking pool, which exists for exactly
        // this, rather than on a reactor thread.
        tokio::task::spawn_blocking(move || capture_blocking(&tool, &staging))
            .await
            .map_err(|e| ScreenError::Capture(format!("capture task failed: {e}")))?
    }
}

fn capture_blocking(tool: &Path, staging: &Path) -> Result<CaptureOutcome, ScreenError> {
    // The slot owns the cleanup: every `return` below drops it, and dropping
    // removes the directory and anything in it.
    let slot = CaptureSlot::claim(staging, CAPTURE_FILE_NAME)?;

    let output = std::process::Command::new(tool)
        .args(screencapture_args(slot.path()))
        .output()
        .map_err(|e| ScreenError::Capture(format!("{} failed to run: {e}", tool.display())))?;

    let stderr = String::from_utf8_lossy(&output.stderr);
    match classify_capture(slot.written_bytes(), &stderr) {
        CaptureVerdict::Captured => Ok(CaptureOutcome::Captured(slot.finish())),
        CaptureVerdict::Cancelled => Ok(CaptureOutcome::Cancelled),
        CaptureVerdict::Failed(message) => Err(ScreenError::Capture(message)),
    }
}

// ---------------------------------------------------------------------------
// Vision
// ---------------------------------------------------------------------------

/// `VNRequestTextRecognitionLevelAccurate`. Measured on this machine: the
/// accurate level is 0 and the fast level is 1. Accurate is right here — this
/// is a one-shot, user-initiated action, so a few hundred extra milliseconds
/// are invisible and a wrong character is not.
const RECOGNITION_LEVEL_ACCURATE: isize = 0;

const VISION_FRAMEWORK: &CStr = c"/System/Library/Frameworks/Vision.framework/Vision";

// dlopen's own constants; declared here rather than taking a `libc`
// dependency for two integers.
const RTLD_LAZY: c_int = 0x1;
const RTLD_LOCAL: c_int = 0x4;

extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
}

/// Load Vision once per process.
///
/// The handle is deliberately never closed: the framework has to stay mapped
/// for the class lookups below to keep resolving, and a process that has done
/// OCR once will almost certainly do it again.
fn vision_available() -> bool {
    static LOADED: OnceLock<bool> = OnceLock::new();
    *LOADED.get_or_init(|| {
        // SAFETY: a constant, NUL-terminated absolute path. `dlopen` returns
        // null rather than trapping when the framework is absent, which is the
        // degradation this function reports.
        let handle = unsafe { dlopen(VISION_FRAMEWORK.as_ptr(), RTLD_LAZY | RTLD_LOCAL) };
        if handle.is_null() {
            tracing::warn!("screen: Vision.framework could not be loaded; OCR unavailable");
        }
        !handle.is_null()
    })
}

fn vision_class(name: &CStr) -> Result<&'static AnyClass, ScreenError> {
    if !vision_available() {
        return Err(ScreenError::Recognize(
            "Vision.framework could not be loaded".into(),
        ));
    }
    AnyClass::get(name).ok_or_else(|| {
        ScreenError::Recognize(format!(
            "{} is not available on this macOS version",
            name.to_string_lossy()
        ))
    })
}

pub struct MacTextRecognizer;

impl MacTextRecognizer {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacTextRecognizer {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl TextRecognizer for MacTextRecognizer {
    fn supported_languages(&self) -> Result<Vec<String>, ScreenError> {
        // Asked of the framework rather than shipped as a guessed list: the
        // set differs by macOS version and by recognition level.
        autoreleasepool(|_| unsafe {
            let request = new_text_request(&OcrOptions::default())?;
            let mut error: *mut AnyObject = std::ptr::null_mut();
            let languages: *mut AnyObject = msg_send![
                &*request,
                supportedRecognitionLanguagesAndReturnError: (&mut error) as *mut *mut AnyObject
            ];
            if languages.is_null() {
                return Err(ScreenError::Recognize(ns_error_message(error)));
            }
            let count: usize = msg_send![languages, count];
            let mut tags = Vec::with_capacity(count);
            for index in 0..count {
                let tag: *mut AnyObject = msg_send![languages, objectAtIndex: index];
                if let Some(tag) = ns_string_to_rust(tag) {
                    tags.push(tag);
                }
            }
            Ok(tags)
        })
    }

    async fn recognize(
        &self,
        image: &Path,
        opts: &OcrOptions,
    ) -> Result<Vec<Observation>, ScreenError> {
        let image = image.to_path_buf();
        let opts = opts.clone();
        // Recognition is hundreds of milliseconds of CPU on the accurate
        // model. Nothing Objective-C crosses the boundary — the objects are
        // created and released inside the closure.
        tokio::task::spawn_blocking(move || recognize_blocking(&image, &opts))
            .await
            .map_err(|e| ScreenError::Recognize(format!("recognition task failed: {e}")))?
    }
}

fn recognize_blocking(image: &Path, opts: &OcrOptions) -> Result<Vec<Observation>, ScreenError> {
    let (width, height) = image_dimensions(image);
    let path = image.to_string_lossy().into_owned();

    autoreleasepool(|_| unsafe {
        let request = new_text_request(opts)?;
        let handler = new_image_handler(&path)?;

        let requests: *mut AnyObject = msg_send![class!(NSArray), arrayWithObject: &*request];
        let mut error: *mut AnyObject = std::ptr::null_mut();
        let performed: Bool = msg_send![
            &*handler,
            performRequests: requests,
            error: (&mut error) as *mut *mut AnyObject
        ];
        if !performed.is_true() {
            return Err(ScreenError::Recognize(ns_error_message(error)));
        }

        // `results` is nil rather than empty when a request found nothing, so
        // "no text" arrives here as an empty Vec and not as an error.
        let results: *mut AnyObject = msg_send![&*request, results];
        Ok(collect_observations(results, width, height))
    })
}

/// Pixel dimensions of the captured PNG, read from its own header.
///
/// Reading order compares horizontal gaps against vertical ones, which is only
/// meaningful in pixels. Falling back to a unit square keeps normalized
/// coordinates instead: the text still comes out, only the column heuristic
/// degrades, which is the right trade against failing a capture the user
/// already made.
fn image_dimensions(path: &Path) -> (f64, f64) {
    use std::io::Read;

    let mut header = [0u8; 24];
    let read = std::fs::File::open(path).and_then(|mut file| file.read_exact(&mut header));
    match read.ok().and_then(|()| png_dimensions(&header)) {
        Some((width, height)) => (f64::from(width), f64::from(height)),
        None => {
            tracing::warn!("screen: capture is not a readable PNG; reading order may suffer");
            (1.0, 1.0)
        }
    }
}

/// SAFETY: all of the following take and return Objective-C pointers and must
/// be called inside an autorelease pool. Every object either comes back
/// autoreleased (left alone) or +1 from `alloc`/`init` (adopted by
/// [`Retained`], which releases it on drop).
unsafe fn new_text_request(opts: &OcrOptions) -> Result<Retained<AnyObject>, ScreenError> {
    let class = vision_class(c"VNRecognizeTextRequest")?;
    let request: *mut AnyObject = msg_send![class, alloc];
    let request: *mut AnyObject = msg_send![request, init];
    let request = Retained::from_raw(request).ok_or_else(|| {
        ScreenError::Recognize("VNRecognizeTextRequest could not be created".into())
    })?;

    let _: () = msg_send![&*request, setRecognitionLevel: RECOGNITION_LEVEL_ACCURATE];
    let _: () = msg_send![
        &*request,
        setUsesLanguageCorrection: Bool::new(opts.language_correction)
    ];
    if !opts.languages.is_empty() {
        let tags: *mut AnyObject = msg_send![class!(NSMutableArray), array];
        for language in &opts.languages {
            let tag = NSString::from_str(language);
            let _: () = msg_send![tags, addObject: &*tag];
        }
        let _: () = msg_send![&*request, setRecognitionLanguages: tags];
    }
    // `minimumTextHeight` and `customWords` are left at their defaults on
    // purpose: `customWords` only nudges language correction, so tying it to
    // the dictation vocabulary would couple two features for no measured gain.
    Ok(request)
}

unsafe fn new_image_handler(path: &str) -> Result<Retained<AnyObject>, ScreenError> {
    let class = vision_class(c"VNImageRequestHandler")?;
    let ns_path = NSString::from_str(path);
    let url: *mut AnyObject = msg_send![class!(NSURL), fileURLWithPath: &*ns_path];
    if url.is_null() {
        return Err(ScreenError::Recognize(format!(
            "{path} is not a usable path"
        )));
    }
    let options: *mut AnyObject = msg_send![class!(NSDictionary), dictionary];
    let handler: *mut AnyObject = msg_send![class, alloc];
    let handler: *mut AnyObject = msg_send![handler, initWithURL: url, options: options];
    Retained::from_raw(handler)
        .ok_or_else(|| ScreenError::Recognize("VNImageRequestHandler could not be created".into()))
}

unsafe fn collect_observations(
    results: *mut AnyObject,
    width: f64,
    height: f64,
) -> Vec<Observation> {
    if results.is_null() {
        return Vec::new();
    }
    let count: usize = msg_send![results, count];
    let mut observations = Vec::with_capacity(count);

    for index in 0..count {
        let observation: *mut AnyObject = msg_send![results, objectAtIndex: index];
        if observation.is_null() {
            continue;
        }
        // One candidate: the alternatives are for interactive correction UI,
        // which this feature does not have.
        let candidates: *mut AnyObject = msg_send![observation, topCandidates: 1usize];
        if candidates.is_null() {
            continue;
        }
        let candidate_count: usize = msg_send![candidates, count];
        if candidate_count == 0 {
            continue;
        }
        let candidate: *mut AnyObject = msg_send![candidates, objectAtIndex: 0usize];
        let string: *mut AnyObject = msg_send![candidate, string];
        let Some(text) = ns_string_to_rust(string) else {
            continue;
        };
        if text.trim().is_empty() {
            continue;
        }
        let confidence: f32 = msg_send![candidate, confidence];
        let bounds: NSRect = msg_send![observation, boundingBox];

        observations.push(Observation {
            text,
            bbox: NormalizedRect {
                x: bounds.origin.x as f64,
                y: bounds.origin.y as f64,
                width: bounds.size.width as f64,
                height: bounds.size.height as f64,
            }
            .to_text_box(width, height),
            confidence,
        });
    }
    observations
}

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

unsafe fn ns_error_message(error: *mut AnyObject) -> String {
    const FALLBACK: &str = "Vision reported no detail";
    if error.is_null() {
        return FALLBACK.into();
    }
    let description: *mut AnyObject = msg_send![error, localizedDescription];
    ns_string_to_rust(description).unwrap_or_else(|| FALLBACK.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_capture_tool_is_present_and_executable() {
        // The settings-page probe, run against the real path. A macOS without
        // /usr/sbin/screencapture would be news.
        MacScreenCapture::new().availability().unwrap();
    }

    #[test]
    fn a_missing_capture_tool_is_a_typed_error_not_a_panic() {
        let capture = MacScreenCapture {
            tool: PathBuf::from("/usr/sbin/definitely-not-screencapture"),
            staging: capture_parent_dir(),
        };
        assert!(matches!(
            capture.availability(),
            Err(ScreenError::Unavailable(_))
        ));
    }

    #[test]
    fn a_non_executable_capture_tool_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let tool = dir.path().join("screencapture");
        std::fs::write(&tool, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
        let capture = MacScreenCapture {
            tool,
            staging: dir.path().to_path_buf(),
        };
        assert!(matches!(
            capture.availability(),
            Err(ScreenError::Unavailable(_))
        ));
    }

    /// Exercises the whole Vision FFI path — dlopen, class lookup, alloc/init,
    /// property set, an NSError out-parameter and NSString extraction —
    /// without needing a screen or an image.
    #[test]
    fn vision_reports_the_languages_this_os_supports() {
        let languages = MacTextRecognizer::new().supported_languages().unwrap();
        assert!(!languages.is_empty());
        assert!(
            languages.iter().any(|tag| tag.starts_with("en")),
            "no English tag in {languages:?}"
        );
    }

    #[tokio::test]
    async fn recognizing_a_file_that_is_not_an_image_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("not-an-image.png");
        std::fs::write(&path, b"this is not a PNG").unwrap();

        let result = MacTextRecognizer::new()
            .recognize(&path, &OcrOptions::default())
            .await;
        assert!(matches!(result, Err(ScreenError::Recognize(_))));
    }

    #[tokio::test]
    async fn recognizing_a_missing_file_is_an_error_not_a_panic() {
        let result = MacTextRecognizer::new()
            .recognize(
                Path::new("/nonexistent/kea/region.png"),
                &OcrOptions::default(),
            )
            .await;
        assert!(matches!(result, Err(ScreenError::Recognize(_))));
    }

    #[test]
    fn language_options_reach_the_request_without_crashing() {
        // Setting recognitionLanguages is the one property whose argument is
        // an object rather than a scalar; a wrong selector spelling shows up
        // here as an unrecognized-selector abort.
        let opts = OcrOptions {
            languages: vec!["en-US".into(), "fr-FR".into()],
            language_correction: false,
        };
        autoreleasepool(|_| unsafe {
            let request = new_text_request(&opts).unwrap();
            let correction: Bool = msg_send![&*request, usesLanguageCorrection];
            assert!(!correction.is_true());
            let level: isize = msg_send![&*request, recognitionLevel];
            assert_eq!(level, RECOGNITION_LEVEL_ACCURATE);
            let tags: *mut AnyObject = msg_send![&*request, recognitionLanguages];
            let count: usize = msg_send![tags, count];
            assert_eq!(count, 2);
        });
    }
}
