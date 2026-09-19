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

/// One live hypothesis on its way to the HUD.
///
/// Its own event rather than fields on `dictation:state`, for four reasons
/// that all point the same way. `emit_dictation_state` is not a pure emit — it
/// calls `crate::overlay::sync_visibility`, which repositions and re-shows the
/// overlay window, so carrying partials there would re-pin the window ten
/// times a second for a whole run. The cardinalities differ by two orders of
/// magnitude (three state emits per run against ten partials a second).
/// `DictationStatePayload` derives `Eq` and is pinned by a serialization test,
/// and every consumer of `dictation:state` — including the non-overlay UI —
/// would re-render on text it does not display.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DictationPartialPayload {
    /// Monotonic within a run, incremented per *emitted* partial, so a drop is
    /// invisible to the frontend and a late arrival is discardable by
    /// comparison rather than by timestamp. The HUD keeps only the highest it
    /// has seen.
    pub seq: u64,
    /// Display text: the tail of the hypothesis, already truncated.
    pub text: String,
    /// Leading **scalar values** — not bytes — the engine considers committed;
    /// `None` when it does not report stability.
    ///
    /// Rust byte offsets are not usable as JavaScript string indices: a
    /// partial containing CJK or an emoji would be split mid-surrogate and
    /// render a replacement character. Counted here with `chars().count()`,
    /// sliced in the HUD with `Array.from`. Invisible in English-only testing,
    /// which is what makes it worth pinning in the payload contract.
    pub stable_chars: Option<usize>,
    /// The last partial of the run — the offline transcript, which is what was
    /// actually typed.
    pub is_final: bool,
}

pub fn emit_dictation_partial(app: &AppHandle, partial: &DictationPartialPayload) {
    let _ = app.emit("dictation:partial", partial.clone());
}

/// The longest tail, in scalar values, a partial is allowed to carry.
///
/// The HUD shows one clipped line and nothing earlier is renderable. Without
/// this, a multi-minute locked dictation ships a growing multi-kilobyte string
/// across IPC ten times a second. The truncation is display-only: this text is
/// never an input to insertion.
pub const PARTIAL_TAIL_BUDGET: usize = 240;

/// The shortest gap between two emitted partials.
///
/// Half the level poll's 50 ms. Below roughly 120 ms a self-revising line
/// reads as strobing rather than as typing, so a faster rate buys nothing a
/// user can perceive and costs an IPC round trip per decode chunk.
pub const PARTIAL_MIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Rate-limits partials on the Rust side.
///
/// Throttling in React would be the wrong seam: by then the events have
/// already crossed the IPC boundary and been deserialized, which is where the
/// cost is. Pure — no `AppHandle`, no clock of its own — so the rules are
/// unit-testable.
#[derive(Debug, Default)]
pub struct PartialThrottle {
    seq: u64,
    last_text: String,
    last_emit: Option<std::time::Instant>,
}

impl PartialThrottle {
    pub fn new() -> Self {
        Self::default()
    }

    /// The payload to emit for `text`, or `None` to say nothing.
    ///
    /// `is_final` always emits and bypasses the interval: otherwise the last
    /// words spoken can land inside a throttle window and never be shown,
    /// which is the bug a naive throttle always has.
    pub fn offer(
        &mut self,
        text: &str,
        is_final: bool,
        now: std::time::Instant,
    ) -> Option<DictationPartialPayload> {
        let text = tail_of(text, PARTIAL_TAIL_BUDGET);

        if !is_final {
            // An unchanged hypothesis — what a greedy decoder emits most of
            // the time — is dropped without consuming the interval budget, so
            // the next genuinely new one goes out immediately rather than
            // waiting out a tick spent on a duplicate.
            if text == self.last_text {
                return None;
            }
            // Last-wins, depth 1: a superseded hypothesis is discarded, never
            // queued. A queue would make the HUD lag real time under load,
            // which is the one thing this feature exists to avoid.
            if let Some(last) = self.last_emit {
                if now.duration_since(last) < PARTIAL_MIN_INTERVAL {
                    return None;
                }
            }
        }

        self.seq += 1;
        self.last_text = text.clone();
        self.last_emit = Some(now);
        Some(DictationPartialPayload {
            seq: self.seq,
            stable_chars: is_final.then(|| text.chars().count()),
            text,
            is_final,
        })
    }
}

/// The last `budget` scalar values of `text`, cut on a scalar boundary.
fn tail_of(text: &str, budget: usize) -> String {
    let count = text.chars().count();
    if count <= budget {
        return text.to_string();
    }
    text.chars().skip(count - budget).collect()
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

/// Progress for one file-transcription job.
///
/// Keyed by `job_id`, not by path — the same reason `model:download:*` keys
/// by `model_id`: two files dropped at once would otherwise interleave into
/// one progress bar. Audio time, not wall time, so the bar is honest about
/// how much of the recording is done rather than how fast the machine is.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TranscribeFileProgressPayload {
    pub job_id: String,
    pub audio_ms_done: u64,
    pub audio_ms_total: u64,
    pub chunk_index: usize,
    pub chunk_count: usize,
}

/// One cue, already rebased onto the source timeline.
///
/// Mirrors [`MeetingSegmentPayload`] so the page can stream text as it lands
/// instead of waiting for the whole file — which for a 40-minute recording is
/// the difference between a usable feature and a spinner.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TranscribeFileSegmentPayload {
    pub job_id: String,
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TranscribeFileCompletePayload {
    pub job_id: String,
    pub transcript_id: String,
    /// True when the user stopped it. The partial transcript is kept and is
    /// still exportable, so this is not an error.
    pub cancelled: bool,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TranscribeFileErrorPayload {
    pub job_id: String,
    pub message: String,
}

pub fn emit_transcribe_file_progress(app: &AppHandle, payload: &TranscribeFileProgressPayload) {
    let _ = app.emit("transcribe:file:progress", payload.clone());
}

pub fn emit_transcribe_file_segment(app: &AppHandle, payload: &TranscribeFileSegmentPayload) {
    let _ = app.emit("transcribe:file:segment", payload.clone());
}

pub fn emit_transcribe_file_complete(
    app: &AppHandle,
    job_id: &str,
    transcript_id: &str,
    cancelled: bool,
) {
    let _ = app.emit(
        "transcribe:file:complete",
        TranscribeFileCompletePayload {
            job_id: job_id.to_string(),
            transcript_id: transcript_id.to_string(),
            cancelled,
        },
    );
}

pub fn emit_transcribe_file_error(app: &AppHandle, job_id: &str, message: &str) {
    let _ = app.emit(
        "transcribe:file:error",
        TranscribeFileErrorPayload {
            job_id: job_id.to_string(),
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
    use std::time::Duration;

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

    /// Pins the field names the frontend types against.
    #[test]
    fn dictation_partial_payload_serializes() {
        let json = serde_json::to_string(&DictationPartialPayload {
            seq: 7,
            text: "hello wurld".into(),
            stable_chars: Some(5),
            is_final: false,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"seq":7,"text":"hello wurld","stable_chars":5,"is_final":false}"#
        );
    }

    #[test]
    fn the_first_hypothesis_is_emitted_immediately() {
        let mut throttle = PartialThrottle::new();
        let now = std::time::Instant::now();
        let payload = throttle.offer("hello", false, now).expect("emitted");
        assert_eq!(payload.seq, 1);
        assert_eq!(payload.text, "hello");
        assert_eq!(payload.stable_chars, None);
        assert!(!payload.is_final);
    }

    /// Two arrivals inside one window collapse to the later text, not to the
    /// earlier one and not to both.
    #[test]
    fn arrivals_inside_the_interval_coalesce_to_the_latest() {
        let mut throttle = PartialThrottle::new();
        let start = std::time::Instant::now();
        assert!(throttle.offer("hello", false, start).is_some());
        assert!(throttle
            .offer("hello there", false, start + Duration::from_millis(10))
            .is_none());
        let payload = throttle
            .offer(
                "hello there world",
                false,
                start + Duration::from_millis(150),
            )
            .expect("emitted after the interval");
        assert_eq!(payload.text, "hello there world");
        assert_eq!(payload.seq, 2, "seq counts emitted partials, not offers");
    }

    /// A duplicate must not spend the interval: the next genuinely new
    /// hypothesis would otherwise wait out a tick that showed nothing.
    #[test]
    fn an_identical_repeat_is_dropped_without_spending_the_interval() {
        let mut throttle = PartialThrottle::new();
        let start = std::time::Instant::now();
        assert!(throttle.offer("hello", false, start).is_some());
        assert!(throttle
            .offer("hello", false, start + Duration::from_millis(300))
            .is_none());
        // Still 300ms since the last *emit*, so this goes out at once.
        assert!(throttle
            .offer("hello world", false, start + Duration::from_millis(301))
            .is_some());
    }

    /// The bug a naive throttle always has: the last words spoken land inside
    /// a window and are never shown.
    #[test]
    fn the_final_partial_always_goes_out() {
        let mut throttle = PartialThrottle::new();
        let start = std::time::Instant::now();
        assert!(throttle.offer("hello", false, start).is_some());
        let payload = throttle
            .offer("hello world", true, start + Duration::from_millis(1))
            .expect("a final is never withheld");
        assert!(payload.is_final);
        assert_eq!(payload.stable_chars, Some("hello world".chars().count()));

        // Even a final identical to what is already on screen: it is what says
        // the text stopped moving.
        let same = throttle
            .offer("hello world", true, start + Duration::from_millis(2))
            .expect("emitted");
        assert!(same.is_final);
    }

    /// Truncation is by scalar value, so a multi-byte character is never cut
    /// in half — and `stable_chars` counts the same units the HUD slices by.
    #[test]
    fn a_long_partial_is_truncated_to_its_tail_on_a_scalar_boundary() {
        let mut throttle = PartialThrottle::new();
        let text: String = std::iter::repeat_n('あ', PARTIAL_TAIL_BUDGET + 40)
            .chain("end".chars())
            .collect();
        let payload = throttle
            .offer(&text, true, std::time::Instant::now())
            .expect("emitted");
        assert_eq!(payload.text.chars().count(), PARTIAL_TAIL_BUDGET);
        assert!(payload.text.ends_with("end"));
        assert_eq!(payload.stable_chars, Some(PARTIAL_TAIL_BUDGET));
        // The tail, not the head.
        assert!(!payload.text.starts_with(&text[..3]) || text.starts_with('あ'));
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
    fn transcribe_file_progress_payload_serializes() {
        let json = serde_json::to_string(&TranscribeFileProgressPayload {
            job_id: "job-1".into(),
            audio_ms_done: 30_000,
            audio_ms_total: 95_000,
            chunk_index: 0,
            chunk_count: 3,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"job_id":"job-1","audio_ms_done":30000,"audio_ms_total":95000,"chunk_index":0,"chunk_count":3}"#
        );
    }

    #[test]
    fn transcribe_file_segment_payload_serializes() {
        let json = serde_json::to_string(&TranscribeFileSegmentPayload {
            job_id: "job-1".into(),
            start_ms: 30_200,
            end_ms: 32_000,
            text: "hello".into(),
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"job_id":"job-1","start_ms":30200,"end_ms":32000,"text":"hello"}"#
        );
    }

    /// A cancel completes the job rather than failing it: the partial
    /// transcript is intact and exportable, so the UI must be able to tell
    /// the two apart from the payload alone.
    #[test]
    fn transcribe_file_complete_payload_carries_the_cancel_flag() {
        let json = serde_json::to_string(&TranscribeFileCompletePayload {
            job_id: "job-1".into(),
            transcript_id: "t-1".into(),
            cancelled: true,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"job_id":"job-1","transcript_id":"t-1","cancelled":true}"#
        );
    }

    #[test]
    fn transcribe_file_error_payload_serializes() {
        let json = serde_json::to_string(&TranscribeFileErrorPayload {
            job_id: "job-1".into(),
            message: "no audio track".into(),
        })
        .unwrap();
        assert_eq!(json, r#"{"job_id":"job-1","message":"no audio track"}"#);
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
