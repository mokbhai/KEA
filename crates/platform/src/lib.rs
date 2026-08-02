//! Platform providers: OS integration behind traits (hotkeys, text I/O, audio, permissions).

pub mod audio;
pub mod hotkeys;
#[cfg(target_os = "macos")]
pub mod macos_services;
pub mod permissions;
pub mod textio;

pub use audio::{
    accumulate_frames, chunk_pcm_by_duration, cue_pcm, mix_frames, resample_linear, rms_level,
    AudioIo, AudioIoError, Cue, DictationState, MeetingState, PcmBuffer, PcmFrame,
    SystemAudioCapability,
};
pub use hotkeys::{parse_accelerator, ActionId, HotkeyBinding, HotkeyError, Hotkeys};
pub use permissions::{new_permissions, PermError, PermKind, PermStatus, Permissions};
pub use textio::{ClipboardPlan, ReplaceMode, TextIo, TextIoError};

/// Construct the active platform [`Hotkeys`] implementation for this OS.
pub fn new_hotkeys() -> Box<dyn Hotkeys> {
    platform_hotkeys()
}

/// Construct the active platform [`TextIo`] implementation for this OS.
pub fn new_text_io() -> Box<dyn TextIo> {
    platform_text_io()
}

/// Construct the active platform [`AudioIo`] implementation for this OS.
pub fn new_audio_io() -> Box<dyn AudioIo> {
    platform_audio_io()
}

/// Returns the OS-selected [`Hotkeys`] backend.
pub fn platform_hotkeys() -> Box<dyn Hotkeys> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(hotkeys::macos::MacHotkeys::new());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(hotkeys::stub::StubHotkeys::new())
    }
}

/// Returns the OS-selected [`TextIo`] backend.
pub fn platform_text_io() -> Box<dyn TextIo> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(textio::macos::MacTextIo::new());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(textio::stub::StubTextIo::new())
    }
}

/// macOS [`Hotkeys`] constructor (explicit; same as [`platform_hotkeys`] on macOS).
#[cfg(target_os = "macos")]
pub fn macos_hotkeys() -> Box<dyn Hotkeys> {
    Box::new(hotkeys::macos::MacHotkeys::new())
}

/// macOS [`TextIo`] constructor (explicit; same as [`platform_text_io`] on macOS).
#[cfg(target_os = "macos")]
pub fn macos_textio() -> Box<dyn TextIo> {
    Box::new(textio::macos::MacTextIo::new())
}

/// Returns the OS-selected [`AudioIo`] backend.
pub fn platform_audio_io() -> Box<dyn AudioIo> {
    #[cfg(target_os = "macos")]
    {
        return Box::new(audio::macos::MacAudioIo::new());
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(audio::stub::StubAudioIo::new())
    }
}

/// macOS [`AudioIo`] constructor (explicit; same as [`platform_audio_io`] on macOS).
#[cfg(target_os = "macos")]
pub fn macos_audio() -> Box<dyn AudioIo> {
    Box::new(audio::macos::MacAudioIo::new())
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
            assert!(hk.register(binding.clone(), "action.rewrite".into()).is_err());
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
