//! Platform providers: OS integration behind traits (hotkeys, text I/O, audio,
//! permissions, screen capture/OCR, calendar).

pub mod audio;
pub mod calendar;
pub mod hotkeys;
#[cfg(target_os = "macos")]
pub mod macos_services;
pub mod permissions;
pub mod screen;
pub mod textio;
pub mod tts;

pub use audio::{
    accumulate_frames, chunk_pcm_by_duration, cue_pcm, mix_frames, new_audio_io, resample_linear,
    rms_level, AudioIo, AudioIoError, Cue, DictationState, MeetingState, PcmBuffer, PcmFrame,
    SystemAudioCapability,
};
pub use calendar::{
    new_calendar_io, title_for_recording, CalendarError, CalendarEvent, CalendarIo,
};
pub use hotkeys::hold::{HoldAction, HoldModifiers, HoldToTalk, DEFAULT_MIN_HOLD};
pub use hotkeys::{
    parse_accelerator, spawn_hold_to_talk, ActionId, HotkeyBinding, HotkeyError, Hotkeys,
};
pub use permissions::{new_permissions, PermError, PermKind, PermStatus, Permissions};
pub use screen::{
    new_screen_capture, new_text_recognizer, observations_to_text, reading_order, CaptureOutcome,
    CaptureSlot, CaptureVerdict, CapturedImage, NormalizedRect, Observation, OcrOptions,
    ScreenCapture, ScreenError, TextBox, TextRecognizer,
};
pub use textio::{
    new_app_context_probe, AppContext, AppContextProbe, CaptureOpts, ClipboardPlan, ReplaceMode,
    TextIo, TextIoError,
};
pub use tts::{new_system_tts, SystemTtsError, SystemTtsInference, SystemVoice};

/// Construct the active platform [`Hotkeys`] implementation for this OS.
pub fn new_hotkeys() -> Box<dyn Hotkeys> {
    #[cfg(target_os = "macos")]
    {
        Box::new(hotkeys::macos::MacHotkeys::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(hotkeys::stub::StubHotkeys::new())
    }
}

/// Construct the active platform [`TextIo`] implementation for this OS.
pub fn new_text_io() -> Box<dyn TextIo> {
    #[cfg(target_os = "macos")]
    {
        Box::new(textio::macos::MacTextIo::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(textio::stub::StubTextIo::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpal_is_linked() {
        let _ = std::any::type_name::<cpal::SampleFormat>();
    }

    #[test]
    fn platform_constructors_do_not_panic() {
        let _hotkeys = new_hotkeys();
        let _text_io = new_text_io();
        let _audio = new_audio_io();
        let _permissions = new_permissions();
        // Constructed, not exercised: capturing a region opens an interactive
        // selector and OCR needs a screen, neither of which a test has.
        let _screen = new_screen_capture();
        let _ocr = new_text_recognizer();
        let _calendar = new_calendar_io();
    }

    /// Reading a status must never take the process down, whatever the answer.
    ///
    /// `Calendar` is the one that can: it asks EventKit, and `class!` *panics*
    /// when the framework is not linked. Nothing else in a Tauri app pulls
    /// EventKit in, so without the `#[link]` in `calendar::macos` this test —
    /// and the Permissions panel — crashes instead of reporting a status.
    #[test]
    fn every_permission_status_can_be_read_without_panicking() {
        let permissions = new_permissions();
        for kind in [
            PermKind::Microphone,
            PermKind::ScreenRecording,
            PermKind::Accessibility,
            PermKind::Calendar,
        ] {
            let _status = permissions.status(kind);
        }
    }

    /// Verify the non-macOS composition path constructs without panic and that
    /// startup-path methods (register, state queries, on_action, permissions)
    /// return cleanly.  Only compiled on Windows/Linux — CI runs this.
    #[cfg(not(target_os = "macos"))]
    mod non_macos_smoke {
        use super::*;

        #[test]
        fn constructors_do_not_panic() {
            let _hotkeys = new_hotkeys();
            let _text_io = new_text_io();
            let _audio = new_audio_io();
            let _permissions = new_permissions();
            let _screen = new_screen_capture();
            let _ocr = new_text_recognizer();
            let _calendar = new_calendar_io();
        }

        #[test]
        fn on_action_returns_closed_channel() {
            let hk = new_hotkeys();
            // Multiple calls must not panic (the new stub has no expect).
            let rx1 = hk.on_action();
            let rx2 = hk.on_action();
            assert!(rx1.is_empty());
            assert!(rx2.is_empty());
        }

        #[test]
        fn register_and_unregister_dont_panic() {
            let mut hk = new_hotkeys();
            let binding = hotkeys::HotkeyBinding {
                accelerator: "Cmd+Shift+R".into(),
            };
            assert!(hk
                .register(binding.clone(), "action.rewrite".into())
                .is_err());
            assert!(hk.unregister(&binding).is_err());
        }

        #[test]
        fn audio_state_queries_dont_panic() {
            let audio = new_audio_io();
            assert_eq!(audio.state(), audio::DictationState::Idle);
            assert_eq!(audio.current_level(), 0.0);
            assert_eq!(
                audio.system_audio_capability(),
                audio::SystemAudioCapability::Unavailable
            );
            assert_eq!(audio.meeting_state(), audio::MeetingState::Idle);
        }

        #[test]
        fn permissions_status_does_not_panic() {
            let perms = new_permissions();
            assert_eq!(
                perms.status(permissions::PermKind::Microphone),
                permissions::PermStatus::Unknown
            );
        }

        #[tokio::test]
        async fn textio_stub_errors_on_use() {
            let io = new_text_io();
            assert!(io.capture_selection().await.is_err());
            assert!(io.replace("test").await.is_err());
            assert!(io.insert_at_cursor("test").await.is_err());
        }

        #[tokio::test]
        async fn audio_stub_errors_on_use() {
            let mut audio = new_audio_io();
            assert!(audio.start_mic().await.is_err());
            let buf = audio.stop_mic().await;
            assert!(buf.is_err());
            assert!(audio.start_meeting(false).await.is_err());
            let meeting_buf = audio.stop_meeting().await;
            assert!(meeting_buf.is_err());
        }

        #[tokio::test]
        async fn permissions_request_dont_panic() {
            let perms = new_permissions();
            let result = perms.request(permissions::PermKind::Microphone).await;
            assert!(result.is_ok());
            assert_eq!(result.unwrap(), permissions::PermStatus::Unknown);
        }
    }
}
