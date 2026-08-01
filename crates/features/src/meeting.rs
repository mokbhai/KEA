use kea_core::meetings::{
    MEETING_NOTES_PROMPT_VERSION, build_meeting_notes_request, build_meeting_title_request,
    format_transcript_for_synthesis, parse_meeting_notes_json, sanitize_meeting_title,
    MeetingSettings,
};
use kea_core::resolve::{Resolution, SlotResolver};
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::meetings::{Meeting, MeetingDetail, MeetingNotes, MeetingRepo, NewMeeting, NewSegment};
use kea_engines::EngineRegistry;
use kea_engines::traits::{AudioPcm, SttOpts, Transcript};
use kea_platform::audio::util::resample_linear;
use kea_platform::{AudioIo, PcmFrame, SystemAudioCapability};

use crate::feature::{CapKind, CapSlot, Command, Feature};

const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;
const MIN_SEGMENT_SECS: u32 = 1;

pub struct MeetingFeature;

impl Feature for MeetingFeature {
    fn id(&self) -> &str {
        "meetings"
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![
            CapSlot {
                name: "stt",
                kind: CapKind::Stt,
            },
            CapSlot {
                name: "llm",
                kind: CapKind::Llm,
            },
        ]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "toggle_meeting".into(),
            title: "Start / Stop Meeting".into(),
            default_accelerator: Some(default_meeting_accelerator().into()),
        }]
    }
}

fn default_meeting_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+M"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+M"
    }
}

fn pcm_to_audio(pcm: PcmFrame) -> AudioPcm {
    let frame = if pcm.sample_rate_hz != WHISPER_SAMPLE_RATE_HZ {
        resample_linear(&pcm, WHISPER_SAMPLE_RATE_HZ)
    } else {
        pcm
    };
    AudioPcm {
        samples: frame.samples,
        sample_rate_hz: frame.sample_rate_hz,
    }
}

fn pcm_duration_ms(frame: &PcmFrame) -> i64 {
    if frame.sample_rate_hz == 0 {
        return 0;
    }
    (frame.samples.len() as i64 * 1000) / frame.sample_rate_hz as i64
}

fn has_min_audio(frame: &PcmFrame) -> bool {
    frame.sample_rate_hz > 0
        && frame.samples.len() >= (frame.sample_rate_hz as usize * MIN_SEGMENT_SECS as usize)
}

fn capture_mode(cap: SystemAudioCapability, prefer_system_audio: bool) -> &'static str {
    if !prefer_system_audio {
        return "mic_only";
    }
    match cap {
        SystemAudioCapability::ScreenCaptureKit | SystemAudioCapability::LoopbackDevice => {
            "mic_and_system"
        }
        _ => "mic_only",
    }
}

fn new_meeting_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("meeting-{millis}")
}

async fn resolve_stt_binding(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
) -> Result<Binding, String> {
    let resolver = SlotResolver::new(engines, bindings);
    match resolver.resolve_stt("meetings", "stt").await {
        Ok(Resolution::Bound(b)) => Ok(b),
        Ok(Resolution::NeedsChoice(_)) => {
            Err("multiple stt engines available; bind the meetings stt slot".into())
        }
        Ok(Resolution::Unresolvable) => Err("no stt engine available".into()),
        Err(e) => Err(e.to_string()),
    }
}

async fn resolve_llm_binding(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
) -> Result<Binding, String> {
    let resolver = SlotResolver::new(engines, bindings);
    match resolver.resolve_llm("meetings", "llm").await {
        Ok(Resolution::Bound(b)) => Ok(b),
        Ok(Resolution::NeedsChoice(_)) => {
            Err("multiple llm engines available; bind the meetings llm slot".into())
        }
        Ok(Resolution::Unresolvable) => Err("no llm engine available".into()),
        Err(e) => Err(e.to_string()),
    }
}

/// Thin single-segment STT call (testable with a fake engine).
pub async fn transcribe_pcm_segment(
    engine: &dyn kea_engines::traits::SttEngine,
    pcm: &PcmFrame,
    opts: SttOpts,
) -> Result<Transcript, kea_engines::traits::EngineError> {
    engine
        .transcribe(pcm_to_audio(pcm.clone()), opts)
        .await
}

pub async fn transcribe_meeting_segment(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    audio: &PcmFrame,
) -> Result<String, String> {
    let binding = resolve_stt_binding(engines, bindings).await?;
    let engine_id = &binding.engine_id;

    let engine = engines
        .stt(engine_id)
        .ok_or_else(|| format!("no stt engine '{engine_id}'"))?;

    let stt_opts = SttOpts {
        model: binding.model.clone(),
        language: None,
        provider_ref: binding.provider_ref.clone(),
    };

    let transcript = transcribe_pcm_segment(engine.as_ref(), audio, stt_opts)
        .await
        .map_err(|e| e.to_string())?;

    Ok(transcript.text)
}

pub async fn synthesize_meeting_notes(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meeting: &Meeting,
    segments: &[kea_core::MeetingSegment],
) -> Result<MeetingNotes, String> {
    let binding = resolve_llm_binding(engines, bindings).await?;
    let engine_id = &binding.engine_id;

    let engine = engines
        .llm(engine_id)
        .ok_or_else(|| format!("no llm engine '{engine_id}'"))?;

    let transcript = format_transcript_for_synthesis(segments);
    let mut req = build_meeting_notes_request(&meeting.title, &meeting.started_at, &transcript);
    req.model = binding.model.clone();
    let resp = engine.complete(req).await.map_err(|e| e.to_string())?;
    let parsed = parse_meeting_notes_json(&resp.text).map_err(|e| e.to_string())?;

    Ok(MeetingNotes {
        meeting_id: meeting.id.clone(),
        summary: parsed.summary,
        decisions: parsed.decisions,
        action_items: parsed.action_items,
        follow_ups: parsed.follow_ups,
        open_questions: parsed.open_questions,
        prompt_version: MEETING_NOTES_PROMPT_VERSION.to_string(),
        engine_id: Some(binding.engine_id.clone()),
        model: binding.model.clone(),
    })
}

pub async fn synthesize_meeting_title(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    summary: &str,
) -> Result<String, String> {
    let binding = resolve_llm_binding(engines, bindings).await?;
    let engine_id = &binding.engine_id;

    let engine = engines
        .llm(engine_id)
        .ok_or_else(|| format!("no llm engine '{engine_id}'"))?;

    let mut req = build_meeting_title_request(summary);
    req.model = binding.model.clone();
    let resp = engine.complete(req).await.map_err(|e| e.to_string())?;
    Ok(sanitize_meeting_title(&resp.text))
}

pub struct MeetingRunContext<'a> {
    pub engines: &'a EngineRegistry,
    pub bindings: &'a BindingRepo,
    pub actions: &'a ActionRepo,
    pub meetings: &'a MeetingRepo,
    pub audio: &'a mut dyn AudioIo,
    pub settings: &'a MeetingSettings,
}

pub struct ActiveMeeting {
    pub meeting_id: String,
    pub action_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingSegmentEvent {
    pub meeting_id: String,
    pub sequence: i32,
    pub text: String,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
}

async fn append_transcribed_segment(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meetings: &MeetingRepo,
    meeting_id: &str,
    pcm: PcmFrame,
    sequence: i32,
    start_offset_ms: i64,
) -> Result<MeetingSegmentEvent, String> {
    let duration_ms = pcm_duration_ms(&pcm);
    let end_offset_ms = start_offset_ms + duration_ms;

    let text = transcribe_meeting_segment(engines, bindings, &pcm).await?;

    meetings
        .append_segment(
            meeting_id,
            &NewSegment {
                sequence,
                start_offset_ms,
                end_offset_ms,
                text: text.clone(),
            },
        )
        .await
        .map_err(|e| e.to_string())?;

    Ok(MeetingSegmentEvent {
        meeting_id: meeting_id.to_string(),
        sequence,
        text,
        start_offset_ms,
        end_offset_ms,
    })
}

pub async fn run_meeting_start(ctx: &mut MeetingRunContext<'_>) -> Result<ActiveMeeting, String> {
    let stt_binding = resolve_stt_binding(ctx.engines, ctx.bindings).await?;
    let llm_binding = resolve_llm_binding(ctx.engines, ctx.bindings).await?;

    let capture_mode = capture_mode(ctx.audio.system_audio_capability(), ctx.settings.prefer_system_audio);
    let meeting_id = new_meeting_id();

    // Acquire the capture gate FIRST. The audio layer admits exactly one
    // recorder, so a losing concurrent start (button + hotkey, or a double
    // invoke) fails here and creates no DB rows — no phantom "error" meeting.
    ctx.audio
        .start_meeting(ctx.settings.prefer_system_audio)
        .await
        .map_err(|e| e.to_string())?;

    // Capture is now live; on any row-creation failure we must release it.
    if let Err(e) = ctx
        .meetings
        .create(&NewMeeting {
            id: meeting_id.clone(),
            title: "Untitled Meeting".into(),
            capture_mode: capture_mode.into(),
            stt_engine_id: Some(stt_binding.engine_id.clone()),
            llm_engine_id: Some(llm_binding.engine_id),
        })
        .await
    {
        let _ = ctx.audio.stop_meeting().await;
        return Err(e.to_string());
    }

    let action_id = match ctx
        .actions
        .record(NewAction {
            feature_id: "meetings".into(),
            command: "toggle_meeting".into(),
            engine_id: stt_binding.engine_id,
            model: stt_binding.model,
            provider_ref: stt_binding.provider_ref,
        })
        .await
    {
        Ok(id) => id,
        Err(e) => {
            let err = e.to_string();
            let _ = ctx.audio.stop_meeting().await;
            if let Err(inner) = ctx.meetings.complete(&meeting_id, "error", Some(&err)).await {
                tracing::warn!(
                    error = %inner,
                    meeting_id = %meeting_id,
                    "meeting: failed to mark meeting as error after action-record failure"
                );
            }
            return Err(err);
        }
    };

    Ok(ActiveMeeting {
        meeting_id,
        action_id,
    })
}

pub async fn run_meeting_poll_segment(
    ctx: &mut MeetingRunContext<'_>,
    meeting_id: &str,
    sequence: &mut i32,
    elapsed_ms: &mut i64,
) -> Result<Option<MeetingSegmentEvent>, String> {
    let pcm = ctx
        .audio
        .drain_meeting_buffer()
        .await
        .map_err(|e| e.to_string())?;

    if !has_min_audio(&pcm) {
        return Ok(None);
    }

    let event = append_transcribed_segment(
        ctx.engines,
        ctx.bindings,
        ctx.meetings,
        meeting_id,
        pcm,
        *sequence,
        *elapsed_ms,
    )
    .await?;
    *sequence += 1;
    *elapsed_ms = event.end_offset_ms;
    Ok(Some(event))
}

/// Audio phase of stopping a meeting: drain the tail buffer and release the
/// capture stream. Kept separate from [`run_meeting_stop`] so the caller can
/// hold the shared audio lock for only this brief step — the STT and LLM
/// synthesis in `run_meeting_stop` (tens of seconds) must not block dictation
/// or a new meeting from acquiring the audio lock. Always releases capture,
/// even when the drain fails.
pub async fn drain_and_stop_meeting(audio: &mut dyn AudioIo) -> Result<PcmFrame, String> {
    let drain_result = audio.drain_meeting_buffer().await.map_err(|e| e.to_string());
    let _ = audio.stop_meeting().await.map_err(|e| e.to_string());
    drain_result
}

/// Post-capture phase of stopping a meeting: transcribe the final tail
/// segment, synthesize notes/title, and finalize the meeting + action rows.
/// Takes no audio handle — the caller has already drained and released
/// capture via [`drain_and_stop_meeting`] and passes the drained result in.
pub async fn run_meeting_stop(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    meetings: &MeetingRepo,
    session: &ActiveMeeting,
    drain_result: Result<PcmFrame, String>,
) -> Result<MeetingDetail, String> {
    let meeting_id = &session.meeting_id;

    let existing = meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))?;

    let sequence = existing.segments.len() as i32;
    let elapsed_ms = existing
        .segments
        .last()
        .map(|s| s.end_offset_ms)
        .unwrap_or(0);

    let final_pcm = match drain_result {
        Ok(pcm) => pcm,
        Err(e) => {
            if let Err(inner) = meetings
                
                .complete(meeting_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    meeting_id = %meeting_id,
                    "meeting: failed to mark meeting as error in DB"
                );
            }
            if let Err(inner) = actions
                
                .finish(session.action_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %session.action_id,
                    "meeting: failed to finish action as error in DB"
                );
            }
            return Err(e);
        }
    };

    if has_min_audio(&final_pcm) {
        if let Err(e) = append_transcribed_segment(
            engines, bindings, meetings, meeting_id, final_pcm, sequence, elapsed_ms,
        )
        .await
        {
            if let Err(inner) = meetings
                
                .complete(meeting_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    meeting_id = %meeting_id,
                    "meeting: failed to mark meeting as error in DB"
                );
            }
            if let Err(inner) = actions
                
                .finish(session.action_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %session.action_id,
                    "meeting: failed to finish action as error in DB"
                );
            }
            return Err(e);
        }
    }

    let partial = meetings
        
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))?;

    let meeting = partial.meeting;
    let segments = partial.segments;

    let notes = match synthesize_meeting_notes(engines, bindings, &meeting, &segments).await
    {
        Ok(notes) => notes,
        Err(e) => {
            if let Err(inner) = meetings
                
                .complete(meeting_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    meeting_id = %meeting_id,
                    "meeting: failed to mark meeting as error in DB"
                );
            }
            if let Err(inner) = actions
                
                .finish(session.action_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %session.action_id,
                    "meeting: failed to finish action as error in DB"
                );
            }
            return Err(e);
        }
    };

    if let Err(e) = meetings.upsert_notes(&notes).await {
        let msg = e.to_string();
        if let Err(inner) = meetings
            
            .complete(meeting_id, "error", Some(&msg))
            .await
        {
            tracing::warn!(
                error = %inner,
                meeting_id = %meeting_id,
                "meeting: failed to mark meeting as error in DB"
            );
        }
        if let Err(inner) = actions
            
            .finish(session.action_id, "error", Some(&msg))
            .await
        {
            tracing::warn!(
                error = %inner,
                action_id = %session.action_id,
                "meeting: failed to finish action as error in DB"
            );
        }
        return Err(msg);
    }

    let title = match synthesize_meeting_title(engines, bindings, &notes.summary).await {
        Ok(title) => title,
        Err(e) => {
            if let Err(inner) = meetings
                
                .complete(meeting_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    meeting_id = %meeting_id,
                    "meeting: failed to mark meeting as error in DB"
                );
            }
            if let Err(inner) = actions
                
                .finish(session.action_id, "error", Some(&e))
                .await
            {
                tracing::warn!(
                    error = %inner,
                    action_id = %session.action_id,
                    "meeting: failed to finish action as error in DB"
                );
            }
            return Err(e);
        }
    };

    if let Err(e) = meetings.set_title(meeting_id, &title).await {
        let msg = e.to_string();
        if let Err(inner) = meetings
            
            .complete(meeting_id, "error", Some(&msg))
            .await
        {
            tracing::warn!(
                error = %inner,
                meeting_id = %meeting_id,
                "meeting: failed to mark meeting as error in DB"
            );
        }
        if let Err(inner) = actions
            
            .finish(session.action_id, "error", Some(&msg))
            .await
        {
            tracing::warn!(
                error = %inner,
                action_id = %session.action_id,
                "meeting: failed to finish action as error in DB"
            );
        }
        return Err(msg);
    }

    meetings
        .complete(meeting_id, "completed", None)
        .await
        .map_err(|e| e.to_string())?;

    actions
        .finish(session.action_id, "ok", None)
        .await
        .map_err(|e| e.to_string())?;

    meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::bindings::Binding;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse, SttEngine};
    use kea_platform::{AudioIoError, DictationState, MeetingState};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    struct FakeStt {
        text: String,
    }

    #[async_trait]
    impl SttEngine for FakeStt {
        fn id(&self) -> &str {
            "fake-stt"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["fake".into()],
            }
        }

        async fn transcribe(
            &self,
            _audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            Ok(Transcript {
                text: self.text.clone(),
            })
        }
    }

    struct CountingFakeStt {
        texts: Vec<String>,
        call: AtomicUsize,
    }

    #[async_trait]
    impl SttEngine for CountingFakeStt {
        fn id(&self) -> &str {
            "fake-stt"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["fake".into()],
            }
        }

        async fn transcribe(
            &self,
            _audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            let idx = self.call.fetch_add(1, Ordering::SeqCst);
            let text = self
                .texts
                .get(idx)
                .cloned()
                .unwrap_or_else(|| format!("segment-{idx}"));
            Ok(Transcript { text })
        }
    }

    struct FakeLlm;

    #[async_trait]
    impl LlmEngine for FakeLlm {
        fn id(&self) -> &str {
            "fake-llm"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
            if req.prompt.contains("<summary>") {
                return Ok(LlmResponse {
                    text: "Sprint Planning".into(),
                });
            }
            Ok(LlmResponse {
                text: r#"{"summary":"kickoff summary","decisions":"","action_items":"follow up","follow_ups":"","open_questions":""}"#
                    .into(),
            })
        }
    }

    struct FakeMeetingAudioIo {
        dictation_state: DictationState,
        meeting_state: MeetingState,
        capability: SystemAudioCapability,
        buffered: PcmFrame,
        pending_drains: Mutex<Vec<PcmFrame>>,
    }

    impl Default for FakeMeetingAudioIo {
        fn default() -> Self {
            Self {
                dictation_state: DictationState::Idle,
                meeting_state: MeetingState::Idle,
                capability: SystemAudioCapability::MicOnly,
                buffered: PcmFrame {
                    samples: vec![],
                    sample_rate_hz: 16_000,
                },
                pending_drains: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl AudioIo for FakeMeetingAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.dictation_state = DictationState::Listening;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.dictation_state = DictationState::Idle;
            Ok(self.buffered.clone())
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            self.dictation_state
        }

        fn system_audio_capability(&self) -> SystemAudioCapability {
            self.capability
        }

        fn meeting_state(&self) -> MeetingState {
            self.meeting_state
        }

        async fn start_meeting(
            &mut self,
            _prefer_system_audio: bool,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            self.meeting_state = MeetingState::Recording;
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_meeting(&mut self) -> Result<PcmFrame, AudioIoError> {
            self.meeting_state = MeetingState::Idle;
            Ok(PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            })
        }

        async fn drain_meeting_buffer(&mut self) -> Result<PcmFrame, AudioIoError> {
            Ok(self
                .pending_drains
                .lock()
                .unwrap()
                .pop()
                .unwrap_or(PcmFrame {
                    samples: vec![],
                    sample_rate_hz: 16_000,
                }))
        }
    }

    async fn test_repos() -> (BindingRepo, ActionRepo, MeetingRepo) {
        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        (
            BindingRepo::new(config_pool),
            ActionRepo::new(data_pool.clone()),
            MeetingRepo::new(data_pool),
        )
    }

    fn one_second_pcm() -> PcmFrame {
        PcmFrame {
            samples: vec![0.0; 16_000],
            sample_rate_hz: 16_000,
        }
    }

    #[test]
    fn meetings_declares_stt_and_llm_slots() {
        let f = MeetingFeature;
        assert_eq!(f.id(), "meetings");
        assert_eq!(f.required_caps().len(), 2);
        assert_eq!(f.required_caps()[0].name, "stt");
        assert_eq!(f.required_caps()[1].name, "llm");
        assert_eq!(f.commands()[0].id, "toggle_meeting");
    }

    #[test]
    fn capture_mode_mic_only_when_user_disables_system_audio() {
        // prefer_system_audio=false → mic_only regardless of capability
        assert_eq!(
            capture_mode(SystemAudioCapability::ScreenCaptureKit, false),
            "mic_only"
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::LoopbackDevice, false),
            "mic_only"
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::MicOnly, false),
            "mic_only"
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::Unavailable, false),
            "mic_only"
        );
    }

    #[test]
    fn capture_mode_respects_capability_when_user_prefers_system_audio() {
        assert_eq!(
            capture_mode(SystemAudioCapability::ScreenCaptureKit, true),
            "mic_and_system"
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::LoopbackDevice, true),
            "mic_and_system"
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::MicOnly, true),
            "mic_only"
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::Unavailable, true),
            "mic_only"
        );
    }

    #[tokio::test]
    async fn transcribe_segment_uses_meetings_stt_binding() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(FakeStt {
            text: "segment text".into(),
        }));

        let (bindings, _, _) = test_repos().await;
        bindings
            .set(
                "meetings",
                "stt",
                Binding {
                    engine_id: "fake-stt".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        let out = transcribe_meeting_segment(
            &reg,
            &bindings,
            &one_second_pcm(),
        )
        .await
        .unwrap();

        assert_eq!(out, "segment text");
    }

    #[tokio::test]
    async fn synthesize_notes_parses_llm_json() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(FakeLlm));

        let (bindings, _, _) = test_repos().await;
        bindings
            .set(
                "meetings",
                "llm",
                Binding {
                    engine_id: "fake-llm".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        let meeting = Meeting {
            id: "m1".into(),
            title: "Untitled Meeting".into(),
            started_at: "2026-06-26T10:00:00Z".into(),
            ended_at: None,
            status: "recording".into(),
            capture_mode: "mic_only".into(),
            stt_engine_id: None,
            llm_engine_id: None,
            error: None,
        };

        let notes = synthesize_meeting_notes(&reg, &bindings, &meeting, &[])
            .await
            .unwrap();

        assert_eq!(notes.summary, "kickoff summary");
        assert_eq!(notes.action_items, "follow up");
        assert_eq!(notes.prompt_version, MEETING_NOTES_PROMPT_VERSION);
    }

    #[tokio::test]
    async fn synthesize_title_returns_sanitized_text() {
        let mut reg = EngineRegistry::default();
        reg.register_llm(Arc::new(FakeLlm));

        let (bindings, _, _) = test_repos().await;
        bindings
            .set(
                "meetings",
                "llm",
                Binding {
                    engine_id: "fake-llm".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        let title = synthesize_meeting_title(&reg, &bindings, "kickoff summary")
            .await
            .unwrap();
        assert_eq!(title, "Sprint Planning");
    }

    #[tokio::test]
    async fn run_meeting_persists_segments_and_notes() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(CountingFakeStt {
            texts: vec!["hello".into(), "world".into()],
            call: AtomicUsize::new(0),
        }));
        reg.register_llm(Arc::new(FakeLlm));

        let (bindings, actions, meetings) = test_repos().await;
        bindings
            .set(
                "meetings",
                "stt",
                Binding {
                    engine_id: "fake-stt".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();
        bindings
            .set(
                "meetings",
                "llm",
                Binding {
                    engine_id: "fake-llm".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        let mut audio = FakeMeetingAudioIo {
            capability: SystemAudioCapability::MicOnly,
            pending_drains: Mutex::new(vec![one_second_pcm(), one_second_pcm()]),
            ..Default::default()
        };

        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
        };

        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
        };

        let session = run_meeting_start(&mut ctx).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;

        let ev1 = run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed)
            .await
            .unwrap();
        assert!(ev1.is_some());
        assert_eq!(ev1.unwrap().text, "hello");

        let ev2 = run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed)
            .await
            .unwrap();
        assert!(ev2.is_some());
        assert_eq!(ev2.unwrap().text, "world");

        let drain_result = drain_and_stop_meeting(ctx.audio).await;
        let detail = run_meeting_stop(
            ctx.engines, ctx.bindings, ctx.actions, ctx.meetings, &session, drain_result,
        )
        .await
        .unwrap();

        assert_eq!(detail.segments.len(), 2);
        assert_eq!(detail.segments[0].text, "hello");
        assert_eq!(detail.segments[1].text, "world");
        assert_eq!(detail.meeting.title, "Sprint Planning");
        assert!(detail.notes.is_some());
        assert_eq!(
            detail.notes.as_ref().unwrap().summary,
            "kickoff summary"
        );
        assert_eq!(detail.meeting.status, "completed");
        assert_eq!(detail.meeting.capture_mode, "mic_only");

        let action_rows = actions.recent(1).await.unwrap();
        assert_eq!(action_rows.len(), 1);
        assert_eq!(action_rows[0].feature_id, "meetings");
        assert_eq!(action_rows[0].command, "toggle_meeting");
        assert_eq!(action_rows[0].engine_id, "fake-stt");
        assert_eq!(action_rows[0].status, "ok");
    }

    #[tokio::test]
    async fn stop_does_not_duplicate_polled_audio() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(CountingFakeStt {
            texts: vec!["polled".into(), "undelivered remainder".into()],
            call: AtomicUsize::new(0),
        }));
        reg.register_llm(Arc::new(FakeLlm));

        let (bindings, actions, meetings) = test_repos().await;
        bindings
            .set(
                "meetings",
                "stt",
                Binding {
                    engine_id: "fake-stt".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();
        bindings
            .set(
                "meetings",
                "llm",
                Binding {
                    engine_id: "fake-llm".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        // Three drain frames: one for the poll, one for the stop drain (undelivered remainder).
        // stop_meeting returns empty — no tail transcription.
        let mut audio = FakeMeetingAudioIo {
            capability: SystemAudioCapability::MicOnly,
            pending_drains: Mutex::new(vec![
                one_second_pcm(),  // pop() returns this last
                one_second_pcm(),  // pop() returns second
                one_second_pcm(),  // pop() returns this first
            ]),
            ..Default::default()
        };

        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
        };

        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
        };

        let session = run_meeting_start(&mut ctx).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;

        // Poll only once — two frames remain undrained
        let ev = run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed)
            .await
            .unwrap();
        assert!(ev.is_some());
        assert_eq!(ev.unwrap().text, "polled");

        // stop drain picks up one undelivered frame; stop_meeting returns empty
        let drain_result = drain_and_stop_meeting(ctx.audio).await;
        let detail = run_meeting_stop(
            ctx.engines, ctx.bindings, ctx.actions, ctx.meetings, &session, drain_result,
        )
        .await
        .unwrap();

        assert_eq!(detail.segments.len(), 2, "should have polled segment + undelivered remainder only, no tail duplication");
        assert_eq!(detail.segments[0].text, "polled");
        assert_eq!(detail.segments[1].text, "undelivered remainder");
    }

    struct ErroringStt;

    #[async_trait]
    impl SttEngine for ErroringStt {
        fn id(&self) -> &str {
            "fake-stt"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["fake".into()],
            }
        }

        async fn transcribe(
            &self,
            _audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            Err(EngineError::Other("stt is down".into()))
        }
    }

    #[tokio::test]
    async fn stop_with_failing_stt_still_stops_capture_and_finalizes_action() {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(ErroringStt));
        reg.register_llm(Arc::new(FakeLlm));

        let (bindings, actions, meetings) = test_repos().await;
        bindings
            .set(
                "meetings",
                "stt",
                Binding {
                    engine_id: "fake-stt".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();
        bindings
            .set(
                "meetings",
                "llm",
                Binding {
                    engine_id: "fake-llm".into(),
                    model: None,
                    provider_ref: None,
                },
            )
            .await
            .unwrap();

        // One >=1s frame left for the stop drain, so the final transcription
        // runs (and fails via ErroringStt).
        let mut audio = FakeMeetingAudioIo {
            capability: SystemAudioCapability::MicOnly,
            pending_drains: Mutex::new(vec![one_second_pcm()]),
            ..Default::default()
        };

        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
        };

        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
        };

        let session = run_meeting_start(&mut ctx).await.unwrap();
        let drain_result = drain_and_stop_meeting(ctx.audio).await;
        let err = run_meeting_stop(
            ctx.engines, ctx.bindings, ctx.actions, ctx.meetings, &session, drain_result,
        )
        .await
        .unwrap_err();
        assert!(err.contains("stt is down"), "unexpected error: {err}");

        // Capture must be released even though transcription failed.
        assert_eq!(audio.meeting_state(), MeetingState::Idle);

        // The action row must not be left pending.
        let detail = actions.get(session.action_id).await.unwrap().unwrap();
        assert_eq!(detail.status, "error");
        assert!(detail.error.is_some());
    }
}
