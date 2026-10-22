use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kea_core::dictation::{apply_vocabulary, hint_terms};
use kea_core::meetings::{
    attribute_segment, build_chat_request, build_interim_notes_request,
    build_meeting_notes_request, build_meeting_title_request, build_notes_repair_request,
    build_speaker_names_request, embedding_windows, format_transcript_for_synthesis,
    parse_chat_reply, parse_meeting_notes_json, parse_speaker_names, person_display_name,
    render_action_items_prose, sanitize_meeting_title, should_run_interim, ChatRole, ChatTurn,
    InterimCadence, MeetingSettings, NotesProposal, ParsedMeetingNotes, SpeakerChannel,
    SpeakerNameProposal, VoiceClusters, MAX_CONSECUTIVE_INTERIM_FAILURES,
    MEETING_INTERIM_PROMPT_VERSION, MEETING_NOTES_PROMPT_VERSION, MIN_WINDOWS,
};
use kea_core::resolve::SlotResolver;
use kea_core::store::actions::{ActionRepo, ActionStatus, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::conversations::MessageRole;
use kea_core::store::meetings::{
    ActionItem, ActionItemStatus, CaptureMode, Meeting, MeetingChatMessage, MeetingDetail,
    MeetingNotes, MeetingRepo, MeetingSegment, MeetingSpeaker, MeetingStatus, NewActionItem,
    NewMeeting, NewSegment, NotesText, ProposalMeta, ProposalState, SegmentRelabel, SpeakerSource,
    TitleSource,
};
use kea_core::store::usage::{NewUsageEvent, UsageRepo};
use kea_core::store::vocabulary::VocabularyEntry;
use kea_core::transcript::speakers::speaker_key;
use kea_engines::traits::{AudioPcm, LlmEngine, SttOpts, Transcript};
use kea_engines::EngineRegistry;
use kea_infer::{SpeakerEmbedder, SPEAKER_EMBEDDING_SAMPLE_RATE_HZ};
use kea_platform::audio::util::resample_linear;
use kea_platform::audio::SpeechSegment;
use kea_platform::{AudioIo, PcmFrame, SystemAudioCapability};
use serde::Serialize;

use crate::feature::{CapKind, CapSlot, Command, Feature};

const WHISPER_SAMPLE_RATE_HZ: u32 = 16_000;
const MIN_SEGMENT_SECS: u32 = 1;

/// Voice separation for one meeting: the online participant clusters, and the
/// embedder that turns a segment's audio into the vectors they are built from.
///
/// Owned by the caller rather than by this crate, so its lifetime is the
/// meeting's. The app holds it on the active session, and dropping the session
/// drops every embedding with it; nothing here writes to disk.
pub struct MeetingVoices {
    clusters: VoiceClusters,
    embedder: Arc<dyn SpeakerEmbedder>,
    /// Participants whose row already exists. A row is written when a voice
    /// first appears, not once per segment.
    announced: BTreeSet<usize>,
    /// Whether an embedder failure has been logged yet. A runtime that fails
    /// once tends to fail on every window, so one warning per meeting says it.
    warned: bool,
}

impl MeetingVoices {
    pub fn new(embedder: Arc<dyn SpeakerEmbedder>) -> Self {
        Self {
            clusters: VoiceClusters::new(),
            embedder,
            announced: BTreeSet::new(),
            warned: false,
        }
    }

    /// Embeds a segment's audio and assigns it a participant. `None` means the
    /// segment has too little speech to tell voices apart, and nothing is
    /// recorded for it.
    ///
    /// Resampled here rather than by the caller: the embedder is pinned to one
    /// rate, and the rate of the audio is a detail of the capture device.
    async fn assign(&mut self, sequence: i32, audio: &PcmFrame) -> Result<Option<usize>, String> {
        let resampled;
        let samples: &[f32] = if audio.sample_rate_hz == SPEAKER_EMBEDDING_SAMPLE_RATE_HZ {
            &audio.samples
        } else {
            resampled = resample_linear(audio, SPEAKER_EMBEDDING_SAMPLE_RATE_HZ);
            &resampled.samples
        };

        // Checked before embedding: a segment that cannot reach the minimum
        // would be embedded for nothing, and would spend the embedder's
        // scripted or metered calls on audio that is then thrown away.
        let ranges = embedding_windows(samples, SPEAKER_EMBEDDING_SAMPLE_RATE_HZ);
        if ranges.len() < MIN_WINDOWS {
            return Ok(None);
        }
        let windows: Vec<Vec<f32>> = ranges.into_iter().map(|r| samples[r].to_vec()).collect();
        let embeddings = self
            .embedder
            .embed_batch(windows, SPEAKER_EMBEDDING_SAMPLE_RATE_HZ)
            .await
            .map_err(|e| e.to_string())?;
        Ok(self.clusters.assign(sequence, embeddings))
    }

    fn log_failure_once(&mut self, error: &str) {
        if !self.warned {
            self.warned = true;
            tracing::warn!(
                error,
                "meeting: voice separation failed for a segment; keeping channel labels"
            );
        }
    }
}

/// The audio whose voice labels a segment, or `None` when the segment keeps its
/// channel label and is not embedded at all.
///
/// Two sources: only the remote half is embedded. The local half is the person
/// sitting here, and embedding the mixed signal would let their voice and any
/// echo into the remote clusters. One source (mic-only): the signal is all
/// there is, so it is embedded whatever the channel decision says.
fn voice_audio(segment: &SpeechSegment, channel: SpeakerChannel) -> Option<&PcmFrame> {
    match (&segment.mic, &segment.system) {
        (Some(_), Some(system)) => (channel == SpeakerChannel::Remote).then_some(system),
        _ => Some(&segment.pcm),
    }
}

/// The key a segment is stored under: the channel key, or a participant key
/// when voice separation names the segment's voice.
///
/// Every failure returns the channel key — or, in a mic-only meeting, the
/// unattributed key, since there the channel says nothing about who spoke. The
/// voice is an improvement on the label, never a condition for the segment,
/// so nothing here may fail the caller. An unassigned remote segment also brings the "Others" row into being
/// here, since in a two-source meeting with voices the row is not seeded at
/// start, and a segment labelled "Others" with no row would be a lie.
async fn speaker_key_for_segment(
    voices: Option<&mut MeetingVoices>,
    meetings: &MeetingRepo,
    meeting_id: &str,
    segment: &SpeechSegment,
    channel: SpeakerChannel,
    sequence: i32,
) -> String {
    let channel_key = channel.as_str().to_string();
    let Some(voices) = voices else {
        return channel_key;
    };
    let Some(audio) = voice_audio(segment, channel) else {
        return channel_key;
    };

    let assigned = match voices.assign(sequence, audio).await {
        Ok(assigned) => assigned,
        Err(e) => {
            voices.log_failure_once(&e);
            None
        }
    };
    let Some(index) = assigned else {
        // A mic-only segment the voices could not place is not "You": the mic
        // is shared by the whole room. It is stored as unattributed instead.
        let fallback = if segment.system.is_none() {
            SpeakerChannel::Mixed
        } else {
            channel
        };
        if fallback != SpeakerChannel::Local {
            if let Err(e) = meetings
                .ensure_speaker(meeting_id, fallback, fallback.display_name())
                .await
            {
                tracing::warn!(error = %e, meeting_id, "meeting: failed to seed a fallback speaker row");
            }
        }
        return fallback.as_str().to_string();
    };

    let index_u32 = index as u32;
    if !voices.announced.contains(&index) {
        if let Err(e) = meetings
            .ensure_participant(
                meeting_id,
                &speaker_key(index_u32),
                &person_display_name(index_u32),
            )
            .await
        {
            tracing::warn!(error = %e, meeting_id, "meeting: failed to create a participant row");
            return channel_key;
        }
        voices.announced.insert(index);
    }
    speaker_key(index_u32)
}

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

/// The sides a meeting starts with, and what they are called before anyone
/// renames them. A mic-only meeting has exactly one.
///
/// With voice separation the voices are the participants, so nothing is
/// assumed about them up front. A mic-only meeting gets no "You": the person
/// at the microphone is not the only voice in the room, and labelling any
/// segment "You" by assumption is the error this feature exists to avoid. The
/// "Others" row of a two-source meeting waits until a remote segment is left
/// unassigned, for the same reason.
fn default_speakers(mode: CaptureMode, voice_separation: bool) -> &'static [SpeakerChannel] {
    match (mode, voice_separation) {
        (CaptureMode::MicOnly, false) => &[SpeakerChannel::Local],
        (CaptureMode::MicOnly, true) => &[],
        (CaptureMode::MicAndSystem, false) => &[SpeakerChannel::Local, SpeakerChannel::Remote],
        (CaptureMode::MicAndSystem, true) => &[SpeakerChannel::Local],
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
/// `source_seq` is the last segment sequence the pass had read; it is stamped
/// on any item the pass adds, and is what marks a row as the model's to
/// reconcile on a later pass rather than the user's.
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
    source_seq: i64,
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
        .reconcile_action_items(meeting_id, &incoming, source_seq)
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
        edited: false,
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
        edited: false,
    })
}

/// What one Ask AI turn stored, as the tab renders it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MeetingChatReply {
    /// The assistant message just stored, with its proposal when it has one.
    pub message: MeetingChatMessage,
    /// Whether the transcript sent with this turn was cut at the context cap,
    /// so the tab can say the answer rests on the start of a long meeting.
    pub truncated: bool,
}

const CHAT_TURN_BUSY: &str = "a reply is already being written for this meeting";

/// Which meetings have a chat turn in flight.
///
/// A turn is a full LLM call the user waits on. Two at once for one meeting
/// would both read the same history, answer the same question and store two
/// replies, so the second is refused here. The UI also disables Send while a
/// turn is pending, but this is the check that holds when a stale window or a
/// double press gets through. Held once in app state, so it covers every window.
#[derive(Debug, Default)]
pub struct MeetingChatTurns {
    in_flight: std::sync::Mutex<BTreeSet<String>>,
}

impl MeetingChatTurns {
    /// Whether a turn is running for `meeting_id`. Used to refuse a chat clear
    /// that would otherwise land under a reply still being written.
    pub fn is_in_flight(&self, meeting_id: &str) -> bool {
        self.set().contains(meeting_id)
    }

    fn set(&self) -> std::sync::MutexGuard<'_, BTreeSet<String>> {
        // A panic while the lock was held leaves a set of ids, which is still
        // a correct answer to "is this meeting busy", so poisoning is ignored.
        self.in_flight
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Takes the meeting's slot, or refuses if a turn holds it. Public so a
    /// chat clear can hold the same slot for its delete: checking "is a turn
    /// running" and then deleting left a window in which a turn could start
    /// and its reply land in the freshly cleared chat.
    pub fn claim(&self, meeting_id: &str) -> Result<ChatTurnClaim<'_>, String> {
        if !self.set().insert(meeting_id.to_string()) {
            return Err(CHAT_TURN_BUSY.into());
        }
        Ok(ChatTurnClaim {
            turns: self,
            meeting_id: meeting_id.to_string(),
        })
    }
}

/// Holds a meeting's turn slot and gives it back on Drop, so an error or a
/// dropped future cannot leave the meeting marked busy for good.
pub struct ChatTurnClaim<'a> {
    turns: &'a MeetingChatTurns,
    meeting_id: String,
}

impl Drop for ChatTurnClaim<'_> {
    fn drop(&mut self) {
        self.turns.set().remove(&self.meeting_id);
    }
}

/// The notes a meeting with none yet is chatted against: empty sections, so
/// the prompt reads the same either way.
fn empty_notes(meeting_id: &str) -> MeetingNotes {
    MeetingNotes {
        meeting_id: meeting_id.to_string(),
        summary: String::new(),
        decisions: String::new(),
        action_items: String::new(),
        follow_ups: String::new(),
        open_questions: String::new(),
        prompt_version: String::new(),
        engine_id: None,
        model: None,
        edited: false,
    }
}

/// The notes as they would read with `proposal` applied. Stored whole with
/// the message, so the card can show before and after and Apply does not
/// depend on the notes still reading the same when the user presses it.
fn proposed_notes(current: &MeetingNotes, proposal: &NotesProposal) -> NotesText {
    NotesText {
        summary: proposal
            .summary
            .clone()
            .unwrap_or_else(|| current.summary.clone()),
        decisions: proposal
            .decisions
            .clone()
            .unwrap_or_else(|| current.decisions.clone()),
        action_items: current.action_items.clone(),
        follow_ups: proposal
            .follow_ups
            .clone()
            .unwrap_or_else(|| current.follow_ups.clone()),
        open_questions: proposal
            .open_questions
            .clone()
            .unwrap_or_else(|| current.open_questions.clone()),
    }
}

/// The sections a proposal changes, by their stored names. See
/// `ProposalMeta::changed` for why Apply needs them.
fn changed_section_names(proposal: &NotesProposal) -> Vec<String> {
    [
        ("summary", proposal.summary.is_some()),
        ("decisions", proposal.decisions.is_some()),
        ("follow_ups", proposal.follow_ups.is_some()),
        ("open_questions", proposal.open_questions.is_some()),
    ]
    .into_iter()
    .filter(|(_, changed)| *changed)
    .map(|(name, _)| name.to_string())
    .collect()
}

/// Run one Ask AI turn for `meeting_id` and store the exchange.
///
/// The reply is computed before anything is written, so a provider failure
/// stores nothing and a retry sends the same history again. Only then are the
/// user's message and the reply appended, in that order.
///
/// `allow_edits` is `status != Recording`. A live meeting's notes are rewritten
/// by the interim and final passes, so a proposal made now would be stale by
/// the time it was applied, and the spec rules it out. That is enforced twice:
/// the prompt leaves the edit instructions out, and [`parse_chat_reply`] drops
/// any proposal the model sends anyway. The second is the one that holds.
///
/// Usage is recorded once per reply, as the other meeting calls are, even when
/// the reply turns out to be prose with no proposal.
pub async fn run_chat_turn(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meetings: &MeetingRepo,
    turns: &MeetingChatTurns,
    usage: Option<&UsageRepo>,
    meeting_id: &str,
    message: &str,
) -> Result<MeetingChatReply, String> {
    let message = message.trim();
    if message.is_empty() {
        return Err("type a question first".into());
    }
    let _claim = turns.claim(meeting_id)?;

    let detail = meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))?;
    let allow_edits = detail.meeting.status != MeetingStatus::Recording;
    let notes = detail
        .notes
        .clone()
        .unwrap_or_else(|| empty_notes(meeting_id));

    let history: Vec<ChatTurn> = meetings
        .chat_messages(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|stored| ChatTurn {
            role: match stored.role {
                MessageRole::User => ChatRole::User,
                MessageRole::Assistant => ChatRole::Assistant,
            },
            content: stored.content,
        })
        .collect();

    let (mut req, truncated) = build_chat_request(
        &notes,
        &detail.action_items,
        &detail.segments,
        &detail.speakers,
        &history,
        message,
        allow_edits,
    );
    let (engine, binding) = meetings_llm(engines, bindings).await?;
    req.model = binding.model.clone();
    req.provider_ref = binding.provider_ref.clone();
    let reply = engine.complete(req).await.map_err(|e| e.to_string())?;
    record_meeting_usage(usage, &binding, reply.usage).await;

    let parsed = parse_chat_reply(&reply.text, &notes, allow_edits);
    let proposal = parsed.proposal.as_ref().map(|changes| ProposalMeta {
        proposal: proposed_notes(&notes, changes),
        state: ProposalState::Pending,
        changed: changed_section_names(changes),
    });

    let id = meetings
        .append_chat_exchange(meeting_id, message, &parsed.reply, proposal.as_ref())
        .await
        .map_err(|e| e.to_string())?;

    let stored = meetings
        .chat_messages(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .find(|stored| stored.id == id)
        .ok_or_else(|| "the reply was not stored".to_string())?;
    Ok(MeetingChatReply {
        message: stored,
        truncated,
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
            // The rows, not just the prose column rendered from them. Given
            // only prose ("Send the deck — Priya (by Friday)") the model
            // re-derives every item each pass, and each rewording used to
            // land as another row.
            action_item_rows: detail
                .action_items
                .iter()
                .filter(|row| row.status == ActionItemStatus::Open)
                .map(|row| kea_core::meetings::ParsedActionItem {
                    text: row.text.clone(),
                    owner: row.owner.clone(),
                    due_hint: row.due_hint.clone(),
                })
                .collect(),
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
        (next_sequence - 1) as i64,
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
    speaker_key: Option<String>,
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
                speaker_key: speaker_key.clone(),
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
        speaker_key,
    })
}

/// Starts a meeting. `voice_separation` is whether voices will be separated for
/// this meeting, decided by the caller: the setting alone is not enough, because
/// the models may be missing or fail to load.
pub async fn run_meeting_start(
    ctx: &mut MeetingRunContext<'_>,
    voice_separation: bool,
) -> Result<ActiveMeeting, String> {
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
    for channel in default_speakers(capture_mode, voice_separation) {
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

/// Cuts, attributes and transcribes one segment, if one is ready.
///
/// `voices` is `Some` when voice separation is active for this meeting. The
/// embedder is consulted before transcription, because the key it decides is
/// stored with the segment.
pub async fn run_meeting_poll_segment(
    ctx: &mut MeetingRunContext<'_>,
    meeting_id: &str,
    sequence: &mut i32,
    elapsed_ms: &mut i64,
    voices: Option<&mut MeetingVoices>,
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
    let mut voices = voices;
    let speaker_key = match segment_speaker(&segment) {
        Some(channel) => Some(
            speaker_key_for_segment(
                voices.as_deref_mut(),
                ctx.meetings,
                meeting_id,
                &segment,
                channel,
                *sequence,
            )
            .await,
        ),
        None => None,
    };

    let event = match append_transcribed_segment(
        ctx.engines,
        ctx.bindings,
        ctx.meetings,
        meeting_id,
        segment.pcm,
        speaker_key,
        *sequence,
        *elapsed_ms,
        ctx.vocabulary,
    )
    .await
    {
        Ok(event) => event,
        Err(e) => {
            // The segment was given a voice but never stored, and the next one
            // reuses its sequence number; its windows must not vote with it.
            if let Some(voices) = voices {
                voices.clusters.forget(*sequence);
            }
            return Err(e);
        }
    };
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
    voices: Option<&mut MeetingVoices>,
) -> Result<MeetingDetail, String> {
    run_meeting_stop_with(
        engines,
        bindings,
        actions,
        meetings,
        session,
        drain_result,
        vocabulary,
        voices,
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
    voices: Option<&mut MeetingVoices>,
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
        voices,
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
    mut voices: Option<&mut MeetingVoices>,
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
        let speaker_key = match segment_speaker(&tail) {
            Some(channel) => Some(
                speaker_key_for_segment(
                    voices.as_deref_mut(),
                    meetings,
                    meeting_id,
                    &tail,
                    channel,
                    sequence,
                )
                .await,
            ),
            None => None,
        };
        append_transcribed_segment(
            engines,
            bindings,
            meetings,
            meeting_id,
            tail.pcm,
            speaker_key,
            sequence,
            elapsed_ms,
            vocabulary,
        )
        .await?;
    }

    let mut partial = reload_meeting(meetings, meeting_id).await?;

    // The stop-time pass runs before anything reads the labels: the names
    // request and the notes both see the participants as they will stay.
    if let Some(voices) = voices.as_deref() {
        recluster_speakers(meetings, meeting_id, &partial.segments, voices).await;
        partial = reload_meeting(meetings, meeting_id).await?;
    }

    // Names are inferred before the notes so the notes are written against
    // them. Any failure inside is logged and skipped; the notes do not wait.
    if infer_speaker_names(
        engines,
        bindings,
        meetings,
        meeting_id,
        &partial,
        opts.usage.as_ref(),
    )
    .await
    {
        partial = reload_meeting(meetings, meeting_id).await?;
    }

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
        partial
            .segments
            .iter()
            .map(|s| s.sequence as i64)
            .max()
            .unwrap_or(0),
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

/// Reads the meeting back in full. The stop reads it after each write that
/// changes what the next step must see, so the notes never work from labels
/// that have since been rewritten.
async fn reload_meeting(meetings: &MeetingRepo, meeting_id: &str) -> Result<MeetingDetail, String> {
    meetings
        .get(meeting_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("meeting {meeting_id} not found"))
}

/// The stop-time voice pass: every segment is relabelled from the whole
/// meeting's voice evidence at once, and the new labels get their names.
///
/// A failure is logged and the live labels stand. The rewrite is a single
/// transaction, so a failure leaves the rows exactly as they were.
async fn recluster_speakers(
    meetings: &MeetingRepo,
    meeting_id: &str,
    segments: &[MeetingSegment],
    voices: &MeetingVoices,
) {
    let live: Vec<(i32, Option<String>)> = segments
        .iter()
        .map(|s| (s.sequence, s.speaker_key.clone()))
        .collect();
    let result = voices.clusters.recluster(&live);

    // Only the segments whose key actually changes are written.
    let current: BTreeMap<i32, Option<&str>> = segments
        .iter()
        .map(|s| (s.sequence, s.speaker_key.as_deref()))
        .collect();
    let relabels: Vec<SegmentRelabel> = result
        .segments
        .into_iter()
        .filter(|(sequence, key)| current.get(sequence).copied().flatten() != Some(key.as_str()))
        .map(|(sequence, speaker_key)| SegmentRelabel {
            sequence,
            speaker_key,
        })
        .collect();
    // No early return when nothing moved: the names can still need
    // renumbering, and the store leaves typed names alone either way.
    if result.names.is_empty() {
        return;
    }

    if let Err(e) = meetings
        .remap_segment_speakers(meeting_id, &relabels, &result.names)
        .await
    {
        tracing::warn!(
            error = %e,
            meeting_id,
            "meeting: stop-time voice pass could not be applied; keeping live labels"
        );
    }
}

/// Asks the meeting's LLM who the "Person N" participants are, and applies the
/// names the transcript backs.
///
/// Returns whether any name was written, so the caller knows to reload. A
/// request that errors, a reply that will not parse, and every name that does
/// not verify all leave the labels as they are. Nothing here can fail the stop.
async fn infer_speaker_names(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    meetings: &MeetingRepo,
    meeting_id: &str,
    detail: &MeetingDetail,
    usage: Option<&UsageRepo>,
) -> bool {
    // Only a row still on its default label is a candidate. A name the user
    // typed, or one already inferred, is never asked about again.
    let unnamed: Vec<&MeetingSpeaker> = detail
        .speakers
        .iter()
        .filter(|s| s.source == SpeakerSource::Channel && s.display_name.starts_with("Person "))
        .collect();
    if unnamed.is_empty() {
        return false;
    }

    let labels: Vec<&str> = unnamed.iter().map(|s| s.display_name.as_str()).collect();
    let transcript = format_transcript_for_synthesis(&detail.segments, &detail.speakers);
    let proposals =
        match request_speaker_names(engines, bindings, &transcript, &labels, usage).await {
            Ok(proposals) => proposals,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    meeting_id,
                    "meeting: speaker name inference failed; keeping Person labels"
                );
                return false;
            }
        };

    let mut applied = false;
    for proposal in proposals {
        let Some(row) = unnamed.iter().find(|s| s.display_name == proposal.speaker) else {
            continue;
        };
        match meetings
            .set_inferred_speaker_name(meeting_id, &row.speaker_key, &proposal.name)
            .await
        {
            Ok(()) => applied = true,
            Err(e) => tracing::warn!(
                error = %e,
                meeting_id,
                "meeting: could not record an inferred speaker name"
            ),
        }
    }
    applied
}

/// The one names request, and the only place its usage is counted.
///
/// The verified proposals are returned, not the raw reply: validation against
/// the transcript is part of the call's contract, not something each caller
/// has to remember to do.
async fn request_speaker_names(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    transcript: &str,
    labels: &[&str],
    usage: Option<&UsageRepo>,
) -> Result<Vec<SpeakerNameProposal>, String> {
    let (engine, binding) = meetings_llm(engines, bindings).await?;
    let mut req = build_speaker_names_request(transcript, labels);
    req.model = binding.model.clone();
    req.provider_ref = binding.provider_ref.clone();
    let resp = engine.complete(req).await.map_err(|e| e.to_string())?;
    record_meeting_usage(usage, &binding, resp.usage).await;
    Ok(parse_speaker_names(&resp.text, transcript, labels))
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
    use kea_infer::ScriptedEmbedder;
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

        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;

        let ev1 =
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed, None)
                .await
                .unwrap();
        assert!(ev1.is_some());
        assert_eq!(ev1.unwrap().text, "hello");

        let ev2 =
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed, None)
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
            None,
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

        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;

        // Poll only once — two frames remain undrained
        let ev =
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed, None)
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
            None,
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

        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let drain_result = drain_and_stop_meeting(ctx.audio).await;
        let err = run_meeting_stop(
            ctx.engines,
            ctx.bindings,
            ctx.actions,
            ctx.meetings,
            &session,
            drain_result,
            &[],
            None,
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

        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;
        let mut keys = Vec::new();
        for _ in 0..3 {
            let ev = run_meeting_poll_segment(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                None,
            )
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

        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;
        let ev =
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed, None)
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
        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let mut seq = 0;
        let mut elapsed = 0;
        for _ in 0..2 {
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed, None)
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

    // --- voice separation (add-meeting-speaker-diarization) ---
    //
    // The embedder is scripted, so a test chooses each window's vector and
    // therefore each voice. Two windows make a segment long enough to be
    // labelled (`MIN_WINDOWS`), and 36 000 samples at 16 kHz is exactly two
    // windows; 48 000 is three.

    const TWO_WINDOWS: usize = 36_000;
    const THREE_WINDOWS: usize = 48_000;

    /// Three unit vectors that no two of which are close enough to join.
    fn voice(index: usize) -> Vec<f32> {
        let mut v = vec![0.0; 3];
        v[index] = 1.0;
        v
    }

    fn windows_of(voice: &[f32], count: usize) -> Vec<Result<Vec<f32>, String>> {
        (0..count).map(|_| Ok(voice.to_vec())).collect()
    }

    fn tone(samples: usize, rate_hz: u32, amplitude: f32) -> PcmFrame {
        PcmFrame {
            samples: (0..samples)
                .map(|i| {
                    let t = i as f32 / rate_hz as f32;
                    (std::f32::consts::TAU * 220.0 * t).sin() * amplitude
                })
                .collect(),
            sample_rate_hz: rate_hz,
        }
    }

    /// A two-source segment the attribution calls remote: the system half
    /// carries the voice, the microphone half is quiet.
    fn remote_segment(samples: usize, rate_hz: u32) -> SpeechSegment {
        let mic = tone(samples, rate_hz, 0.01);
        let system = tone(samples, rate_hz, 0.5);
        SpeechSegment {
            pcm: kea_platform::audio::mix_frames(&mic, &system),
            has_speech: true,
            mic: Some(mic),
            system: Some(system),
        }
    }

    /// A two-source segment the attribution calls local.
    fn local_segment(samples: usize) -> SpeechSegment {
        let mic = tone(samples, 16_000, 0.5);
        let system = tone(samples, 16_000, 0.01);
        SpeechSegment {
            pcm: kea_platform::audio::mix_frames(&mic, &system),
            has_speech: true,
            mic: Some(mic),
            system: Some(system),
        }
    }

    fn mic_segment(samples: usize) -> SpeechSegment {
        SpeechSegment {
            pcm: tone(samples, 16_000, 0.5),
            has_speech: true,
            mic: None,
            system: None,
        }
    }

    /// The fake audio pops from the back, so segments are queued reversed to be
    /// heard in the order given.
    fn queued(segments: Vec<SpeechSegment>) -> Mutex<Vec<SpeechSegment>> {
        Mutex::new(segments.into_iter().rev().collect())
    }

    fn empty_tail() -> Result<SpeechSegment, String> {
        Ok(SpeechSegment {
            pcm: PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            },
            has_speech: false,
            mic: None,
            system: None,
        })
    }

    /// Answers each meeting prompt by its kind, and keeps every prompt it was
    /// asked, so a test can check what the model was shown.
    struct KindLlm {
        names_reply: String,
        fail_names: bool,
        prompts: Mutex<Vec<String>>,
    }

    const KIND_NOTES: &str = r#"{"summary":"kickoff summary","decisions":"","action_items":"","follow_ups":"","open_questions":""}"#;

    impl KindLlm {
        fn new(names_reply: &str, fail_names: bool) -> Arc<Self> {
            Arc::new(Self {
                names_reply: names_reply.to_string(),
                fail_names,
                prompts: Mutex::new(Vec::new()),
            })
        }

        fn prompts(&self) -> Vec<String> {
            self.prompts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl LlmEngine for KindLlm {
        fn id(&self) -> &str {
            "fake-llm"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }

        async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, EngineError> {
            self.prompts.lock().unwrap().push(req.prompt.clone());
            if req.prompt.contains("<participants>") {
                if self.fail_names {
                    return Err(EngineError::Other("names are down".into()));
                }
                return Ok(LlmResponse::untracked(self.names_reply.clone()));
            }
            if req.prompt.contains("<summary>") {
                return Ok(LlmResponse::untracked("Sprint Planning"));
            }
            Ok(LlmResponse::untracked(KIND_NOTES))
        }
    }

    /// Registries and repos for a voice test: the given transcript lines in
    /// order, and the given LLM for both the names request and the notes.
    async fn voice_world(
        texts: &[&str],
        llm: Arc<dyn LlmEngine>,
    ) -> (EngineRegistry, BindingRepo, ActionRepo, MeetingRepo) {
        let mut reg = EngineRegistry::default();
        reg.register_stt(Arc::new(CountingFakeStt {
            texts: texts.iter().map(|t| t.to_string()).collect(),
            call: AtomicUsize::new(0),
        }));
        reg.register_llm(llm);

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

    fn two_source_audio(segments: Vec<SpeechSegment>) -> FakeMeetingAudioIo {
        FakeMeetingAudioIo {
            capability: SystemAudioCapability::ScreenCaptureKit,
            pending_drains: queued(segments),
            ..Default::default()
        }
    }

    fn mic_only_audio(segments: Vec<SpeechSegment>) -> FakeMeetingAudioIo {
        FakeMeetingAudioIo {
            capability: SystemAudioCapability::MicOnly,
            pending_drains: queued(segments),
            ..Default::default()
        }
    }

    fn voices_with(steps: Vec<Result<Vec<f32>, String>>) -> MeetingVoices {
        MeetingVoices::new(Arc::new(ScriptedEmbedder::new(steps)))
    }

    fn settings(prefer_system_audio: bool) -> MeetingSettings {
        MeetingSettings {
            segment_duration_secs: 30,
            prefer_system_audio,
            ..MeetingSettings::default()
        }
    }

    /// One poll that must produce a segment, with voices.
    async fn poll_with(
        ctx: &mut MeetingRunContext<'_>,
        meeting_id: &str,
        seq: &mut i32,
        elapsed: &mut i64,
        voices: &mut MeetingVoices,
    ) -> MeetingSegmentEvent {
        run_meeting_poll_segment(ctx, meeting_id, seq, elapsed, Some(voices))
            .await
            .unwrap()
            .expect("a segment was queued")
    }

    fn speaker_map(detail: &MeetingDetail) -> BTreeMap<String, MeetingSpeaker> {
        detail
            .speakers
            .iter()
            .map(|s| (s.speaker_key.clone(), s.clone()))
            .collect()
    }

    #[tokio::test]
    async fn three_remote_voices_become_three_participants_and_local_stays_local() {
        let (reg, bindings, actions, meetings) =
            voice_world(&[], KindLlm::new("{\"names\":[]}", false)).await;
        let mut audio = two_source_audio(vec![
            remote_segment(THREE_WINDOWS, 16_000),
            remote_segment(THREE_WINDOWS, 16_000),
            remote_segment(THREE_WINDOWS, 16_000),
            local_segment(THREE_WINDOWS),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        // Nine windows for the three remote segments, in order. The local
        // segment must embed nothing: any call past the script would fail.
        let mut steps = Vec::new();
        for index in 0..3 {
            steps.extend(windows_of(&voice(index), 3));
        }
        let mut voices = voices_with(steps);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        let mut keys = Vec::new();
        for _ in 0..4 {
            let ev = poll_with(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                &mut voices,
            )
            .await;
            keys.push(ev.speaker_key.unwrap());
        }
        assert_eq!(keys, vec!["spk0", "spk1", "spk2", "local"]);

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        let speakers = speaker_map(&detail);
        assert_eq!(speakers["spk0"].display_name, "Person 1");
        assert_eq!(speakers["spk1"].display_name, "Person 2");
        assert_eq!(speakers["spk2"].display_name, "Person 3");
        assert_eq!(speakers["local"].display_name, "You");
    }

    #[tokio::test]
    async fn a_new_voice_gets_its_key_and_row_on_first_speech_and_a_return_reuses_it() {
        let (reg, bindings, actions, meetings) =
            voice_world(&[], KindLlm::new("{\"names\":[]}", false)).await;
        let mut audio = two_source_audio(vec![
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let mut steps = windows_of(&voice(0), 2);
        steps.extend(windows_of(&voice(1), 2));
        steps.extend(windows_of(&voice(0), 2));
        let mut voices = voices_with(steps);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        let first = poll_with(
            &mut ctx,
            &session.meeting_id,
            &mut seq,
            &mut elapsed,
            &mut voices,
        )
        .await;
        assert_eq!(first.speaker_key.as_deref(), Some("spk0"));
        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert!(
            !speaker_map(&detail).contains_key("spk1"),
            "no row before the voice speaks"
        );

        let second = poll_with(
            &mut ctx,
            &session.meeting_id,
            &mut seq,
            &mut elapsed,
            &mut voices,
        )
        .await;
        assert_eq!(second.speaker_key.as_deref(), Some("spk1"));
        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert_eq!(speaker_map(&detail)["spk1"].display_name, "Person 2");

        let third = poll_with(
            &mut ctx,
            &session.meeting_id,
            &mut seq,
            &mut elapsed,
            &mut voices,
        )
        .await;
        assert_eq!(
            third.speaker_key.as_deref(),
            Some("spk0"),
            "same voice, same label"
        );
    }

    #[tokio::test]
    async fn a_mic_only_meeting_with_voices_never_labels_anyone_you() {
        let (reg, bindings, actions, meetings) =
            voice_world(&[], KindLlm::new("{\"names\":[]}", false)).await;
        let mut audio = mic_only_audio(vec![mic_segment(TWO_WINDOWS), mic_segment(TWO_WINDOWS)]);
        let settings = settings(false);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let mut steps = windows_of(&voice(0), 2);
        steps.extend(windows_of(&voice(1), 2));
        let mut voices = voices_with(steps);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        let mut keys = Vec::new();
        for _ in 0..2 {
            let ev = poll_with(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                &mut voices,
            )
            .await;
            keys.push(ev.speaker_key.unwrap());
        }
        assert_eq!(keys, vec!["spk0", "spk1"]);

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert!(detail
            .segments
            .iter()
            .all(|s| s.speaker_key.as_deref() != Some("local")));
        assert!(
            detail.speakers.iter().all(|s| s.display_name != "You"),
            "no 'You' by assumption: {:?}",
            detail.speakers
        );
    }

    #[tokio::test]
    async fn a_failing_embedder_keeps_recording_and_labels_remote() {
        let (reg, bindings, actions, meetings) =
            voice_world(&[], KindLlm::new("{\"names\":[]}", false)).await;
        let mut audio = two_source_audio(vec![
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        // The first call fails, and once the script is spent every call fails.
        let mut voices = voices_with(vec![Err("embedding runtime broke".into())]);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        for _ in 0..2 {
            let ev = poll_with(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                &mut voices,
            )
            .await;
            assert_eq!(ev.speaker_key.as_deref(), Some("remote"));
            assert!(!ev.text.is_empty());
        }

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert_eq!(detail.segments.len(), 2, "recording went on");
        assert!(detail
            .segments
            .iter()
            .all(|s| s.speaker_key.as_deref() == Some("remote")));
        // The unassigned remote side is still remote, so its row now exists.
        assert_eq!(speaker_map(&detail)["remote"].display_name, "Others");
    }

    #[tokio::test]
    async fn a_resampled_segment_is_embedded_at_the_model_rate() {
        let (reg, bindings, actions, meetings) =
            voice_world(&[], KindLlm::new("{\"names\":[]}", false)).await;
        // 2.5 s at 48 kHz is 2.5 s at 16 kHz after resampling: two windows.
        let mut audio = two_source_audio(vec![remote_segment(120_000, 48_000)]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let mut voices = voices_with(windows_of(&voice(0), 2));

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        let ev = poll_with(
            &mut ctx,
            &session.meeting_id,
            &mut seq,
            &mut elapsed,
            &mut voices,
        )
        .await;
        assert_eq!(ev.speaker_key.as_deref(), Some("spk0"));
    }

    /// Two live voices, one of which the stop-time pass finds to be the other.
    ///
    /// Live, the second voice opens its own label, because its window is under
    /// `JOIN_SIM` from the first centroid. Across the whole meeting the windows
    /// of the two labels average above `MERGE_SIM`, so the stop merges them. The
    /// vectors are chosen for exactly that: cos(a, b) = 0.5, cos(a, x) = 0.9,
    /// cos(x, b) ≈ 0.83.
    fn split_voice_vectors() -> (Vec<f32>, Vec<f32>, Vec<f32>) {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.5, 0.866_025_4, 0.0];
        let x = vec![0.9, 0.435_889_9, 0.0];
        (a, b, x)
    }

    /// A mic-only segment too short to place is unattributed, not "You".
    #[tokio::test]
    async fn a_mic_only_segment_too_short_to_place_is_unattributed() {
        let (reg, bindings, actions, meetings) =
            voice_world(&[], KindLlm::new("{\"names\":[]}", false)).await;
        // About 1.25 s: one window at most, below MIN_WINDOWS, so nothing is
        // embedded and an empty script is enough.
        let mut audio = mic_only_audio(vec![mic_segment(20_000)]);
        let settings = settings(false);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let mut voices = voices_with(Vec::new());

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        let ev = poll_with(
            &mut ctx,
            &session.meeting_id,
            &mut seq,
            &mut elapsed,
            &mut voices,
        )
        .await;
        assert_eq!(ev.speaker_key.as_deref(), Some("mixed"));

        let detail = meetings.get(&session.meeting_id).await.unwrap().unwrap();
        assert!(detail.speakers.iter().all(|s| s.display_name != "You"));
    }

    /// A name typed while recording stays on its voice through the stop pass.
    #[tokio::test]
    async fn a_name_typed_mid_meeting_survives_the_stop_pass() {
        let llm = KindLlm::new("{\"names\":[]}", false);
        let (reg, bindings, actions, meetings) = voice_world(&[], llm).await;
        let mut audio = two_source_audio(vec![
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let mut steps = windows_of(&voice(0), 2);
        steps.extend(windows_of(&voice(1), 2));
        let mut voices = voices_with(steps);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        for _ in 0..2 {
            poll_with(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                &mut voices,
            )
            .await;
        }
        meetings
            .set_speaker_name(&session.meeting_id, "spk1", "Dan")
            .await
            .unwrap();

        let detail = run_meeting_stop(
            &reg,
            &bindings,
            &actions,
            &meetings,
            &session,
            empty_tail(),
            &[],
            Some(&mut voices),
        )
        .await
        .unwrap();
        let speakers = speaker_map(&detail);
        let second_key = detail.segments[1].speaker_key.clone().unwrap();
        assert_eq!(speakers[&second_key].display_name, "Dan");
        assert_eq!(speakers[&second_key].source, SpeakerSource::User);
        let first_key = detail.segments[0].speaker_key.clone().unwrap();
        assert_eq!(speakers[&first_key].display_name, "Person 1");
    }

    #[tokio::test]
    async fn a_voice_split_while_recording_is_one_participant_after_stop() {
        let llm = KindLlm::new("{\"names\":[]}", false);
        let (reg, bindings, actions, meetings) = voice_world(&[], llm.clone()).await;
        let mut audio = two_source_audio(vec![
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let (a, b, x) = split_voice_vectors();
        let mut steps = windows_of(&a, 2);
        steps.extend(windows_of(&b, 2));
        steps.extend(windows_of(&x, 2));
        let mut voices = voices_with(steps);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        let mut live = Vec::new();
        for _ in 0..3 {
            let ev = poll_with(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                &mut voices,
            )
            .await;
            live.push(ev.speaker_key.unwrap());
        }
        assert_eq!(live, vec!["spk0", "spk1", "spk0"], "the live split");

        let detail = run_meeting_stop(
            &reg,
            &bindings,
            &actions,
            &meetings,
            &session,
            empty_tail(),
            &[],
            Some(&mut voices),
        )
        .await
        .unwrap();

        let remote: Vec<&str> = detail
            .segments
            .iter()
            .filter_map(|s| s.speaker_key.as_deref())
            .filter(|k| *k != "local")
            .collect();
        assert_eq!(remote, vec!["spk0", "spk0", "spk0"]);
        let participants: Vec<&MeetingSpeaker> = detail
            .speakers
            .iter()
            .filter(|s| s.speaker_key != "local")
            .collect();
        assert_eq!(participants.len(), 1, "{participants:?}");
        assert_eq!(participants[0].display_name, "Person 1");

        let prompts = llm.prompts();
        assert!(
            // "Person 2: " is how a transcript line is labelled. The bare name
            // also appears in the prompts' own examples, which is not a line.
            prompts.iter().all(|p| !p.contains("Person 2: ")),
            "the notes prompt must show one label"
        );
        assert!(prompts
            .iter()
            .any(|p| p.contains("<transcript>") && p.contains("Person 1: ")));
    }

    /// Two remote voices with transcript lines, for the naming tests.
    async fn two_named_candidates(llm: Arc<KindLlm>) -> (MeetingDetail, Arc<KindLlm>) {
        let (reg, bindings, actions, meetings) = voice_world(
            &["Okay, let's begin.", "Thanks Dan, that helps."],
            llm.clone(),
        )
        .await;
        let mut audio = two_source_audio(vec![
            remote_segment(TWO_WINDOWS, 16_000),
            remote_segment(TWO_WINDOWS, 16_000),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let mut steps = windows_of(&voice(0), 2);
        steps.extend(windows_of(&voice(1), 2));
        let mut voices = voices_with(steps);

        let session = run_meeting_start(&mut ctx, true).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        for _ in 0..2 {
            poll_with(
                &mut ctx,
                &session.meeting_id,
                &mut seq,
                &mut elapsed,
                &mut voices,
            )
            .await;
        }
        let detail = run_meeting_stop(
            &reg,
            &bindings,
            &actions,
            &meetings,
            &session,
            empty_tail(),
            &[],
            Some(&mut voices),
        )
        .await
        .unwrap();
        (detail, llm)
    }

    #[tokio::test]
    async fn an_inferred_name_reaches_the_notes_prompt() {
        let reply = r#"{"names":[{"speaker":"Person 2","name":"Dan","evidence":"Thanks Dan, that helps."}]}"#;
        let (detail, llm) = two_named_candidates(KindLlm::new(reply, false)).await;

        let speakers = speaker_map(&detail);
        assert_eq!(speakers["spk1"].display_name, "Dan");
        assert_eq!(speakers["spk1"].source, SpeakerSource::Inferred);

        let prompts = llm.prompts();
        assert!(prompts.iter().any(|p| p.contains("<participants>")));
        assert!(
            prompts.iter().any(|p| p.contains("<transcript>")
                && !p.contains("<participants>")
                && p.contains("Dan: Thanks Dan, that helps.")),
            "the notes prompt must carry the inferred name"
        );
    }

    #[tokio::test]
    async fn a_name_that_does_not_verify_is_not_applied() {
        let reply = r#"{"names":[{"speaker":"Person 2","name":"Priya","evidence":"Thanks Priya, that helps."}]}"#;
        let (detail, _llm) = two_named_candidates(KindLlm::new(reply, false)).await;
        assert_eq!(speaker_map(&detail)["spk1"].display_name, "Person 2");
    }

    #[tokio::test]
    async fn a_failed_name_request_still_writes_the_notes() {
        let (detail, _llm) = two_named_candidates(KindLlm::new("", true)).await;
        assert_eq!(speaker_map(&detail)["spk1"].display_name, "Person 2");
        let notes = detail.notes.expect("notes are written without names");
        assert_eq!(notes.summary, "kickoff summary");
        assert_eq!(detail.meeting.status, MeetingStatus::Completed);
    }

    #[tokio::test]
    async fn no_name_request_when_nothing_is_left_to_name() {
        let llm = KindLlm::new("{\"names\":[]}", false);
        let (reg, bindings, actions, meetings) = voice_world(&[], llm.clone()).await;
        let mut audio = two_source_audio(vec![
            local_segment(TWO_WINDOWS),
            remote_segment(TWO_WINDOWS, 16_000),
        ]);
        let settings = settings(true);
        let mut ctx = MeetingRunContext {
            engines: &reg,
            bindings: &bindings,
            actions: &actions,
            meetings: &meetings,
            audio: &mut audio,
            settings: &settings,
            vocabulary: &[],
        };
        let session = run_meeting_start(&mut ctx, false).await.unwrap();
        let (mut seq, mut elapsed) = (0, 0);
        for _ in 0..2 {
            run_meeting_poll_segment(&mut ctx, &session.meeting_id, &mut seq, &mut elapsed, None)
                .await
                .unwrap();
        }
        run_meeting_stop(
            &reg,
            &bindings,
            &actions,
            &meetings,
            &session,
            empty_tail(),
            &[],
            None,
        )
        .await
        .unwrap();

        assert!(
            llm.prompts().iter().all(|p| !p.contains("<participants>")),
            "no name request without a Person label"
        );
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
                        speaker_key: Some(SpeakerChannel::Local.as_str().into()),
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

    // --- 3.1: the Ask AI turn ---

    /// A meeting with one transcript line and generated notes, in the status
    /// the test needs. A recording meeting keeps the status `create` gave it.
    async fn chatted_meeting(meetings: &MeetingRepo, id: &str, status: MeetingStatus) {
        recording_meeting(meetings, id, &["we should ship on the 14th"]).await;
        meetings
            .upsert_notes(&MeetingNotes {
                meeting_id: id.into(),
                summary: "The launch slips a week.".into(),
                decisions: "Launch moves to the 14th.".into(),
                action_items: String::new(),
                follow_ups: "Confirm the venue".into(),
                open_questions: String::new(),
                prompt_version: "meeting-notes-v3".into(),
                engine_id: Some("fake-llm".into()),
                model: None,
                edited: false,
            })
            .await
            .unwrap();
        if status != MeetingStatus::Recording {
            meetings.complete(id, status, None).await.unwrap();
        }
    }

    /// Answers only once the test releases it, and says when it was called,
    /// so a test can hold one turn open while it sends a second.
    struct GatedLlm {
        called: tokio::sync::Notify,
        release: tokio::sync::Notify,
    }

    #[async_trait]
    impl LlmEngine for GatedLlm {
        fn id(&self) -> &str {
            "fake-llm"
        }

        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }

        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, EngineError> {
            self.called.notify_one();
            self.release.notified().await;
            Ok(LlmResponse::untracked(r#"{"reply":"Done.","notes":null}"#))
        }
    }

    #[tokio::test]
    async fn a_question_is_answered_and_both_messages_are_stored() {
        let llm =
            RecordingLlm::new(&[r#"{"reply":"It was agreed on the 14th [00:30].","notes":null}"#]);
        let (reg, bindings, _, meetings) = world(llm).await;
        chatted_meeting(&meetings, "c1", MeetingStatus::Completed).await;
        let turns = MeetingChatTurns::default();

        let reply = run_chat_turn(
            &reg,
            &bindings,
            &meetings,
            &turns,
            None,
            "c1",
            "  When was it agreed?  ",
        )
        .await
        .unwrap();

        assert_eq!(reply.message.content, "It was agreed on the 14th [00:30].");
        assert!(matches!(reply.message.role, MessageRole::Assistant));
        assert_eq!(reply.message.proposal, None);
        assert!(!reply.truncated);
        let chat = meetings.chat_messages("c1").await.unwrap();
        assert_eq!(chat.len(), 2);
        assert!(matches!(chat[0].role, MessageRole::User));
        assert_eq!(chat[0].content, "When was it agreed?");
        assert!(!turns.is_in_flight("c1"));
    }

    /// Every reply is a billed call, so each one lands in the usage ledger
    /// under the meetings feature — including a prose reply with no proposal.
    #[tokio::test]
    async fn each_chat_reply_is_recorded_in_the_usage_ledger() {
        let llm = RecordingLlm::new(&[
            r#"{"reply":"One.","notes":null}"#,
            "plain prose, not the JSON shape",
        ]);
        let (reg, bindings, _, meetings) = world(llm).await;
        chatted_meeting(&meetings, "c9", MeetingStatus::Completed).await;
        let turns = MeetingChatTurns::default();
        let data = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data).await.unwrap();
        let usage = UsageRepo::new(data.clone());

        for question in ["first?", "second?"] {
            run_chat_turn(
                &reg,
                &bindings,
                &meetings,
                &turns,
                Some(&usage),
                "c9",
                question,
            )
            .await
            .unwrap();
        }

        let rows: Vec<String> = sqlx::query_scalar("SELECT feature_id FROM usage_events")
            .fetch_all(&data)
            .await
            .unwrap();
        assert_eq!(rows, vec!["meetings", "meetings"]);
    }

    /// The proposal is stored whole, with the sections it does not change
    /// carried over, and the notes themselves do not move until Apply.
    #[tokio::test]
    async fn a_proposal_on_a_stopped_meeting_is_stored_pending_with_the_full_notes() {
        let llm = RecordingLlm::new(&[
            r#"{"reply":"Here is a shorter summary.","notes":{"summary":"Launch slips to the 14th."}}"#,
        ]);
        let (reg, bindings, _, meetings) = world(llm).await;
        chatted_meeting(&meetings, "c2", MeetingStatus::Completed).await;

        let reply = run_chat_turn(
            &reg,
            &bindings,
            &meetings,
            &MeetingChatTurns::default(),
            None,
            "c2",
            "make the summary shorter",
        )
        .await
        .unwrap();

        let proposal = reply.message.proposal.expect("a proposal");
        assert_eq!(proposal.state, ProposalState::Pending);
        assert_eq!(proposal.proposal.summary, "Launch slips to the 14th.");
        assert_eq!(proposal.proposal.decisions, "Launch moves to the 14th.");
        assert_eq!(proposal.proposal.follow_ups, "Confirm the venue");

        let detail = meetings.get("c2").await.unwrap().unwrap();
        assert_eq!(detail.notes.unwrap().summary, "The launch slips a week.");
        assert!(!detail.can_undo);
    }

    /// The model proposed an edit while the meeting is live. The prompt left
    /// the edit instructions out, and the reply parser drops the proposal even
    /// though the model sent one, so nothing is stored to be applied later.
    #[tokio::test]
    async fn a_proposal_is_dropped_while_the_meeting_is_recording() {
        let llm = RecordingLlm::new(&[r#"{"reply":"Shorter now.","notes":{"summary":"Short."}}"#]);
        let (reg, bindings, _, meetings) = world(llm.clone()).await;
        chatted_meeting(&meetings, "c3", MeetingStatus::Recording).await;

        let reply = run_chat_turn(
            &reg,
            &bindings,
            &meetings,
            &MeetingChatTurns::default(),
            None,
            "c3",
            "make the summary shorter",
        )
        .await
        .unwrap();

        assert_eq!(reply.message.content, "Shorter now.");
        assert_eq!(reply.message.proposal, None);
        assert!(llm.prompts()[0].contains("not available while the meeting is still recording"));
    }

    /// A provider failure stores nothing, so the user can send the same
    /// question again without the failed one sitting in the history.
    #[tokio::test]
    async fn a_provider_failure_stores_nothing_and_frees_the_meeting() {
        let (reg, bindings, _, meetings) = world(RecordingLlm::failing()).await;
        chatted_meeting(&meetings, "c4", MeetingStatus::Completed).await;
        let turns = MeetingChatTurns::default();

        let err = run_chat_turn(&reg, &bindings, &meetings, &turns, None, "c4", "anything")
            .await
            .unwrap_err();

        assert!(err.contains("provider is down"));
        assert!(meetings.chat_messages("c4").await.unwrap().is_empty());
        assert!(!turns.is_in_flight("c4"));
    }

    /// The first turn is parked inside the provider call. The second, for the
    /// same meeting, is refused, and the first still stores its own pair.
    #[tokio::test]
    async fn a_second_turn_for_the_same_meeting_is_refused_while_one_is_in_flight() {
        let llm = Arc::new(GatedLlm {
            called: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let (reg, bindings, _, meetings) = world(llm.clone()).await;
        chatted_meeting(&meetings, "c5", MeetingStatus::Completed).await;
        let turns = MeetingChatTurns::default();

        let first = run_chat_turn(&reg, &bindings, &meetings, &turns, None, "c5", "first");
        let second = async {
            llm.called.notified().await;
            assert!(turns.is_in_flight("c5"));
            let refused =
                run_chat_turn(&reg, &bindings, &meetings, &turns, None, "c5", "second").await;
            llm.release.notify_one();
            refused
        };
        let (first, refused) = tokio::join!(first, second);

        assert_eq!(first.unwrap().message.content, "Done.");
        assert_eq!(refused.unwrap_err(), CHAT_TURN_BUSY);
        assert_eq!(meetings.chat_messages("c5").await.unwrap().len(), 2);
        assert!(!turns.is_in_flight("c5"));
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
            0,
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
                    speaker_key: Some(SpeakerChannel::Local.as_str().into()),
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

    /// The bug a user reported as "every item four times over": each pass
    /// restated the same tasks in new words, and each wording became a row.
    #[tokio::test]
    async fn repeated_interim_passes_keep_one_row_per_task() {
        let llm = RecordingLlm::new(&[
            r#"{"summary":"s","decisions":"","action_items":"","follow_ups":"","open_questions":"","action_item_rows":[{"text":"Grant Confluence access","owner":"Onboarding lead","due_hint":"in progress now"}]}"#,
            r#"{"summary":"s","decisions":"","action_items":"","follow_ups":"","open_questions":"","action_item_rows":[{"text":"Grant Confluence access; it will take a while to come through","owner":"Onboarding lead","due_hint":"in progress now"},{"text":"Schedule a meeting with Dan","owner":"Onboarding lead","due_hint":"for next Monday"}]}"#,
        ]);
        let (reg, bindings, _, meetings) = world(llm.clone()).await;
        recording_meeting(&meetings, "m1", &["I'm granting Confluence now"]).await;

        let first = run_interim_notes_pass(&reg, &bindings, &meetings, "m1", 0, None)
            .await
            .unwrap();
        meetings
            .append_segment(
                "m1",
                &NewSegment {
                    sequence: 1,
                    start_offset_ms: 30_000,
                    end_offset_ms: 60_000,
                    text: "it takes a while; I'll set you up with Dan on Monday".into(),
                    speaker_key: Some(SpeakerChannel::Local.as_str().into()),
                },
            )
            .await
            .unwrap();
        run_interim_notes_pass(&reg, &bindings, &meetings, "m1", first.next_sequence, None)
            .await
            .unwrap();

        let rows = meetings.action_items("m1").await.unwrap();
        let texts: Vec<&str> = rows.iter().map(|r| r.text.as_str()).collect();
        assert_eq!(
            texts,
            [
                "Grant Confluence access; it will take a while to come through",
                "Schedule a meeting with Dan",
            ]
        );
        // The second pass was shown the list it had to carry forward.
        assert!(llm.prompts()[1].contains(r#""text":"Grant Confluence access""#));
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
            None,
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
