//! Apple's on-device speech recognizer.
//!
//! Sibling to `calendar/`, `screen/`, `tts/`: a trait, a cfg-selected
//! constructor, a macOS implementation and a non-macOS stub.
//!
//! # Which API this reaches, and why it is not the new one
//!
//! macOS 26 ships `SpeechAnalyzer`/`SpeechTranscriber`, which are better than
//! `SFSpeechRecognizer` in every way that matters here. They are **not
//! reachable from Rust**. Measured on this machine (macOS 26.6.2, SDK 26.5)
//! by asking the Objective-C runtime for each class after `dlopen`ing
//! Speech.framework:
//!
//! ```text
//! SFSpeechRecognizer                     FOUND
//! SFSpeechAudioBufferRecognitionRequest  FOUND
//! SFSpeechURLRecognitionRequest          FOUND
//! SpeechAnalyzer                         missing
//! SpeechTranscriber                      missing
//! _TtC6Speech15SpeechAnalyzer            missing
//! ```
//!
//! The reason is in the SDK: `Speech.swiftinterface` declares
//! `final public actor SpeechAnalyzer` and `final public class
//! SpeechTranscriber`, neither marked `@objc`. A Swift actor has no
//! Objective-C class at all — there is nothing for `objc_getClass` or
//! `msg_send!` to find, under any name. Reaching them would mean compiling a
//! Swift shim with a C ABI and linking it into the Tauri binary: a new
//! toolchain in the build, a new artifact to codesign, and a second language
//! in a workspace that has none. That is a real option, but it is a build
//! decision rather than an engine one, so this module targets the API that is
//! actually reachable and says so rather than half-building the other.
//!
//! `SFSpeechRecognizer` is not a consolation prize for the job at hand: with
//! `requiresOnDeviceRecognition` it runs locally with no download and no
//! model management, which is the entire point of the item.
//!
//! # Three things this needs from outside this crate
//!
//! 1. **`NSSpeechRecognitionUsageDescription` in Info.plist.** Apple's own
//!    header: "your app will crash when you call [`requestAuthorization`]" if
//!    the key is absent. [`SpeechRecognition::request_authorization`] is the
//!    only call that can prompt, and nothing else here touches it.
//! 2. **A TCC grant.** Like the microphone and the calendar, and read with
//!    `+authorizationStatus`, which never prompts.
//! 3. **Speech.framework loaded.** A Tauri process loads AppKit and WebKit,
//!    not Speech — verified, see the table above, where every class was
//!    missing *before* the `dlopen`. This is the third time (EventKit needed
//!    a `#[link]`, Vision a `dlopen`) and the first two were each found by a
//!    crash rather than by review.
//!
//! # Limits Apple documents, that the caller has to live with
//!
//! One minute of audio per task, and a per-device daily cap. Neither is
//! visible until it is hit, so [`SpeechError::Failed`] carries the
//! framework's own message rather than a rewritten one.

use std::path::Path;

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
pub mod stub;

/// `SFSpeechRecognizerAuthorizationStatus`, minus the distinction between
/// "denied" and "restricted" — the user can act on neither differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpeechAuth {
    /// Never asked. The only state in which requesting can prompt.
    NotDetermined,
    Granted,
    Denied,
    /// No Speech framework on this platform at all.
    Unavailable,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SpeechError {
    #[error("speech recognition is not authorized")]
    NotAuthorized,
    #[error("on-device speech recognition is not available on this system")]
    Unavailable,
    /// The recognizer ran and refused. Carries the framework's own text
    /// because the interesting cases — over a minute of audio, the daily
    /// cap, an unsupported locale — are only distinguishable by it.
    #[error("{0}")]
    Failed(String),
}

/// One timed span of recognized speech, in milliseconds from the start of the
/// file. Mirrors the engine layer's own segment type without depending on it:
/// `kea-engines` depends on `kea-platform`, never the other way round.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeechSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpeechTranscript {
    pub text: String,
    /// Empty when the recognizer reported no per-segment timing, never a
    /// single invented span covering the whole file.
    pub segments: Vec<SpeechSegment>,
}

/// What one recognition run is allowed to ask for.
#[derive(Debug, Clone, Default)]
pub struct SpeechOpts {
    /// BCP-47 tag. `None` means the recognizer's own default, which is the
    /// user's system language — the right answer for someone who has chosen
    /// nothing.
    pub locale: Option<String>,
    /// Canonical spellings to bias toward, reaching
    /// `SFSpeechRecognitionRequest.contextualStrings`.
    pub vocabulary: Vec<String>,
}

/// On-device recognition of an audio file.
///
/// A *file*, not a PCM buffer, on purpose. `SFSpeechURLRecognitionRequest`
/// takes a URL and does its own decoding; the alternative
/// (`SFSpeechAudioBufferRecognitionRequest`) means building `AVAudioPCMBuffer`s
/// in the right `AVAudioFormat` and appending them, which is more Objective-C
/// for no gain. The caller already has a WAV writer, so the seam costs one
/// temp file and removes a whole class of format mismatch.
///
/// Synchronous and blocking: recognition takes as long as it takes, and every
/// caller here is already inside `spawn_blocking`.
pub trait SpeechRecognition: Send + Sync {
    /// The current grant. Never prompts — safe to call from a settings panel.
    fn authorization(&self) -> SpeechAuth;

    /// Prompts if and only if the status is `NotDetermined`.
    ///
    /// # Panics on a misconfigured bundle
    ///
    /// Apple's framework calls `abort` when
    /// `NSSpeechRecognitionUsageDescription` is missing from Info.plist.
    /// Nothing in Rust can catch that, which is why it is documented at the
    /// one call that can trigger it.
    fn request_authorization(&self) -> SpeechAuth;

    /// Whether this system can recognize without a network round-trip.
    ///
    /// Asked separately from [`Self::transcribe_file`] so a picker can refuse
    /// to offer the engine rather than offering one that silently sends audio
    /// to Apple — which is what `requiresOnDeviceRecognition` on an
    /// unsupported system produces: an error, but only after the upload.
    fn supports_on_device(&self, locale: Option<&str>) -> bool;

    /// Recognizes `path`, on device, or fails.
    fn transcribe_file(
        &self,
        path: &Path,
        opts: &SpeechOpts,
    ) -> Result<SpeechTranscript, SpeechError>;
}

/// Construct the active platform [`SpeechRecognition`] implementation.
pub fn new_speech_recognition() -> Box<dyn SpeechRecognition> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacSpeechRecognition::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubSpeechRecognition::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reading a status must never take the process down, whatever the
    /// answer — the same rule the Calendar permission is under, and for the
    /// same reason: `class!` panics when the framework is not loaded, and
    /// Speech is not loaded in a Tauri process by default.
    #[test]
    fn reading_the_authorization_status_never_panics() {
        let speech = new_speech_recognition();
        let status = speech.authorization();
        assert!(matches!(
            status,
            SpeechAuth::NotDetermined
                | SpeechAuth::Granted
                | SpeechAuth::Denied
                | SpeechAuth::Unavailable
        ));
    }

    /// And neither must asking whether on-device recognition is possible.
    /// This constructs an `SFSpeechRecognizer`, which is the second place the
    /// framework has to already be loaded.
    #[test]
    fn asking_about_on_device_support_never_panics() {
        let speech = new_speech_recognition();
        let _ = speech.supports_on_device(None);
        let _ = speech.supports_on_device(Some("en-US"));
        // A tag no recognizer supports must be a `false`, not a panic.
        let _ = speech.supports_on_device(Some("zz-ZZ"));
    }

    /// Without a grant, transcription refuses by name rather than returning
    /// an empty transcript that reads as silence. On a machine where the
    /// grant *has* been given, the file below does not exist and the
    /// framework reports that instead — either way it is an error, and either
    /// way nothing pretends to have heard anything.
    #[test]
    fn transcribing_without_a_grant_is_an_error_not_an_empty_transcript() {
        let speech = new_speech_recognition();
        let err = speech
            .transcribe_file(Path::new("/nonexistent/audio.wav"), &SpeechOpts::default())
            .unwrap_err();
        assert!(matches!(
            err,
            SpeechError::NotAuthorized | SpeechError::Unavailable | SpeechError::Failed(_)
        ));
    }
}
