use kea_infer::DownloadProgress;
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

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DictationStatePayload {
    pub state: String,
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

pub fn emit_dictation_state(app: &AppHandle, state: &str) {
    let _ = app.emit(
        "dictation:state",
        DictationStatePayload {
            state: state.to_string(),
        },
    );
}

pub fn emit_dictation_level(app: &AppHandle, level: f32) {
    let _ = app.emit("dictation:level", DictationLevelPayload { level });
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

pub fn emit_meeting_state(app: &AppHandle, state: &str) {
    let _ = app.emit(
        "meeting:state",
        MeetingStatePayload {
            state: state.to_string(),
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

pub fn emit_tts_state(app: &AppHandle, state: &str) {
    let _ = app.emit(
        "tts:state",
        TtsStatePayload {
            state: state.to_string(),
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
    fn tts_state_payload_serializes() {
        let json = serde_json::to_string(&TtsStatePayload {
            state: "reading".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"state":"reading"}"#);
    }
}
