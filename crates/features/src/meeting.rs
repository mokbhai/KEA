use std::sync::Arc;
use std::time::{Duration, Instant};

use kea_core::dictation::{apply_vocabulary, hint_terms};
use kea_core::meetings::{
    attribute_segment, build_interim_notes_request, build_meeting_notes_request,
    build_meeting_title_request, build_notes_repair_request, format_transcript_for_synthesis,
    parse_meeting_notes_json, render_action_items_prose, sanitize_meeting_title,
    should_run_interim, InterimCadence, MeetingSettings, ParsedMeetingNotes, SpeakerChannel,
    MAX_CONSECUTIVE_INTERIM_FAILURES, MEETING_INTERIM_PROMPT_VERSION, MEETING_NOTES_PROMPT_VERSION,
};
use kea_core::resolve::SlotResolver;
use kea_core::store::actions::{ActionRepo, ActionStatus, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::meetings::{
    ActionItem, CaptureMode, Meeting, MeetingDetail, MeetingNotes, MeetingRepo, MeetingSpeaker,
    MeetingStatus, NewActionItem, NewMeeting, NewSegment, TitleSource,
};
use kea_core::store::usage::{NewUsageEvent, UsageRepo};
use kea_core::store::vocabulary::VocabularyEntry;
use kea_engines::traits::{AudioPcm, LlmEngine, SttOpts, Transcript};
use kea_engines::EngineRegistry;
use kea_platform::audio::util::resample_linear;
use kea_platform::audio::SpeechSegment;
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

fn capture_mode(cap: SystemAudioCapability, prefer_system_audio: bool) -> CaptureMode {
    if !prefer_system_audio {
        return CaptureMode::MicOnly;
    }
    match cap {
        SystemAudioCapability::ScreenCaptureKit | SystemAudioCapability::LoopbackDevice => {
            CaptureMode::MicAndSystem
        }
        _ => CaptureMode::MicOnly,
    }
}

/// Which side of the meeting a drained segment came from.
///
/// Deliberately reads the *segment*, not the capture mode: the capture mode is
/// what was asked for, and the two halves are what was actually recorded. A
/// loopback stream that never delivered a frame leaves both halves `None`, and
/// the honest answer then is the same as for a mic-only meeting — everything
/// on the recording reached it through the microphone.
///
/// Deriving it here also keeps the poll off `system_audio_capability()`, which
/// re-probes the host's devices on every call.
fn segment_speaker(segment: &SpeechSegment) -> Option<SpeakerChannel> {
    match (&segment.mic, &segment.system) {
        (Some(mic), Some(system)) => Some(attribute_segment(
            &mic.samples,
            &system.samples,
            mic.sample_rate_hz,
        )),
        // One source: there is nothing to compare, so this is the person
        // sitting here rather than an unknown. Saying "you" is honest;
        // inventing a second speaker from a channel that was never recorded
        // is not.
        _ => Some(SpeakerChannel::Local),
    }
}

/// The two sides a meeting can have, and what they are called before anyone
/// renames them. A mic-only meeting has exactly one.
fn default_speakers(mode: CaptureMode) -> &'static [SpeakerChannel] {
    match mode {
        CaptureMode::MicOnly => &[SpeakerChannel::Local],
        CaptureMode::MicAndSystem => &[SpeakerChannel::Local, SpeakerChannel::Remote],
    }
}

fn new_meeting_id() -> String {
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("meeting-{millis}")
}

/// Thin single-segment STT call (testable with a fake engine).
pub async fn transcribe_pcm_segment(
    engine: &dyn kea_engines::traits::SttEngine,
    pcm: &PcmFrame,
    opts: SttOpts,
) -> Result<Transcript, kea_engines::traits::EngineError> {
    engine.transcribe(pcm_to_audio(pcm.clone()), opts).await
}

pub async fn transcribe_meeting_segment(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    audio: &PcmFrame,
    vocabulary: &[VocabularyEntry],
) -> Result<String, String> {
    let binding = SlotResolver::new(engines, bindings)
        .require_stt("meetings")
        .await
        .map_err(|e| e.to_string())?;
    let engine_id = &binding.engine_id;

    let engine = engines
        .stt(engine_id)
        .ok_or_else(|| format!("no stt engine '{engine_id}'"))?;

    let stt_opts = SttOpts {
        model: binding.model.clone(),
        // Deliberately not `dictation.language`. Meetings have no language
        // setting of their own, and silently borrowing dictation's would mean a
        // user who pinned dictation to one language finds their meetings
        // decoded as it too, with nothing in the meetings UI to explain why.
        language: None,
        provider_ref: binding.provider_ref.clone(),
        vocabulary: hint_terms(vocabulary),
    };

    let transcript = transcribe_pcm_segment(engine.as_ref(), audio, stt_opts)
        .await
        .map_err(|e| e.to_string())?;

    Ok(transcript.text)
}

/// Resolve the meetings LLM slot once, returning the engine and its binding.
///
/// Notes, the repair round trip, the interim pass and the title call all need
/// the same two things from the same slot; resolving them in one place is what
/// keeps `provider_ref` and `model` from being forgotten on one of the paths.
async fn meetings_llm(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
) -> Result<(std::sync::Arc<dyn LlmEngine>, Binding), String> {
    let binding = SlotResolver::new(engines, bindings)
        .require_llm("meetings")
        .await
        .map_err(|e| e.to_string())?;
    let engine = engines
        .llm(&binding.engine_id)
        .ok_or_else(|| format!("no llm engine '{}'", binding.engine_id))?;
    Ok((engine, binding))
}

/// Writes one meeting LLM call to the usage ledger.
///
/// Best effort on purpose: a failed ledger write must not fail a meeting the
/// user has already sat through, so it is logged and swallowed. The counts are
/// the provider's own or absent — never a guess.
///
/// `action_id` stays `None` for every meeting call. The stop path owns a
/// ledger row and the interim passes deliberately own none (see
/// [`run_interim_notes_pass`]), so attributing only some of a meeting's calls
/// would make the per-action view lie about the rest; the feature and the
/// model are what the usage view groups by anyway.
async fn record_meeting_usage(
    usage: Option<&UsageRepo>,
    binding: &Binding,
    reported: Option<kea_engines::traits::TokenUsage>,
) {
    let Some(repo) = usage else { return };
    let event = NewUsageEvent {
        model: binding.model.clone(),
        provider_ref: binding.provider_ref.clone(),
        prompt_tokens: reported.map(|u| i64::from(u.prompt)),
        completion_tokens: reported.map(|u| i64::from(u.completion)),
        ..NewUsageEvent::new("meetings", &binding.engine_id)
    };
    if let Err(error) = repo.record(&event).await {
        tracing::warn!(%error, "meeting: could not record token usage");
    }
}

/// Complete `req` and parse the reply as notes, with exactly one repair round
/// trip if it will not parse.
///
/// Four layers get JSON out of a provider that promises none of tool calling,
/// JSON schema mode or `response_format`: the prompt carries a literal example
/// (`meeting_notes_system_prompt`), the parser strips fences and scans for the
/// first balanced object (`parse_meeting_notes_json`), this function asks once
/// more, and the caller below keeps the raw text rather than failing the
/// meeting. `response_format` is the fifth layer and is *not* implemented here
/// — see the note on [`synthesize_meeting_notes`].
///
/// Exactly one retry. A loop bills the user once per attempt against a server
/// that has already shown it cannot produce the shape.
async fn complete_notes(
    engine: &dyn LlmEngine,
    binding: &Binding,
    mut req: kea_engines::LlmRequest,
    usage: Option<&UsageRepo>,
) -> Result<ParsedMeetingNotes, String> {
    req.model = binding.model.clone();
    req.provider_ref = binding.provider_ref.clone();
    let first = engine.complete(req).await.map_err(|e| e.to_string())?;
    // Recorded before the parse: a reply that will not parse still cost the
    // user, and the repair round trip below costs them again.
    record_meeting_usage(usage, binding, first.usage).await;

    match parse_meeting_notes_json(&first.text) {
        Ok(parsed) => Ok(parsed),
        Err(e) => {
            tracing::warn!(error = %e, "meeting: notes reply was not JSON, asking once more");
            let mut repair = build_notes_repair_request(&first.text);
            repair.model = binding.model.clone();
            repair.provider_ref = binding.provider_ref.clone();
            let second = engine.complete(repair).await.map_err(|e| e.to_string())?;
            record_meeting_usage(usage, binding, second.usage).await;
            parse_meeting_notes_json(&second.text).map_err(|e| {
                // Carried up so the caller can keep the raw text as the
                // summary rather than failing a meeting the user already paid
                // to transcribe.
                format!("{e}|raw:{}", second.text)
            })
        }
    }
}

/// Notes from a reply that would not parse even after the repair round trip.
///
/// The raw text becomes the summary and the structured fields stay empty.
/// Before this, a malformed reply marked the whole meeting
/// [`MeetingStatus::Error`] through [`ActiveMeeting::fail`] and the user lost a
/// transcript they had already paid to produce.
fn notes_from_unparsable_reply(raw: &str) -> ParsedMeetingNotes {
    ParsedMeetingNotes {
        summary: raw.trim().to_string(),
        ..Default::default()
    }
}

/// Split the `{error}|raw:{text}` marker [`complete_notes`] uses to carry a
/// failed reply's text up with its error.
fn unparsable_reply_text(error: &str) -> Option<&str> {
    error.split_once("|raw:").map(|(_, raw)| raw)
}

/// Turn parsed notes into the row to store, and persist the action items as
/// rows at the same time.
///
/// The rows are the source of truth and `meeting_notes.action_items` is a
/// derived view of them, re-rendered on every write, so `MeetingDetail` and
/// anything else reading the column keeps working and a meeting recorded
/// before the table still renders.
async fn persist_notes(
    meetings: &MeetingRepo,
    meeting_id: &str,
    parsed: &ParsedMeetingNotes,
    prompt_version: &str,
    binding: &Binding,
) -> Result<MeetingNotes, String> {
    let incoming: Vec<NewActionItem> = parsed
        .action_item_rows()
        .into_iter()
        .map(|item| NewActionItem {
            text: item.text,
            owner: item.owner,
            due_hint: item.due_hint,
            source_seq: None,
        })
        .collect();

    let rows = meetings
        .merge_action_items(meeting_id, &incoming)
        .await
        .map_err(|e| e.to_string())?;

    let notes = MeetingNotes {
        meeting_id: meeting_id.to_string(),
        summary: parsed.summary.clone(),
        decisions: parsed.decisions.clone(),
        action_items: action_items_prose(&rows, &parsed.action_items),
        follow_ups: parsed.follow_ups.clone(),
        open_questions: parsed.open_questions.clone(),
        prompt_version: prompt_version.to_string(),
        engine_id: Some(binding.engine_id.clone()),
        model: binding.model.clone(),
    };

    meetings
        .upsert_notes(&notes)
        .await
        .map_err(|e| e.to_string())?;

    Ok(notes)
}

/// The prose column: the rows rendered back, or what the model wrote if there
/// are no rows to render.
fn action_items_prose(rows: &[ActionItem], fallback: &str) -> String {
    if rows.is_empty() {
        return fallback.to_string();
    }
    let parsed: Vec<kea_core::meetings::ParsedActionItem> = rows
        .iter()
        .map(|row| kea_core::meetings::ParsedActionItem {
            text: row.text.clone(),
            owner: row.owner.clone(),
            due_hint: row.due_hint.clone(),
        })
        .collect();
    render_action_items_prose(&parsed)
}

/// The final pass at stop: the whole transcript, in one request.
///
/// # `response_format` is deliberately absent
///
/// Layer 3 of the plan's JSON reliability design — sending
/// `"response_format": {"type": "json_object"}`, retrying without it on a 400
/// that mentions the field, and remembering the answer per provider — needs
/// `json_mode` on `LlmRequest` and a branch in `post_chat_completion`, both in
/// `kea-engines`, which this change does not own. Layers 1, 2 and 4 (the
/// literal example, the tolerant parser and the one repair round trip) are all
/// here and carry the load without it; layer 3 is a latency and token saving
/// on providers that support it, not a correctness requirement.
async fn synthesize_notes_parsed(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meeting: &Meeting,
    segments: &[kea_core::MeetingSegment],
    speakers: &[MeetingSpeaker],
    usage: Option<&UsageRepo>,
) -> Result<(ParsedMeetingNotes, Binding), String> {
    let (engine, binding) = meetings_llm(engines, bindings).await?;

    let transcript = format_transcript_for_synthesis(segments, speakers);
    let req = build_meeting_notes_request(&meeting.title, &meeting.started_at, &transcript);

    let parsed = match complete_notes(engine.as_ref(), &binding, req, usage).await {
        Ok(parsed) => parsed,
        Err(e) => match unparsable_reply_text(&e) {
            Some(raw) => {
                tracing::warn!("meeting: keeping an unparsable notes reply as the summary");
                notes_from_unparsable_reply(raw)
            }
            None => return Err(e),
        },
    };
    Ok((parsed, binding))
}

pub async fn synthesize_meeting_notes(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meeting: &Meeting,
    segments: &[kea_core::MeetingSegment],
    speakers: &[MeetingSpeaker],
    usage: Option<&UsageRepo>,
) -> Result<MeetingNotes, String> {
    let (parsed, binding) =
        synthesize_notes_parsed(engines, bindings, meeting, segments, speakers, usage).await?;

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

/// One interim notes pass, folding the segments recorded since `from_sequence`
/// into the notes already stored.
///
/// # What this deliberately does not do
///
/// It takes no [`ActiveMeeting`], so it *cannot* call [`ActiveMeeting::fail`].
/// A failed interim pass is an optional extra that did not happen: the meeting
/// stays `Recording`, the ledger row stays open, and capture is untouched. The
/// caller logs, emits `meeting:notes_error`, and counts the failure against
/// [`MAX_CONSECUTIVE_INTERIM_FAILURES`].
///
/// It also never touches the meeting title. An interim pass can run long
/// before the title step, and after item 17 that title may be a calendar title
/// that must never reach an engine.
pub async fn run_interim_notes_pass(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meetings: &MeetingRepo,
    meeting_id: &str,
    from_sequence: i32,
    usage: Option<&UsageRepo>,
) -> Result<InterimPass, String> {
    let detail = meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))?;

    let new_segments: Vec<kea_core::MeetingSegment> = detail
        .segments
        .iter()
        .filter(|s| s.sequence >= from_sequence)
        .cloned()
        .collect();

    // Nothing was said since the last pass. Returning early is not an
    // optimisation: sending an empty transcript invites the model to
    // re-summarise from nothing and quietly shrink the notes.
    if new_segments.is_empty() {
        return Ok(InterimPass {
            notes: detail.notes,
            next_sequence: from_sequence,
        });
    }

    let previous = detail
        .notes
        .as_ref()
        .map(|n| ParsedMeetingNotes {
            summary: n.summary.clone(),
            decisions: n.decisions.clone(),
            action_items: n.action_items.clone(),
            follow_ups: n.follow_ups.clone(),
            open_questions: n.open_questions.clone(),
            action_item_rows: Vec::new(),
        })
        .unwrap_or_default();

    let (engine, binding) = meetings_llm(engines, bindings).await?;
    let req = build_interim_notes_request(&previous, &new_segments, &detail.speakers);
    let parsed = match complete_notes(engine.as_ref(), &binding, req, usage).await {
        Ok(parsed) => parsed,
        // An interim pass that cannot be parsed keeps the previous notes
        // rather than replacing good notes with a raw error string; the final
        // pass at stop is where an unparsable reply is worth keeping.
        Err(e) => return Err(e.split("|raw:").next().unwrap_or(&e).to_string()),
    };

    let next_sequence = new_segments
        .iter()
        .map(|s| s.sequence)
        .max()
        .map(|max| max + 1)
        .unwrap_or(from_sequence);

    let notes = persist_notes(
        meetings,
        meeting_id,
        &parsed,
        MEETING_INTERIM_PROMPT_VERSION,
        &binding,
    )
    .await?;

    Ok(InterimPass {
        notes: Some(notes),
        next_sequence,
    })
}

/// What one interim pass produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InterimPass {
    /// The stored notes after the pass, or the ones already there when there
    /// was nothing new to fold in.
    pub notes: Option<MeetingNotes>,
    /// The first segment sequence the *next* pass should start from.
    pub next_sequence: i32,
}

/// One meeting's interim schedule: when the next pass is due, how many have
/// run, and when to give up.
///
/// The owner of every counter the cadence rule reads, so the app layer stores
/// one of these beside the active meeting and never does the arithmetic
/// itself. The in-flight guard stays in the app layer — it is an `AtomicBool`
/// on the shared state, not per-meeting bookkeeping — and this struct is
/// deliberately not `Sync`-clever: it is touched only from the poll task.
pub struct InterimSchedule {
    cadence: InterimCadence,
    /// The first segment not yet folded into the notes.
    next_sequence: i32,
    segments_since: u32,
    last_pass: Instant,
    passes: u32,
    consecutive_failures: u32,
}

impl InterimSchedule {
    pub fn new(cadence: InterimCadence) -> Self {
        Self {
            cadence,
            next_sequence: 0,
            segments_since: 0,
            // The meeting start is pass zero for cadence purposes, so the
            // first pass waits a full window rather than firing on segment 8
            // of a meeting that is 40 seconds old.
            last_pass: Instant::now(),
            passes: 0,
            consecutive_failures: 0,
        }
    }

    /// Record that one more segment has been transcribed.
    pub fn note_segment(&mut self) {
        self.segments_since = self.segments_since.saturating_add(1);
    }

    /// Where the next pass starts reading.
    pub fn next_sequence(&self) -> i32 {
        self.next_sequence
    }

    /// Whether the schedule has given up for the rest of this meeting.
    pub fn is_disabled(&self) -> bool {
        self.consecutive_failures >= MAX_CONSECUTIVE_INTERIM_FAILURES
    }

    /// Whether a pass is due now.
    pub fn is_due(&self) -> bool {
        !self.is_disabled()
            && should_run_interim(
                self.segments_since,
                self.elapsed().as_secs(),
                self.passes,
                &self.cadence,
            )
    }

    fn elapsed(&self) -> Duration {
        self.last_pass.elapsed()
    }

    /// A pass finished. Resets the counters and clears the failure streak.
    pub fn record_success(&mut self, next_sequence: i32) {
        self.next_sequence = next_sequence;
        self.segments_since = 0;
        self.last_pass = Instant::now();
        self.passes = self.passes.saturating_add(1);
        self.consecutive_failures = 0;
    }

    /// A pass failed.
    ///
    /// The segment counter is *not* reset: those segments still have not been
    /// folded in, and pretending they have would silently drop them from the
    /// interim notes for the rest of the meeting. The clock is reset so a
    /// failing provider is retried on the cadence rather than on every tick.
    pub fn record_failure(&mut self) {
        self.last_pass = Instant::now();
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
    }
}

pub async fn synthesize_meeting_title(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    summary: &str,
    usage: Option<&UsageRepo>,
) -> Result<String, String> {
    let (engine, binding) = meetings_llm(engines, bindings).await?;

    let mut req = build_meeting_title_request(summary);
    req.model = binding.model.clone();
    req.provider_ref = binding.provider_ref.clone();
    let resp = engine.complete(req).await.map_err(|e| e.to_string())?;
    record_meeting_usage(usage, &binding, resp.usage).await;
    Ok(sanitize_meeting_title(&resp.text))
}

pub struct MeetingRunContext<'a> {
    pub engines: &'a EngineRegistry,
    pub bindings: &'a BindingRepo,
    pub actions: &'a ActionRepo,
    pub meetings: &'a MeetingRepo,
    pub audio: &'a mut dyn AudioIo,
    pub settings: &'a MeetingSettings,
    /// Terms to bias transcription toward and to normalize spelling against.
    /// Read once when the meeting starts rather than per segment: a meeting
    /// whose transcript changed spelling halfway through because the user
    /// edited their vocabulary mid-recording would be worse than either answer.
    pub vocabulary: &'a [VocabularyEntry],
}

pub struct ActiveMeeting {
    pub meeting_id: String,
    pub action_id: i64,
}

impl ActiveMeeting {
    /// Closes both rows this session owns — the meeting and its ledger entry —
    /// as `error`, and hands back the message to return. A DB failure while
    /// doing so is logged, never propagated: the run's own error is the one the
    /// caller asked about.
    pub async fn fail(
        &self,
        meetings: &MeetingRepo,
        actions: &ActionRepo,
        e: impl std::fmt::Display,
    ) -> String {
        let msg = e.to_string();
        if let Err(inner) = meetings
            .complete(&self.meeting_id, MeetingStatus::Error, Some(&msg))
            .await
        {
            tracing::warn!(
                error = %inner,
                meeting_id = %self.meeting_id,
                "meeting: failed to mark meeting as error in DB"
            );
        }
        if let Err(inner) = actions
            .finish(self.action_id, ActionStatus::Error, Some(&msg))
            .await
        {
            tracing::warn!(
                error = %inner,
                action_id = %self.action_id,
                "meeting: failed to finish action as error in DB"
            );
        }
        msg
    }

    /// Closes both rows as a finished run. Unlike [`ActiveMeeting::fail`] a
    /// failure here is propagated — there is no other error to report, and the
    /// caller is about to read the meeting back.
    pub async fn complete(
        &self,
        meetings: &MeetingRepo,
        actions: &ActionRepo,
    ) -> Result<(), String> {
        meetings
            .complete(&self.meeting_id, MeetingStatus::Completed, None)
            .await
            .map_err(|e| e.to_string())?;

        actions
            .finish(self.action_id, ActionStatus::Ok, None)
            .await
            .map_err(|e| e.to_string())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeetingSegmentEvent {
    pub meeting_id: String,
    pub sequence: i32,
    pub text: String,
    pub start_offset_ms: i64,
    pub end_offset_ms: i64,
    /// The persisted speaker key, so the live transcript can label the line
    /// while it is still being recorded rather than only after a reload.
    pub speaker_key: Option<String>,
}

#[allow(clippy::too_many_arguments)]
async fn append_transcribed_segment(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meetings: &MeetingRepo,
    meeting_id: &str,
    pcm: PcmFrame,
    speaker: Option<SpeakerChannel>,
    sequence: i32,
    start_offset_ms: i64,
    vocabulary: &[VocabularyEntry],
) -> Result<MeetingSegmentEvent, String> {
    let duration_ms = pcm_duration_ms(&pcm);
    let end_offset_ms = start_offset_ms + duration_ms;

    let text = transcribe_meeting_segment(engines, bindings, &pcm, vocabulary).await?;
    // Same reason as dictation: the stored segment is what the notes are
    // synthesized from and what the user reads, so it carries the corrected
    // spelling rather than the decoder's first guess.
    let text = apply_vocabulary(&text, vocabulary);

    meetings
        .append_segment(
            meeting_id,
            &NewSegment {
                sequence,
                start_offset_ms,
                end_offset_ms,
                text: text.clone(),
                speaker,
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
        speaker_key: speaker.map(|c| c.as_str().to_string()),
    })
}

pub async fn run_meeting_start(ctx: &mut MeetingRunContext<'_>) -> Result<ActiveMeeting, String> {
    let resolver = SlotResolver::new(ctx.engines, ctx.bindings);
    let stt_binding = resolver
        .require_stt("meetings")
        .await
        .map_err(|e| e.to_string())?;
    let llm_binding = resolver
        .require_llm("meetings")
        .await
        .map_err(|e| e.to_string())?;

    let capture_mode = capture_mode(
        ctx.audio.system_audio_capability(),
        ctx.settings.prefer_system_audio,
    );
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
            capture_mode,
            stt_engine_id: Some(stt_binding.engine_id.clone()),
            llm_engine_id: Some(llm_binding.engine_id),
        })
        .await
    {
        let _ = ctx.audio.stop_meeting().await;
        return Err(e.to_string());
    }

    // Seed the meeting's speaker rows now rather than on the first attributed
    // segment, so the detail view has names to show — and to rename — from the
    // moment recording starts. A failure here is logged, not fatal: a meeting
    // with unnamed sides is still a meeting.
    for channel in default_speakers(capture_mode) {
        if let Err(e) = ctx
            .meetings
            .ensure_speaker(&meeting_id, *channel, channel.display_name())
            .await
        {
            tracing::warn!(
                error = %e,
                meeting_id = %meeting_id,
                channel = channel.as_str(),
                "meeting: failed to seed speaker row"
            );
        }
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
            if let Err(inner) = ctx
                .meetings
                .complete(&meeting_id, MeetingStatus::Error, Some(&err))
                .await
            {
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
    // Ends the segment at a pause in speech when there is one, and at the
    // user's configured length when the speaker never pauses — a boundary on
    // the clock alone lands mid-word.
    let cut_cfg = kea_platform::audio::segment::SegmentCutConfig {
        max_secs: ctx.settings.segment_duration_secs as f32,
        ..Default::default()
    };
    let Some(segment) = ctx
        .audio
        .try_drain_meeting_segment(cut_cfg)
        .await
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };

    // Silence is dropped rather than transcribed: a model handed a silent
    // clip tends to emit plausible-looking text that was never spoken.
    if !segment.has_speech {
        return Ok(None);
    }

    if !has_min_audio(&segment.pcm) {
        return Ok(None);
    }

    // Decided before the STT call, off the two halves this segment carries.
    // They are dropped with the segment either way — there is no recording on
    // disk to go back to — so attribution has to happen while the audio is
    // still in hand.
    let speaker = segment_speaker(&segment);

    let event = append_transcribed_segment(
        ctx.engines,
        ctx.bindings,
        ctx.meetings,
        meeting_id,
        segment.pcm,
        speaker,
        *sequence,
        *elapsed_ms,
        ctx.vocabulary,
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
pub async fn drain_and_stop_meeting(audio: &mut dyn AudioIo) -> Result<SpeechSegment, String> {
    let drain_result = audio
        .drain_meeting_sources()
        .await
        .map_err(|e| e.to_string());
    let _ = audio.stop_meeting().await.map_err(|e| e.to_string());
    drain_result
}

/// Post-capture phase of stopping a meeting: transcribe the final tail
/// segment, synthesize notes/title, and finalize the meeting + action rows.
/// Takes no audio handle — the caller has already drained and released
/// capture via [`drain_and_stop_meeting`] and passes the drained result in.
/// Somewhere a recording can get a name that is not generated.
///
/// The port for calendar titles, kept here rather than taking
/// `kea_platform::calendar::CalendarIo` directly for one structural reason:
/// this trait gives the notes path *only* a title. There is no method that
/// returns an event, so no future edit to `finalize_meeting` can reach an
/// attendee list, a location or a body, and the privacy rule stops being
/// something a reviewer has to check.
///
/// `'static` and `Send + Sync` because the implementation is blocking — see
/// [`calendar_title`].
pub trait MeetingTitleSource: Send + Sync + 'static {
    /// The title of the event this recording happened during, or `None` for
    /// any reason at all. Implementations never fail: a calendar that cannot
    /// be read has no title to offer, which is the same answer as an empty
    /// calendar and leads to the same fallback.
    fn title_for(&self, started_at: &str, ended_at: Option<&str>) -> Option<String>;
}

/// Everything the stop needs beyond the repos: today, only the calendar.
///
/// A struct rather than two more positional arguments, and
/// [`MeetingStopOptions::default`] is exactly today's behaviour, so
/// [`run_meeting_stop`] keeps the signature its one caller already uses while
/// [`run_meeting_stop_with`] is the door for the calendar.
#[derive(Default)]
pub struct MeetingStopOptions {
    /// Where a title may come from. `None` is "no calendar on this build".
    pub calendar: Option<Arc<dyn MeetingTitleSource>>,
    /// The `meetings.calendar_titles` setting. `false` skips the lookup
    /// entirely — no permission check, no EventKit initialization, nothing.
    pub calendar_titles: bool,
    /// Where the stop's LLM calls are counted, or `None` to count nothing.
    ///
    /// Owned rather than borrowed because this struct is built by value at the
    /// call site and `UsageRepo` is a pool handle — a clone of an `Arc`, not a
    /// connection.
    pub usage: Option<UsageRepo>,
}

/// How long the calendar read is given before the stop gives up on it.
///
/// EventKit's first access on a machine with a large store can be slow. This
/// path is already doing STT and LLM work so it is not latency-critical, but
/// hanging the stop on a calendar lookup would be a much worse bug than
/// falling through to the generated title.
const CALENDAR_READ_TIMEOUT: Duration = Duration::from_secs(2);

#[allow(clippy::too_many_arguments)]
pub async fn run_meeting_stop(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    meetings: &MeetingRepo,
    session: &ActiveMeeting,
    drain_result: Result<SpeechSegment, String>,
    vocabulary: &[VocabularyEntry],
) -> Result<MeetingDetail, String> {
    run_meeting_stop_with(
        engines,
        bindings,
        actions,
        meetings,
        session,
        drain_result,
        vocabulary,
        MeetingStopOptions::default(),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub async fn run_meeting_stop_with(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    meetings: &MeetingRepo,
    session: &ActiveMeeting,
    drain_result: Result<SpeechSegment, String>,
    vocabulary: &[VocabularyEntry],
    opts: MeetingStopOptions,
) -> Result<MeetingDetail, String> {
    let meeting_id = &session.meeting_id;

    // Reading the row back is the precondition of the stop, not part of it: a
    // meeting that is not there owns no rows to close as an error.
    let existing = meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))?;

    if let Err(e) = finalize_meeting(
        engines,
        bindings,
        meetings,
        session,
        &existing,
        drain_result,
        vocabulary,
        &opts,
    )
    .await
    {
        return Err(session.fail(meetings, actions, e).await);
    }

    session.complete(meetings, actions).await?;

    meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))
}

/// Everything between the drain and the final status write: transcribe the tail
/// segment, synthesize notes and a title, and persist both. Every step here
/// closes the meeting and the action rows as an error via the one epilogue in
/// [`run_meeting_stop`].
#[allow(clippy::too_many_arguments)]
async fn finalize_meeting(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meetings: &MeetingRepo,
    session: &ActiveMeeting,
    existing: &MeetingDetail,
    drain_result: Result<SpeechSegment, String>,
    vocabulary: &[VocabularyEntry],
    opts: &MeetingStopOptions,
) -> Result<(), String> {
    let meeting_id = &session.meeting_id;

    let sequence = existing.segments.len() as i32;
    let elapsed_ms = existing
        .segments
        .last()
        .map(|s| s.end_offset_ms)
        .unwrap_or(0);

    let tail = drain_result?;

    if has_min_audio(&tail.pcm) {
        // The tail is attributed like every other segment: the last thing said
        // in a meeting is as worth labelling as the first.
        let speaker = segment_speaker(&tail);
        append_transcribed_segment(
            engines, bindings, meetings, meeting_id, tail.pcm, speaker, sequence, elapsed_ms,
            vocabulary,
        )
        .await?;
    }

    let partial = meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))?;

    // The final pass reads the whole transcript rather than folding, even when
    // interim passes already wrote this row. The fold is an approximation, and
    // the version the user keeps is the one place a fresh full-context read is
    // worth paying for.
    let (parsed, binding) = synthesize_notes_parsed(
        engines,
        bindings,
        &partial.meeting,
        &partial.segments,
        &partial.speakers,
        opts.usage.as_ref(),
    )
    .await?;

    let notes = persist_notes(
        meetings,
        meeting_id,
        &parsed,
        MEETING_NOTES_PROMPT_VERSION,
        &binding,
    )
    .await?;

    // The title step, and the one place a calendar title may be applied.
    //
    // The ordering is not incidental. `build_meeting_notes_request` embeds the
    // meeting title verbatim in the prompt, so a calendar title written to the
    // row *before* the notes call above would be transmitted to whatever
    // hosted provider is bound — exactly the leak the privacy rule forbids.
    // Applying it here, after synthesis has already run against
    // "Untitled Meeting", prevents that structurally rather than by
    // convention.
    if let Some(event_title) = calendar_title(opts, &partial.meeting).await {
        return meetings
            .set_title_with_source(meeting_id, &event_title, TitleSource::Calendar)
            .await
            .map_err(|e| e.to_string());
    }

    let title =
        synthesize_meeting_title(engines, bindings, &notes.summary, opts.usage.as_ref()).await?;

    meetings
        .set_title_with_source(meeting_id, &title, TitleSource::Llm)
        .await
        .map_err(|e| e.to_string())
}

/// The title of the calendar event this recording happened during, if there is
/// one worth using.
///
/// Every failure — feature off, no calendar on this build, permission denied,
/// EventKit error, timeout, empty calendar, no match above the threshold —
/// returns `None`, which sends the caller to the LLM title path that runs
/// today. Nothing here can propagate with `?`: `finalize_meeting`'s errors
/// reach [`ActiveMeeting::fail`], and a calendar failure must never mark a
/// meeting as an error when the existing title path would have succeeded.
///
/// The window read is `[start − 10 min, start + 10 min]` and nothing wider.
/// Reading the day or the calendar would pull in events that have no bearing
/// on this recording, which is both a worse match and more of someone's
/// calendar than this feature needs.
async fn calendar_title(opts: &MeetingStopOptions, meeting: &Meeting) -> Option<String> {
    if !opts.calendar_titles {
        return None;
    }
    let calendar = opts.calendar.clone()?;
    let started_at = meeting.started_at.clone();
    let ended_at = meeting.ended_at.clone();

    // The lookup is blocking — EventKit's first access on a machine with a
    // large calendar store can take a moment — so it runs on the blocking pool
    // and the stop only *waits* for it for a bounded time. A timeout cannot
    // cancel a blocking FFI call; what it does is stop a slow calendar hanging
    // the stop path, which is the failure that matters.
    let lookup =
        tokio::task::spawn_blocking(move || calendar.title_for(&started_at, ended_at.as_deref()));

    match tokio::time::timeout(CALENDAR_READ_TIMEOUT, lookup).await {
        Ok(Ok(title)) => title,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "meeting: calendar lookup panicked, using the generated title");
            None
        }
        Err(_) => {
            tracing::warn!("meeting: calendar lookup timed out, using the generated title");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::bindings::Binding;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::traits::{
        EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse, SttEngine,
    };
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
            Ok(Transcript::text_only(self.text.clone()))
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
            Ok(Transcript::text_only(text))
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
                return Ok(LlmResponse::untracked("Sprint Planning"));
            }
            Ok(LlmResponse::untracked(
                r#"{"summary":"kickoff summary","decisions":"","action_items":"follow up","follow_ups":"","open_questions":""}"#,
            ))
        }
    }

    struct FakeMeetingAudioIo {
        dictation_state: DictationState,
        meeting_state: MeetingState,
        capability: SystemAudioCapability,
        buffered: PcmFrame,
        /// Each entry stands for one segment the cut logic released. Where the
        /// boundary falls is covered by the cut tests in kea-platform; these
        /// tests are about what the meeting does with a segment once it has
        /// one, including which halves it carries.
        pending_drains: Mutex<Vec<SpeechSegment>>,
    }

    /// A mic-only segment: one source, so no halves to compare.
    fn mic_only_segment(pcm: PcmFrame) -> SpeechSegment {
        SpeechSegment {
            pcm,
            has_speech: true,
            mic: None,
            system: None,
        }
    }

    /// A two-source segment whose halves are `mic_amplitude` and
    /// `system_amplitude` loud throughout — enough for `attribute_segment` to
    /// reach a verdict without a real recording.
    fn two_source_segment(mic_amplitude: f32, system_amplitude: f32) -> SpeechSegment {
        let tone = |amplitude: f32| PcmFrame {
            samples: (0..16_000)
                .map(|i| {
                    let t = i as f32 / 16_000.0;
                    (std::f32::consts::TAU * 220.0 * t).sin() * amplitude
                })
                .collect(),
            sample_rate_hz: 16_000,
        };
        let mic = tone(mic_amplitude);
        let system = tone(system_amplitude);
        SpeechSegment {
            pcm: kea_platform::audio::mix_frames(&mic, &system),
            has_speech: true,
            mic: Some(mic),
            system: Some(system),
        }
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

        async fn drain_meeting_sources(&mut self) -> Result<SpeechSegment, AudioIoError> {
            Ok(self
                .pending_drains
                .lock()
                .unwrap()
                .pop()
                .unwrap_or_else(|| {
                    mic_only_segment(PcmFrame {
                        samples: vec![],
                        sample_rate_hz: 16_000,
                    })
                }))
        }

        async fn try_drain_meeting_segment(
            &mut self,
            _cfg: kea_platform::audio::segment::SegmentCutConfig,
        ) -> Result<Option<SpeechSegment>, AudioIoError> {
            Ok(self.pending_drains.lock().unwrap().pop())
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
            CaptureMode::MicOnly
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::LoopbackDevice, false),
            CaptureMode::MicOnly
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::MicOnly, false),
            CaptureMode::MicOnly
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::Unavailable, false),
            CaptureMode::MicOnly
        );
    }

    #[test]
    fn capture_mode_respects_capability_when_user_prefers_system_audio() {
        assert_eq!(
            capture_mode(SystemAudioCapability::ScreenCaptureKit, true),
            CaptureMode::MicAndSystem
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::LoopbackDevice, true),
            CaptureMode::MicAndSystem
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::MicOnly, true),
            CaptureMode::MicOnly
        );
        assert_eq!(
            capture_mode(SystemAudioCapability::Unavailable, true),
            CaptureMode::MicOnly
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

        let out = transcribe_meeting_segment(&reg, &bindings, &one_second_pcm(), &[])
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
            status: MeetingStatus::Recording,
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
            error: None,
            title_source: TitleSource::Llm,
        };

        let notes = synthesize_meeting_notes(&reg, &bindings, &meeting, &[], &[], None)
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

        let title = synthesize_meeting_title(&reg, &bindings, "kickoff summary", None)
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
            pending_drains: Mutex::new(vec![
                mic_only_segment(one_second_pcm()),
                mic_only_segment(one_second_pcm()),
            ]),
            ..Default::default()
        };

        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
            ..MeetingSettings::default()
        };

        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
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
            ctx.engines,
            ctx.bindings,
            ctx.actions,
            ctx.meetings,
            &session,
            drain_result,
            &[],
        )
        .await
        .unwrap();

        assert_eq!(detail.segments.len(), 2);
        assert_eq!(detail.segments[0].text, "hello");
        assert_eq!(detail.segments[1].text, "world");
        assert_eq!(detail.meeting.title, "Sprint Planning");
        assert!(detail.notes.is_some());
        assert_eq!(detail.notes.as_ref().unwrap().summary, "kickoff summary");
        assert_eq!(detail.meeting.status, MeetingStatus::Completed);
        assert_eq!(detail.meeting.capture_mode, CaptureMode::MicOnly);

        let action_rows = actions.recent(1).await.unwrap();
        assert_eq!(action_rows.len(), 1);
        assert_eq!(action_rows[0].feature_id, "meetings");
        assert_eq!(action_rows[0].command, "toggle_meeting");
        assert_eq!(action_rows[0].engine_id, "fake-stt");
        assert_eq!(action_rows[0].status, ActionStatus::Ok);
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
                mic_only_segment(one_second_pcm()), // pop() returns this last
                mic_only_segment(one_second_pcm()), // pop() returns second
                mic_only_segment(one_second_pcm()), // pop() returns this first
            ]),
            ..Default::default()
        };

        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
            ..MeetingSettings::default()
        };

        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
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
            ctx.engines,
            ctx.bindings,
            ctx.actions,
            ctx.meetings,
            &session,
            drain_result,
            &[],
        )
        .await
        .unwrap();

        assert_eq!(
            detail.segments.len(),
            2,
            "should have polled segment + undelivered remainder only, no tail duplication"
        );
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
            pending_drains: Mutex::new(vec![mic_only_segment(one_second_pcm())]),
            ..Default::default()
        };

        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
            ..MeetingSettings::default()
        };

        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };

        let session = run_meeting_start(&mut ctx).await.unwrap();
        let drain_result = drain_and_stop_meeting(ctx.audio).await;
        let err = run_meeting_stop(
            ctx.engines,
            ctx.bindings,
            ctx.actions,
            ctx.meetings,
            &session,
            drain_result,
            &[],
        )
        .await
        .unwrap_err();
        assert!(err.contains("stt is down"), "unexpected error: {err}");

        // Capture must be released even though transcription failed.
        assert_eq!(audio.meeting_state(), MeetingState::Idle);

        // The action row must not be left pending.
        let detail = actions.get(session.action_id).await.unwrap().unwrap();
        assert_eq!(detail.status, ActionStatus::Error);
        assert!(detail.error.is_some());
    }

    /// Helper for the attribution tests: a meetings-bound registry plus repos.
    async fn attribution_world() -> (EngineRegistry, BindingRepo, ActionRepo, MeetingRepo) {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(CountingFakeStt {
            texts: vec!["mine".into(), "theirs".into(), "both at once".into()],
            call: AtomicUsize::new(0),
        }));
        reg.register_llm(Arc::new(FakeLlm));

        let (bindings, actions, meetings) = test_repos().await;
        for slot in ["stt", "llm"] {
            bindings
                .set(
                    "meetings",
                    slot,
                    Binding {
                        engine_id: if slot == "stt" {
                            "fake-stt"
                        } else {
                            "fake-llm"
                        }
                        .into(),
                        model: None,
                        provider_ref: None,
                    },
                )
                .await
                .unwrap();
        }
        (reg, bindings, actions, meetings)
    }

    /// End to end: two sources in, two different speaker keys out, and an
    /// ambiguous segment left unattributed rather than forced onto a side.
    #[tokio::test]
    async fn a_two_source_meeting_attributes_each_segment_to_a_channel() {
        let (reg, bindings, actions, meetings) = attribution_world().await;
        let mut audio = FakeMeetingAudioIo {
            capability: SystemAudioCapability::ScreenCaptureKit,
            pending_drains: Mutex::new(vec![
                // pop() drains from the back, so this is the third segment.
                two_source_segment(0.4, 0.4),
                two_source_segment(0.01, 0.5),
                two_source_segment(0.5, 0.01),
            ]),
            ..Default::default()
        };
        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: true,
            ..MeetingSettings::default()
        };
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };

        let session = run_meeting_start(&mut ctx).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;
        let mut keys = Vec::new();
        for _ in 0..3 {
            let ev =
                run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed)
                    .await
                    .unwrap()
                    .expect("a segment was queued");
            keys.push(ev.speaker_key);
        }
        assert_eq!(
            keys,
            vec![
                Some("local".to_string()),
                Some("remote".to_string()),
                Some("mixed".to_string()),
            ]
        );

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert_eq!(
            detail
                .segments
                .iter()
                .map(|s| s.speaker_key.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("local"), Some("remote"), Some("mixed")]
        );
        // Both sides exist to be named, and start with the honest defaults.
        assert_eq!(
            detail
                .speakers
                .iter()
                .map(|s| (s.speaker_key.as_str(), s.display_name.as_str()))
                .collect::<Vec<_>>(),
            vec![("local", "You"), ("remote", "Others")]
        );
    }

    /// A mic-only meeting has exactly one source. Every segment is the person
    /// sitting here, and there is no second speaker to invent.
    #[tokio::test]
    async fn a_mic_only_meeting_has_one_speaker_and_it_is_you() {
        let (reg, bindings, actions, meetings) = attribution_world().await;
        let mut audio = FakeMeetingAudioIo {
            capability: SystemAudioCapability::MicOnly,
            pending_drains: Mutex::new(vec![mic_only_segment(one_second_pcm())]),
            ..Default::default()
        };
        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: false,
            ..MeetingSettings::default()
        };
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };

        let session = run_meeting_start(&mut ctx).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;
        let ev = run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(ev.speaker_key.as_deref(), Some("local"));

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert_eq!(detail.speakers.len(), 1, "no second side to invent");
        assert_eq!(detail.speakers[0].display_name, "You");
    }

    /// The transcript handed to the notes model is the whole point of the
    /// feature: it must name both sides instead of one fictional "Speaker".
    #[tokio::test]
    async fn the_notes_prompt_sees_both_speakers_by_their_names() {
        let (reg, bindings, actions, meetings) = attribution_world().await;
        let mut audio = FakeMeetingAudioIo {
            capability: SystemAudioCapability::ScreenCaptureKit,
            pending_drains: Mutex::new(vec![
                two_source_segment(0.01, 0.5),
                two_source_segment(0.5, 0.01),
            ]),
            ..Default::default()
        };
        let settings = MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio: true,
            ..MeetingSettings::default()
        };
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let session = run_meeting_start(&mut ctx).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;
        for _ in 0..2 {
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed)
                .await
                .unwrap();
        }

        meetings
            .set_speaker_name(&session.meeting_id, "remote", "Priya")
            .await
            .unwrap();

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        let transcript = format_transcript_for_synthesis(&detail.segments, &detail.speakers);
        assert!(transcript.contains("You: mine"), "{transcript}");
        assert!(transcript.contains("Priya: theirs"), "{transcript}");
        assert!(!transcript.contains("Speaker:"), "{transcript}");
    }
}

#[cfg(test)]
mod item_16_17_tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::actions::ActionRepo;
    use kea_core::store::bindings::{Binding, BindingRepo};
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_core::store::meetings::{
        ActionItemStatus, MeetingRepo, MeetingStatus, NewMeeting, NewSegment,
    };
    use kea_engines::traits::{EngineCaps, EngineError, LlmEngine, LlmRequest, LlmResponse};
    use kea_engines::EngineRegistry;
    use std::sync::{Arc, Mutex};

    const VALID_NOTES: &str = r#"{"summary":"kickoff summary","decisions":"","action_items":"follow up","follow_ups":"","open_questions":""}"#;

    /// An LLM that records every prompt it is handed and replies from a
    /// script, falling back to valid notes / a title once the script runs out.
    ///
    /// The prompt log is what makes the privacy rule testable: a calendar
    /// title must never appear in anything this engine was asked.
    struct RecordingLlm {
        replies: Mutex<std::collections::VecDeque<String>>,
        prompts: Mutex<Vec<String>>,
        fail: bool,
    }

    impl RecordingLlm {
        fn new(replies: &[&str]) -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(replies.iter().map(|s| s.to_string()).collect()),
                prompts: Mutex::new(Vec::new()),
                fail: false,
            })
        }

        fn failing() -> Arc<Self> {
            Arc::new(Self {
                replies: Mutex::new(Default::default()),
                prompts: Mutex::new(Vec::new()),
                fail: true,
            })
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmEngine for RecordingLlm {
        fn id(&self) -> &str {
            "fake-llm"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
            self.prompts.lock().unwrap().push(req.prompt.clone());
            if self.fail {
                return Err(EngineError::Other("provider is down".into()));
            }
            if let Some(text) = self.replies.lock().unwrap().pop_front() {
                return Ok(LlmResponse::untracked(text));
            }
            if req.prompt.contains("<summary>") {
                return Ok(LlmResponse::untracked("Sprint Planning"));
            }
            Ok(LlmResponse::untracked(VALID_NOTES))
        }
    }

    /// A calendar that always offers the same title, and never anything else.
    struct FixedTitle(&'static str);

    impl MeetingTitleSource for FixedTitle {
        fn title_for(&self, _started_at: &str, _ended_at: Option<&str>) -> Option<String> {
            Some(self.0.to_string())
        }
    }

    /// Permission denied, EventKit error, empty calendar, no match — every one
    /// of them reaches the caller as this.
    struct NoTitle;

    impl MeetingTitleSource for NoTitle {
        fn title_for(&self, _started_at: &str, _ended_at: Option<&str>) -> Option<String> {
            None
        }
    }

    async fn world(
        llm: Arc<dyn LlmEngine>,
    ) -> (EngineRegistry, BindingRepo, ActionRepo, MeetingRepo) {
        let mut reg = EngineRegistry::default();
        reg.register_llm(llm);

        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();

        let bindings = BindingRepo::new(config_pool);
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

        (
            reg,
            bindings,
            ActionRepo::new(data_pool.clone()),
            MeetingRepo::new(data_pool),
        )
    }

    async fn recording_meeting(meetings: &MeetingRepo, id: &str, texts: &[&str]) {
        meetings
            .create(&NewMeeting {
                id: id.into(),
                title: "Untitled Meeting".into(),
                capture_mode: CaptureMode::MicOnly,
                stt_engine_id: None,
                llm_engine_id: None,
            })
            .await
            .unwrap();
        for (index, text) in texts.iter().enumerate() {
            meetings
                .append_segment(
                    id,
                    &NewSegment {
                        sequence: index as i32,
                        start_offset_ms: index as i64 * 30_000,
                        end_offset_ms: index as i64 * 30_000 + 30_000,
                        text: (*text).into(),
                        speaker: Some(SpeakerChannel::Local),
                    },
                )
                .await
                .unwrap();
        }
    }

    fn meeting_row(id: &str, started_at: &str) -> Meeting {
        Meeting {
            id: id.into(),
            title: "Untitled Meeting".into(),
            started_at: started_at.into(),
            ended_at: None,
            status: MeetingStatus::Recording,
            capture_mode: CaptureMode::MicOnly,
            stt_engine_id: None,
            llm_engine_id: None,
            error: None,
            title_source: TitleSource::Llm,
        }
    }

    // --- 16b: reliable JSON out of a provider that promises nothing ---

    #[tokio::test]
    async fn a_notes_reply_that_is_not_json_is_repaired_in_one_round_trip() {
        let llm = RecordingLlm::new(&["I'd be happy to help! Here are your notes:"]);
        let (reg, bindings, _, _) = world(llm.clone()).await;

        let notes = synthesize_meeting_notes(
            &reg,
            &bindings,
            &meeting_row("m1", "2026-09-19 10:00:00"),
            &[],
            &[],
            None,
        )
        .await
        .unwrap();

        assert_eq!(notes.summary, "kickoff summary");
        let prompts = llm.prompts();
        assert_eq!(prompts.len(), 2, "exactly one repair round trip");
        assert!(prompts[1].contains("was not valid JSON"));
        assert!(prompts[1].contains("I'd be happy to help!"));
    }

    /// The user already paid to transcribe this meeting. A provider that
    /// cannot produce JSON twice must not cost them the transcript.
    #[tokio::test]
    async fn two_unparsable_replies_keep_the_text_instead_of_failing_the_meeting() {
        let llm = RecordingLlm::new(&["no can do", "still no can do"]);
        let (reg, bindings, _, _) = world(llm.clone()).await;

        let notes = synthesize_meeting_notes(
            &reg,
            &bindings,
            &meeting_row("m1", "2026-09-19 10:00:00"),
            &[],
            &[],
            None,
        )
        .await
        .unwrap();

        assert_eq!(notes.summary, "still no can do");
        assert_eq!(notes.decisions, "");
        assert_eq!(llm.prompts().len(), 2, "one repair, never a loop");
    }

    #[tokio::test]
    async fn a_provider_that_errors_is_still_an_error() {
        let llm = RecordingLlm::failing();
        let (reg, bindings, _, _) = world(llm).await;
        assert!(synthesize_meeting_notes(
            &reg,
            &bindings,
            &meeting_row("m1", "2026-09-19 10:00:00"),
            &[],
            &[],
            None
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn structured_action_items_become_rows_and_the_prose_column_follows() {
        let llm = RecordingLlm::new(&[
            r#"{"summary":"s","decisions":"","action_items":"whatever the model wrote","follow_ups":"","open_questions":"","action_item_rows":[{"text":"Send the deck","owner":"Priya","due_hint":"by Friday"},{"text":"Book the room"}]}"#,
        ]);
        let (reg, bindings, _, meetings) = world(llm).await;
        recording_meeting(&meetings, "m1", &["hello"]).await;

        let detail = meetings.get("m1").await.unwrap().unwrap();
        let (parsed, binding) = synthesize_notes_parsed(
            &reg,
            &bindings,
            &detail.meeting,
            &detail.segments,
            &[],
            None,
        )
        .await
        .unwrap();
        let notes = persist_notes(
            &meetings,
            "m1",
            &parsed,
            MEETING_NOTES_PROMPT_VERSION,
            &binding,
        )
        .await
        .unwrap();

        let rows = meetings.action_items("m1").await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "Send the deck");
        assert_eq!(rows[0].owner.as_deref(), Some("Priya"));
        assert_eq!(rows[0].status, ActionItemStatus::Open);
        // The column is a derived view of the rows, not what the model wrote.
        assert_eq!(
            notes.action_items,
            "Send the deck — Priya (by Friday)\nBook the room"
        );
    }

    // --- 16a: interim passes ---

    #[tokio::test]
    async fn an_interim_pass_folds_only_the_new_segments() {
        let llm = RecordingLlm::new(&[
            r#"{"summary":"first half","decisions":"","action_items":"","follow_ups":"","open_questions":""}"#,
            r#"{"summary":"first and second half","decisions":"","action_items":"","follow_ups":"","open_questions":""}"#,
        ]);
        let (reg, bindings, _, meetings) = world(llm.clone()).await;
        recording_meeting(&meetings, "m1", &["opening remarks"]).await;

        let first = run_interim_notes_pass(&reg, &bindings, &meetings, "m1", 0, None)
            .await
            .unwrap();
        assert_eq!(first.next_sequence, 1);
        assert_eq!(
            first.notes.as_ref().unwrap().prompt_version,
            MEETING_INTERIM_PROMPT_VERSION
        );

        meetings
            .append_segment(
                "m1",
                &NewSegment {
                    sequence: 1,
                    start_offset_ms: 30_000,
                    end_offset_ms: 60_000,
                    text: "closing remarks".into(),
                    speaker: Some(SpeakerChannel::Local),
                },
            )
            .await
            .unwrap();

        let second =
            run_interim_notes_pass(&reg, &bindings, &meetings, "m1", first.next_sequence, None)
                .await
                .unwrap();
        assert_eq!(second.next_sequence, 2);
        assert_eq!(
            second.notes.as_ref().unwrap().summary,
            "first and second half"
        );

        let prompts = llm.prompts();
        assert_eq!(prompts.len(), 2);
        // The cost argument, asserted: the second pass re-sends the previous
        // notes, never the segments the first pass already folded in.
        assert!(prompts[1].contains("closing remarks"));
        assert!(!prompts[1].contains("opening remarks"));
        assert!(prompts[1].contains("first half"));
    }

    /// Nothing new means nothing to pay for.
    #[tokio::test]
    async fn an_interim_pass_with_no_new_segments_asks_nothing() {
        let llm = RecordingLlm::new(&[]);
        let (reg, bindings, _, meetings) = world(llm.clone()).await;
        recording_meeting(&meetings, "m1", &["only segment"]).await;

        let pass = run_interim_notes_pass(&reg, &bindings, &meetings, "m1", 1, None)
            .await
            .unwrap();
        assert_eq!(pass.next_sequence, 1);
        assert!(llm.prompts().is_empty());
    }

    /// The inverse of `stop_with_failing_stt_still_stops_capture_and_finalizes_action`:
    /// a failed *interim* pass must leave the meeting recording and the ledger
    /// row open. It has no `ActiveMeeting`, so it cannot call `fail` at all.
    #[tokio::test]
    async fn an_interim_failure_leaves_the_meeting_recording_and_the_action_row_open() {
        let llm = RecordingLlm::failing();
        let (reg, bindings, actions, meetings) = world(llm).await;
        recording_meeting(&meetings, "m1", &["hello"]).await;
        let action_id = actions
            .record(NewAction {
                feature_id: "meetings".into(),
                command: "toggle_meeting".into(),
                engine_id: "fake-stt".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();

        assert!(
            run_interim_notes_pass(&reg, &bindings, &meetings, "m1", 0, None)
                .await
                .is_err()
        );

        let detail = meetings.get("m1").await.unwrap().unwrap();
        assert_eq!(detail.meeting.status, MeetingStatus::Recording);
        assert!(detail.meeting.error.is_none());
        assert!(detail.notes.is_none());

        let row = actions.get(action_id).await.unwrap().unwrap();
        assert_eq!(row.status, ActionStatus::Started);
    }

    /// An interim pass never renames the meeting: it can run long before the
    /// title step, and after item 17 the title may be a calendar title that
    /// must not reach an engine.
    #[tokio::test]
    async fn an_interim_pass_never_touches_the_title() {
        let llm = RecordingLlm::new(&[]);
        let (reg, bindings, _, meetings) = world(llm.clone()).await;
        recording_meeting(&meetings, "m1", &["hello"]).await;
        meetings
            .set_title_with_source("m1", "Q3 Roadmap Review", TitleSource::Calendar)
            .await
            .unwrap();

        run_interim_notes_pass(&reg, &bindings, &meetings, "m1", 0, None)
            .await
            .unwrap();

        let detail = meetings.get("m1").await.unwrap().unwrap();
        assert_eq!(detail.meeting.title, "Q3 Roadmap Review");
        assert_eq!(detail.meeting.title_source, TitleSource::Calendar);
        for prompt in llm.prompts() {
            assert!(!prompt.contains("Q3 Roadmap Review"));
        }
    }

    #[test]
    fn the_schedule_waits_a_window_then_fires_and_resets() {
        let mut schedule = InterimSchedule::new(InterimCadence {
            every_segments: 2,
            every_minutes: 5,
            max_passes: 2,
        });
        assert_eq!(schedule.next_sequence(), 0);
        assert!(!schedule.is_due(), "nothing recorded yet");

        schedule.note_segment();
        schedule.note_segment();
        // The 90-second floor has not passed, so the segment trigger waits.
        assert!(!schedule.is_due());

        schedule.record_success(2);
        assert_eq!(schedule.next_sequence(), 2);
        assert!(!schedule.is_due());
    }

    /// A pass that failed did not fold its segments in, so the counter must
    /// not be reset — those segments still need to reach the next pass.
    #[tokio::test]
    async fn consecutive_failures_disable_the_schedule_for_the_rest_of_the_meeting() {
        let mut schedule = InterimSchedule::new(InterimCadence::default());
        schedule.note_segment();
        for _ in 0..MAX_CONSECUTIVE_INTERIM_FAILURES {
            assert!(!schedule.is_disabled());
            schedule.record_failure();
        }
        assert!(schedule.is_disabled());
        assert!(!schedule.is_due());

        // …and a success in between clears the streak.
        let mut schedule = InterimSchedule::new(InterimCadence::default());
        schedule.record_failure();
        schedule.record_success(1);
        schedule.record_failure();
        assert!(!schedule.is_disabled());
    }

    // --- 17: calendar titles ---

    async fn stop_with(
        opts: MeetingStopOptions,
        llm: Arc<RecordingLlm>,
    ) -> (MeetingDetail, Arc<RecordingLlm>) {
        let (reg, bindings, actions, meetings) = world(llm.clone()).await;
        recording_meeting(&meetings, "m1", &["hello", "world"]).await;
        let action_id = actions
            .record(NewAction {
                feature_id: "meetings".into(),
                command: "toggle_meeting".into(),
                engine_id: "fake-stt".into(),
                model: None,
                provider_ref: None,
            })
            .await
            .unwrap();
        let session = ActiveMeeting {
            meeting_id: "m1".into(),
            action_id,
        };

        // An empty tail: the drain contributed no audio, which is the normal
        // case for a meeting that ended on a segment boundary.
        let drain = Ok(SpeechSegment {
            pcm: PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            },
            has_speech: false,
            mic: None,
            system: None,
        });

        let detail = run_meeting_stop_with(
            &reg,
            &bindings,
            &actions,
            &meetings,
            &session,
            drain,
            &[],
            opts,
        )
        .await
        .unwrap();
        (detail, llm)
    }

    #[tokio::test]
    async fn a_matched_calendar_event_names_the_meeting_and_skips_the_title_call() {
        let (detail, llm) = stop_with(
            MeetingStopOptions {
                calendar: Some(Arc::new(FixedTitle("Q3 Roadmap Review"))),
                calendar_titles: true,
                ..MeetingStopOptions::default()
            },
            RecordingLlm::new(&[]),
        )
        .await;

        assert_eq!(detail.meeting.title, "Q3 Roadmap Review");
        assert_eq!(detail.meeting.title_source, TitleSource::Calendar);
        assert_eq!(detail.meeting.status, MeetingStatus::Completed);
        // Skipping the title call removes a network round trip from the stop
        // path, which is the slowest part of the app.
        assert_eq!(llm.prompts().len(), 1, "notes only, no title call");
    }

    /// The leak test. `build_meeting_notes_request` embeds the meeting title
    /// verbatim, so applying a calendar title before synthesis would transmit
    /// it to whatever hosted provider is bound.
    #[tokio::test]
    async fn a_calendar_title_never_reaches_a_prompt() {
        let (_detail, llm) = stop_with(
            MeetingStopOptions {
                calendar: Some(Arc::new(FixedTitle("1:1 — performance review"))),
                calendar_titles: true,
                ..MeetingStopOptions::default()
            },
            RecordingLlm::new(&[]),
        )
        .await;

        for prompt in llm.prompts() {
            assert!(
                !prompt.contains("performance review"),
                "calendar title leaked into a prompt: {prompt}"
            );
        }
    }

    /// The contract: with the feature off, with no match, and by default, a
    /// stop produces exactly what it produced before item 17 existed.
    #[tokio::test]
    async fn every_failure_mode_degrades_to_todays_behaviour() {
        for opts in [
            // Feature off, calendar present — the lookup never happens.
            MeetingStopOptions {
                calendar: Some(Arc::new(FixedTitle("Q3 Roadmap Review"))),
                calendar_titles: false,
                ..MeetingStopOptions::default()
            },
            // Feature on, but permission denied / no match / EventKit error.
            MeetingStopOptions {
                calendar: Some(Arc::new(NoTitle)),
                calendar_titles: true,
                ..MeetingStopOptions::default()
            },
            // No calendar on this build at all.
            MeetingStopOptions {
                calendar: None,
                calendar_titles: true,
                ..MeetingStopOptions::default()
            },
            MeetingStopOptions::default(),
        ] {
            let (detail, llm) = stop_with(opts, RecordingLlm::new(&[])).await;
            assert_eq!(detail.meeting.title, "Sprint Planning");
            assert_eq!(detail.meeting.title_source, TitleSource::Llm);
            assert_eq!(detail.meeting.status, MeetingStatus::Completed);
            assert_eq!(detail.segments.len(), 2);
            assert_eq!(detail.notes.as_ref().unwrap().summary, "kickoff summary");
            assert_eq!(llm.prompts().len(), 2, "notes and title, as today");
        }
    }

    /// The stop path must survive a calendar implementation that panics rather
    /// than turning it into a failed meeting.
    #[tokio::test]
    async fn a_panicking_calendar_falls_back_instead_of_failing_the_meeting() {
        struct Panics;
        impl MeetingTitleSource for Panics {
            fn title_for(&self, _: &str, _: Option<&str>) -> Option<String> {
                panic!("EventKit blew up");
            }
        }

        let (detail, _) = stop_with(
            MeetingStopOptions {
                calendar: Some(Arc::new(Panics)),
                calendar_titles: true,
                ..MeetingStopOptions::default()
            },
            RecordingLlm::new(&[]),
        )
        .await;
        assert_eq!(detail.meeting.title, "Sprint Planning");
        assert_eq!(detail.meeting.status, MeetingStatus::Completed);
    }
}
