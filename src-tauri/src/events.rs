use kea_infer::DownloadProgress;
use kea_platform::audio::DeviceFallback;
use kea_platform::{DictationState, MeetingState};
use serde::Serialize;
use tauri::{AppHandle, Emitter};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct RewriteEventPayload {
    pub message: String,
}

pub fn emit_rewrite_progress(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "rewrite:progress",
        RewriteEventPayload {
            message: message.to_string(),
        },
    );
}

pub fn emit_rewrite_error(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "rewrite:error",
        RewriteEventPayload {
            message: message.to_string(),
        },
    );
}

/// The lifecycle payloads carry the wire string rather than the state enum
/// itself: the platform enums serialize as their variant names, and the
/// frontend and the overlay have always matched lowercase. The enum stays the
/// argument type at every emit site; the mapping to the wire happens once, in
/// the `*_state_wire` functions below.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DictationStatePayload {
    pub state: String,
}

/// The `dictation:state` value for each state.
pub fn dictation_state_wire(state: DictationState) -> &'static str {
    match state {
        DictationState::Idle => "idle",
        DictationState::Listening => "listening",
        DictationState::Locked => "locked",
        DictationState::Processing => "processing",
    }
}

/// The `meeting:state` value for each state.
pub fn meeting_state_wire(state: MeetingState) -> &'static str {
    match state {
        MeetingState::Idle => "idle",
        MeetingState::Recording => "recording",
        MeetingState::Processing => "processing",
    }
}

/// What the TTS feature publishes on `tts:state`. There is no platform enum
/// for it — reading aloud is not an audio-capture state — so the two values
/// the UI knows about live here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TtsState {
    Idle,
    Reading,
}

impl TtsState {
    /// The `tts:state` value for each state.
    pub fn wire(self) -> &'static str {
        match self {
            TtsState::Idle => "idle",
            TtsState::Reading => "reading",
        }
    }
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct DictationLevelPayload {
    pub level: f32,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModelDownloadProgressPayload {
    pub model_id: String,
    pub bytes_received: u64,
    pub bytes_total: u64,
}

pub fn emit_dictation_state(app: &AppHandle, state: DictationState) {
    // Driven from the emit site rather than the frontend so the floating HUD
    // tracks dictation whether or not the main window is open.
    crate::overlay::sync_visibility(app, state);
    let _ = app.emit(
        "dictation:state",
        DictationStatePayload {
            state: dictation_state_wire(state).to_string(),
        },
    );
}

pub fn emit_dictation_level(app: &AppHandle, level: f32) {
    let _ = app.emit("dictation:level", DictationLevelPayload { level });
}

/// Whether the microphone is open purely to show its level.
///
/// A separate event from `dictation:state` because a preview is not a
/// dictation run: it records nothing, and the HUD must stay hidden for it.
/// The UI needs it because the preview stops itself — on a timer, on window
/// blur, and when a recording takes the device — and the toggle has to follow.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DictationPreviewPayload {
    pub active: bool,
}

pub fn emit_dictation_preview(app: &AppHandle, active: bool) {
    let _ = app.emit("dictation:preview", DictationPreviewPayload { active });
}

/// The saved microphone was not there and capture opened the default instead.
///
/// Emitted once per stream open rather than per audio callback — see
/// `AudioIo::take_device_fallback`.
pub fn emit_device_fallback(app: &AppHandle, fallback: &DeviceFallback) {
    let _ = app.emit("dictation:device_fallback", fallback.clone());
}

pub fn emit_dictation_error(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "dictation:error",
        RewriteEventPayload {
            message: message.to_string(),
        },
    );
}

pub fn emit_model_download_progress(app: &AppHandle, progress: &DownloadProgress) {
    let _ = app.emit(
        "model:download:progress",
        ModelDownloadProgressPayload {
            model_id: progress.model_id.clone(),
            bytes_received: progress.bytes_received,
            bytes_total: progress.bytes_total,
        },
    );
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModelDownloadCompletePayload {
    pub model_id: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct ModelDownloadErrorPayload {
    pub model_id: String,
    pub message: String,
}

pub fn emit_model_download_complete(app: &AppHandle, model_id: &str) {
    let _ = app.emit(
        "model:download:complete",
        ModelDownloadCompletePayload {
            model_id: model_id.to_string(),
        },
    );
}

pub fn emit_model_download_error(app: &AppHandle, model_id: &str, message: &str) {
    let _ = app.emit(
        "model:download:error",
        ModelDownloadErrorPayload {
            model_id: model_id.to_string(),
            message: message.to_string(),
        },
    );
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MeetingStatePayload {
    pub state: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct MeetingSegmentPayload {
    pub meeting_id: String,
    pub sequence: i32,
    pub text: String,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct MeetingLevelPayload {
    pub level: f32,
}

pub fn emit_meeting_state(app: &AppHandle, state: MeetingState) {
    let _ = app.emit(
        "meeting:state",
        MeetingStatePayload {
            state: meeting_state_wire(state).to_string(),
        },
    );
}

pub fn emit_meeting_segment(app: &AppHandle, seg: &MeetingSegmentPayload) {
    let _ = app.emit("meeting:segment", seg.clone());
}

pub fn emit_meeting_level(app: &AppHandle, level: f32) {
    let _ = app.emit("meeting:level", MeetingLevelPayload { level });
}

pub fn emit_meeting_error(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "meeting:error",
        RewriteEventPayload {
            message: message.to_string(),
        },
    );
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TtsStatePayload {
    pub state: String,
}

pub fn emit_tts_state(app: &AppHandle, state: TtsState) {
    let _ = app.emit(
        "tts:state",
        TtsStatePayload {
            state: state.wire().to_string(),
        },
    );
}

pub fn emit_tts_error(app: &AppHandle, message: &str) {
    let _ = app.emit(
        "tts:error",
        RewriteEventPayload {
            message: message.to_string(),
        },
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rewrite_payload_serializes_message_field() {
        let json = serde_json::to_string(&RewriteEventPayload {
            message: "Capturing selection...".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"message":"Capturing selection..."}"#);
    }

    #[test]
    fn dictation_level_payload_serializes() {
        let json = serde_json::to_string(&DictationLevelPayload { level: 0.5 }).unwrap();
        assert_eq!(json, r#"{"level":0.5}"#);
    }

    #[test]
    fn dictation_state_payload_serializes() {
        let json = serde_json::to_string(&DictationStatePayload {
            state: "listening".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"listening"}"#);
    }

    #[test]
    fn model_download_progress_payload_serializes() {
        let json = serde_json::to_string(&ModelDownloadProgressPayload {
            model_id: "ggml-base.en".into(),
            bytes_received: 100,
            bytes_total: 1000,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"model_id":"ggml-base.en","bytes_received":100,"bytes_total":1000}"#
        );
    }

    #[test]
    fn model_download_complete_payload_serializes() {
        let json = serde_json::to_string(&ModelDownloadCompletePayload {
            model_id: "ggml-base.en".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"model_id":"ggml-base.en"}"#);
    }

    #[test]
    fn model_download_error_payload_serializes() {
        let json = serde_json::to_string(&ModelDownloadErrorPayload {
            model_id: "ggml-base.en".into(),
            message: "network error".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"model_id":"ggml-base.en","message":"network error"}"#
        );
    }

    #[test]
    fn meeting_segment_payload_serializes() {
        let json = serde_json::to_string(&MeetingSegmentPayload {
            meeting_id: "m1".into(),
            sequence: 0,
            text: "hi".into(),
            start_offset_ms: 0,
            end_offset_ms: 30_000,
        })
        .unwrap();
        assert!(json.contains(r#""text":"hi""#));
    }

    #[test]
    fn meeting_state_payload_serializes() {
        let json = serde_json::to_string(&MeetingStatePayload {
            state: "recording".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"recording"}"#);
    }

    #[test]
    fn meeting_level_payload_serializes() {
        let json = serde_json::to_string(&MeetingLevelPayload { level: 0.25 }).unwrap();
        assert_eq!(json, r#"{"level":0.25}"#);
    }

    #[test]
    fn state_wire_values_are_the_lowercase_contract() {
        assert_eq!(dictation_state_wire(DictationState::Idle), "idle");
        assert_eq!(dictation_state_wire(DictationState::Listening), "listening");
        assert_eq!(dictation_state_wire(DictationState::Locked), "locked");
        assert_eq!(
            dictation_state_wire(DictationState::Processing),
            "processing"
        );
        assert_eq!(meeting_state_wire(MeetingState::Idle), "idle");
        assert_eq!(meeting_state_wire(MeetingState::Recording), "recording");
        assert_eq!(meeting_state_wire(MeetingState::Processing), "processing");
        assert_eq!(TtsState::Idle.wire(), "idle");
        assert_eq!(TtsState::Reading.wire(), "reading");
    }

    #[test]
    fn dictation_preview_payload_serializes() {
        let json = serde_json::to_string(&DictationPreviewPayload { active: true }).unwrap();
        assert_eq!(json, r#"{"active":true}"#);
    }

    #[test]
    fn device_fallback_serializes_both_names() {
        // The UI says "your Yeti is gone, using the MacBook mic", so it needs
        // both halves on the wire.
        let json = serde_json::to_string(&DeviceFallback {
            requested: "Yeti".into(),
            using: Some("MacBook Air Microphone".into()),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"requested":"Yeti","using":"MacBook Air Microphone"}"#
        );
    }

    #[test]
    fn tts_state_payload_serializes() {
        let json = serde_json::to_string(&TtsStatePayload {
            state: "reading".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"reading"}"#);
    }
}
