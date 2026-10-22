//! The OpenAI wire shapes for the model routes: requests in, bodies out.
//!
//! Pure. Nothing here reads `AppState`, opens a file, touches an engine or
//! looks at a clock. The caller passes in the bytes it received and the ids
//! and timestamps it wants stamped, and gets back bytes and a status. That is
//! what lets every shape in this file be pinned by a golden string, and it is
//! what keeps the OpenAI vocabulary out of `exec`, which speaks KEA's own.
//!
//! The error type is the one place the two vocabularies meet. An internal
//! failure becomes an [`ApiError`] here, and [`ApiError::body`] is the only
//! thing a client sees of it. A provider's message goes in as plain text and
//! nothing else: no key, no token, no request body. So there is no field for
//! a secret to end up in, and [`redact`] is the one helper for the case where
//! a provider echoes one back.

// Nothing calls these until the model routes in `server.rs` and `exec.rs`
// land (tasks 3 and 4). Remove this allow when the first route uses them.
#![allow(dead_code)]

use kea_core::transcript::{SubtitleFormat, SubtitleOpts};
use kea_engines::traits::{SttSegment, TokenUsage};
use serde::Deserialize;
use serde_json::{json, Value};

/// `Content-Type` of the single-chunk chat stream.
pub const SSE_CONTENT_TYPE: &str = "text/event-stream";

/// The longest `input` a speech request may carry, in `char`s.
///
/// OpenAI's own limit. It bounds how long one request can hold the speech
/// engine, which is the cost a runaway script actually imposes.
pub const SPEECH_MAX_CHARS: usize = 4096;

/// The speed range a speech request may ask for, inclusive.
pub const SPEECH_SPEED_RANGE: std::ops::RangeInclusive<f32> = 0.25..=4.0;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// An error a model route answers with.
///
/// `status` is what the HTTP layer sends. `kind` and `code` are the two
/// fields OpenAI clients read: SDKs branch on `code` (for example
/// `model_not_found`), and `kind` is only for humans reading a log. Only
/// [`ApiError::retry_after`] becomes a header, on the two statuses that have
/// one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    pub status: u16,
    pub message: String,
    /// OpenAI's `error.type`.
    pub kind: &'static str,
    /// OpenAI's `error.code`. `None` serialises as `null`, which is what
    /// OpenAI itself sends for errors with no finer code.
    pub code: Option<String>,
    /// Seconds for the `Retry-After` header. Set only on `busy` and
    /// `rate_limited`, the two statuses a client is expected to retry.
    pub retry_after: Option<u64>,
}

impl ApiError {
    fn new(
        status: u16,
        kind: &'static str,
        code: Option<&str>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            status,
            message: message.into(),
            kind,
            code: code.map(str::to_string),
            retry_after: None,
        }
    }

    /// `400`: the request is malformed or asks for something this route
    /// does not do. Also the answer for a body the route cannot decode.
    pub fn invalid_request(message: impl Into<String>) -> Self {
        Self::new(400, "invalid_request_error", None, message)
    }

    /// `401`: the token is missing or wrong. The auth layer says this
    /// without saying which check failed, and so does this.
    pub fn auth(message: impl Into<String>) -> Self {
        Self::new(
            401,
            "authentication_error",
            Some("invalid_api_key"),
            message,
        )
    }

    /// `404`: a `model` value that is neither omitted, an alias, nor a
    /// listed id for this capability. The requested name is echoed back, which
    /// is the only way a caller can see what it sent wrong.
    pub fn model_not_found(model: &str) -> Self {
        Self::new(
            404,
            "invalid_request_error",
            Some("model_not_found"),
            format!("The model `{model}` does not exist or is not available for this endpoint."),
        )
    }

    /// `409`: nothing is bound for this capability yet. The message names the
    /// page to visit, because the caller cannot fix it from the request.
    pub fn not_configured(message: impl Into<String>) -> Self {
        Self::new(
            409,
            "invalid_request_error",
            Some("not_configured"),
            message,
        )
    }

    /// `409` with `Retry-After`: another request of the same kind is already
    /// running. Refused rather than queued, so a script cannot pile up work.
    pub fn busy(message: impl Into<String>, retry_after: u64) -> Self {
        let mut err = Self::new(409, "invalid_request_error", Some("busy"), message);
        err.retry_after = Some(retry_after);
        err
    }

    /// `413`: the body is over the route's limit. The limit is in the message
    /// so a script can tell a size problem from a format problem.
    pub fn too_large(limit_bytes: usize) -> Self {
        Self::new(
            413,
            "invalid_request_error",
            Some("request_too_large"),
            format!(
                "The request body is larger than the {limit_bytes}-byte limit for this endpoint."
            ),
        )
    }

    /// `429` with `Retry-After`: the per-minute LLM limit is spent.
    pub fn rate_limited(retry_after: u64) -> Self {
        let mut err = Self::new(
            429,
            "rate_limit_error",
            Some("rate_limit_exceeded"),
            "Too many requests this minute. Wait and try again.",
        );
        err.retry_after = Some(retry_after);
        err
    }

    /// `502`: the bound hosted provider failed. Takes the message alone, so
    /// the caller decides what the provider said is safe to repeat. Pass it
    /// through [`redact`] first when a secret might be in it.
    pub fn provider(message: impl Into<String>) -> Self {
        Self::new(502, "api_error", Some("provider_error"), message)
    }

    /// The JSON body: `{"error": {"message", "type", "code"}}`.
    ///
    /// Built from the four fields and nothing else, so an error body can
    /// never carry more than the caller put in its message.
    pub fn body(&self) -> Value {
        json!({
            "error": {
                "message": self.message,
                "type": self.kind,
                "code": self.code,
            }
        })
    }
}

/// Replaces every occurrence of `secret` in `text` with `[redacted]`.
///
/// For a provider error that echoes back the key it was sent. An empty
/// secret is a no-op rather than a replace-everything, which would blank the
/// message.
pub fn redact(text: &str, secret: &str) -> String {
    if secret.is_empty() {
        return text.to_string();
    }
    text.replace(secret, "[redacted]")
}

// ---------------------------------------------------------------------------
// Model names
// ---------------------------------------------------------------------------

/// The three things a model route can be asked for.
///
/// Each route fixes one of these, so the capability is never read from the
/// request. A `whisper-1` sent to `/v1/chat/completions` is a caller error,
/// not a hint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    Stt,
    Tts,
    Llm,
}

impl Capability {
    /// The name the `kea_capability` field and the CLI both print.
    pub fn as_str(self) -> &'static str {
        match self {
            Capability::Stt => "stt",
            Capability::Tts => "tts",
            Capability::Llm => "llm",
        }
    }
}

/// What a `model` value resolved to.
///
/// `Bound` means "whatever the user bound for API use", which is also the
/// answer for an omitted name. `Listed` names one engine and model directly,
/// and is applied to this request only. It is never written to a binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelChoice {
    Bound,
    Listed {
        engine_id: String,
        model: Option<String>,
    },
}

/// One entry in `GET /v1/models`, and the thing a `model` value is matched
/// against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedModel {
    /// The string a caller sends back. Always [`model_id`] of the other fields.
    pub id: String,
    pub engine_id: String,
    pub model: Option<String>,
    pub capability: Capability,
    /// Whether this is the model bound for API use for its capability.
    pub is_default: bool,
}

/// The id a listed model is known by: `<engine_id>/<model>`, or just
/// `<engine_id>` for an engine with one model.
///
/// Built from engine ids, which are already persisted in bindings, so the
/// same model has the same id across restarts. A unit test pins the format
/// because clients hard-code these strings.
pub fn model_id(engine_id: &str, model: Option<&str>) -> String {
    match model {
        Some(m) => format!("{engine_id}/{m}"),
        None => engine_id.to_string(),
    }
}

/// The OpenAI aliases a route accepts for its bound model.
fn is_alias(requested: &str, cap: Capability) -> bool {
    match cap {
        Capability::Stt => requested == "whisper-1",
        Capability::Tts => matches!(requested, "tts-1" | "tts-1-hd"),
        Capability::Llm => requested.starts_with("gpt-"),
    }
}

/// Resolves a request's `model` field for one route.
///
/// In order: omitted, empty, or `default` means the bound model. Then a
/// known OpenAI alias for this route means the bound model. Then a listed id
/// for this same capability means that model. Anything else is
/// `model_not_found`, including a listed id of another capability, which is
/// the wrong-route case clients hit when they point a chat model at speech.
pub fn resolve_model(
    requested: Option<&str>,
    cap: Capability,
    listed: &[ListedModel],
) -> Result<ModelChoice, ApiError> {
    let requested = match requested.map(str::trim) {
        None | Some("") | Some("default") => return Ok(ModelChoice::Bound),
        Some(r) => r,
    };
    if is_alias(requested, cap) {
        return Ok(ModelChoice::Bound);
    }
    listed
        .iter()
        .find(|m| m.id == requested && m.capability == cap)
        .map(|m| ModelChoice::Listed {
            engine_id: m.engine_id.clone(),
            model: m.model.clone(),
        })
        .ok_or_else(|| ApiError::model_not_found(requested))
}

/// `GET /v1/models` in OpenAI's list shape, with KEA's two extra fields.
///
/// `created` is `0` because the listing is not a record of when a model was
/// installed, and reading the clock here would make the body impure. SDKs
/// accept it as an opaque number. `kea_capability` says which route a model
/// serves; `kea_default` marks the one bound for API use.
pub fn models_list_body(listed: &[ListedModel]) -> Value {
    let data: Vec<Value> = listed
        .iter()
        .map(|m| {
            json!({
                "id": m.id,
                "object": "model",
                "created": 0,
                "owned_by": "kea",
                "kea_capability": m.capability.as_str(),
                "kea_default": m.is_default,
            })
        })
        .collect();
    json!({"object": "list", "data": data})
}

// ---------------------------------------------------------------------------
// Transcription
// ---------------------------------------------------------------------------

/// The `response_format` values a transcription accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscriptionFormat {
    Json,
    Text,
    VerboseJson,
    Srt,
    Vtt,
}

impl TranscriptionFormat {
    fn parse(value: &str) -> Result<Self, ApiError> {
        match value {
            "json" => Ok(Self::Json),
            "text" => Ok(Self::Text),
            "verbose_json" => Ok(Self::VerboseJson),
            "srt" => Ok(Self::Srt),
            "vtt" => Ok(Self::Vtt),
            other => Err(ApiError::invalid_request(format!(
                "`response_format` `{other}` is not supported; use json, text, verbose_json, srt or vtt."
            ))),
        }
    }
}

/// The non-file fields of `POST /v1/audio/transcriptions`.
///
/// The `file` part is the caller's to read, because its bytes are not text
/// and do not belong in a field list. `prompt` and `language` are accepted
/// and carried, but the engines do not take them yet, so they are ignored by
/// the route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptionForm {
    pub model: Option<String>,
    pub language: Option<String>,
    pub prompt: Option<String>,
    pub response_format: TranscriptionFormat,
    /// KEA's extension. Asks for per-segment speaker labels.
    pub speakers: bool,
}

impl TranscriptionForm {
    /// Parses the text fields the caller collected from the multipart body.
    ///
    /// Takes a slice of pairs rather than a multipart type, so the parser is
    /// testable without a body. The first occurrence of a name wins, which
    /// is what a form with a repeated field is most likely to mean. An empty
    /// value counts as absent, because some clients send `model=` for "use
    /// the default".
    pub fn from_fields(fields: &[(String, String)]) -> Result<Self, ApiError> {
        let get = |name: &str| {
            fields
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.trim())
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        };
        let response_format = match get("response_format") {
            None => TranscriptionFormat::Json,
            Some(v) => TranscriptionFormat::parse(&v)?,
        };
        let speakers = match get("speakers").as_deref() {
            None | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            Some(other) => {
                return Err(ApiError::invalid_request(format!(
                    "`speakers` must be `true` or `false`, not `{other}`."
                )))
            }
        };
        Ok(Self {
            model: get("model"),
            language: get("language"),
            prompt: get("prompt"),
            response_format,
            speakers,
        })
    }
}

/// One timed span of a transcript, as the transcription route renders it.
///
/// Its own type rather than [`SttSegment`] because a speaker is a KEA
/// addition the engine does not know about. [`render_transcription`] folds
/// the speaker into the text only for the subtitle formats, which have
/// nowhere else to put it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
    /// `Person N`, when `speakers=true` was asked for and the segment had
    /// a voice assigned to it.
    pub speaker: Option<String>,
}

const CONTENT_TYPE_JSON: &str = "application/json";
const CONTENT_TYPE_TEXT: &str = "text/plain; charset=utf-8";
const CONTENT_TYPE_SRT: &str = "application/x-subrip";
const CONTENT_TYPE_VTT: &str = "text/vtt; charset=utf-8";

/// Renders a transcript in the requested format, returning content type and
/// body.
///
/// `srt` and `vtt` go through the same [`SubtitleFormat`] renderer the
/// Transcripts page exports with, so an API subtitle and an exported one
/// cannot disagree on cue splitting or timing. The one addition is the
/// speaker label, which is written into the cue text as `Person N: ` because
/// a subtitle cue has no other field for it.
pub fn render_transcription(
    format: TranscriptionFormat,
    full_text: &str,
    segments: &[TranscriptSegment],
) -> (&'static str, Vec<u8>) {
    match format {
        TranscriptionFormat::Json => (CONTENT_TYPE_JSON, to_vec(&json!({"text": full_text}))),
        TranscriptionFormat::Text => (CONTENT_TYPE_TEXT, full_text.as_bytes().to_vec()),
        TranscriptionFormat::VerboseJson => (
            CONTENT_TYPE_JSON,
            to_vec(&verbose_body(full_text, segments)),
        ),
        TranscriptionFormat::Srt | TranscriptionFormat::Vtt => {
            let (subtitle, content_type) = match format {
                TranscriptionFormat::Srt => (SubtitleFormat::Srt, CONTENT_TYPE_SRT),
                _ => (SubtitleFormat::Vtt, CONTENT_TYPE_VTT),
            };
            let opts = SubtitleOpts::default();
            let cues = labelled_cues(segments, &opts);
            let body = subtitle.render(&cues, &opts);
            (content_type, body.into_bytes())
        }
    }
}

/// `verbose_json`: timed segments in seconds, the shape OpenAI clients read.
///
/// `speaker` is written only when present, so a transcript without
/// `speakers=true` is byte-identical to what it would have been without the
/// extension.
fn verbose_body(full_text: &str, segments: &[TranscriptSegment]) -> Value {
    let segs: Vec<Value> = segments
        .iter()
        .enumerate()
        .map(|(i, s)| {
            let mut seg = json!({
                "id": i,
                "start": seconds(s.start_ms),
                "end": seconds(s.end_ms),
                "text": s.text,
            });
            if let Some(speaker) = &s.speaker {
                seg["speaker"] = json!(speaker);
            }
            seg
        })
        .collect();
    json!({"task": "transcribe", "text": full_text, "segments": segs})
}

fn seconds(ms: u64) -> f64 {
    ms as f64 / 1000.0
}

/// A segment's text with its speaker label in front, for the subtitle
/// formats. An empty segment stays empty, so the subtitle writer still drops
/// it rather than showing a cue that is only a name.
/// The segments to hand the subtitle writer, with every cue of a speaker's
/// turn labelled — not only the first.
///
/// The writer splits a long segment into several cues, and a label written
/// into the segment's text would survive on the first of them only. So a
/// labelled segment is split here first, with the line width narrowed by the
/// label's length, and each piece is labelled; the pieces already fit, so the
/// writer's own pass leaves them whole.
fn labelled_cues(segments: &[TranscriptSegment], opts: &SubtitleOpts) -> Vec<SttSegment> {
    let mut out = Vec::new();
    for segment in segments {
        let Some(speaker) = segment.speaker.as_deref() else {
            out.push(SttSegment::new(
                segment.start_ms,
                segment.end_ms,
                segment.text.clone(),
            ));
            continue;
        };
        let prefix = format!("{speaker}: ");
        let narrowed = SubtitleOpts {
            max_line_chars: opts
                .max_line_chars
                .saturating_sub(prefix.chars().count())
                .max(8),
            ..*opts
        };
        let pieces = kea_core::transcript::build_cues(
            &[SttSegment::new(
                segment.start_ms,
                segment.end_ms,
                segment.text.clone(),
            )],
            &narrowed,
        );
        for cue in pieces {
            out.push(SttSegment::new(
                cue.start_ms,
                cue.end_ms,
                format!("{prefix}{}", cue.lines.join(" ")),
            ));
        }
    }
    out
}

fn to_vec(value: &Value) -> Vec<u8> {
    // Serialising a `Value` cannot fail: every key is a string and every
    // number is finite, so the fallback is unreachable and empty rather than
    // a panic in a request handler.
    serde_json::to_vec(value).unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Speech
// ---------------------------------------------------------------------------

/// The body of `POST /v1/audio/speech`.
///
/// `input` is optional at the serde layer so that a missing field gets the
/// same `400` as an empty one, with KEA's wording, rather than serde's.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct SpeechRequest {
    pub model: Option<String>,
    pub input: Option<String>,
    pub voice: Option<String>,
    pub speed: Option<f32>,
    pub response_format: Option<String>,
}

/// The audio encodings speech can return.
///
/// Anything else is refused rather than converted: a client that asked for
/// `mp3` and got a WAV would fail somewhere else, later, with no message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpeechFormat {
    /// 16-bit PCM in a RIFF container.
    Wav,
    /// Raw 16-bit little-endian mono PCM, with the rate in a header.
    Pcm,
}

impl SpeechFormat {
    pub fn content_type(self) -> &'static str {
        match self {
            SpeechFormat::Wav => "audio/wav",
            SpeechFormat::Pcm => "application/octet-stream",
        }
    }
}

/// Parses a speech body. Unknown fields are ignored, as on every route.
pub fn parse_speech(body: &[u8]) -> Result<SpeechRequest, ApiError> {
    serde_json::from_slice(body)
        .map_err(|e| ApiError::invalid_request(format!("The request body is not valid JSON: {e}.")))
}

/// Checks a speech request's own fields, everything that does not need the
/// engine.
///
/// The voice is not here: it can only be checked against the bound engine's
/// list, which the route asks for. A missing `input`, an empty one, one over
/// [`SPEECH_MAX_CHARS`], a speed outside [`SPEECH_SPEED_RANGE`], and a format
/// that is not `wav` or `pcm` are all `400`.
pub fn validate_speech(req: &SpeechRequest) -> Result<SpeechFormat, ApiError> {
    let input = req.input.as_deref().unwrap_or("");
    if input.trim().is_empty() {
        return Err(ApiError::invalid_request(
            "`input` is required and must not be empty.",
        ));
    }
    let chars = input.chars().count();
    if chars > SPEECH_MAX_CHARS {
        return Err(ApiError::invalid_request(format!(
            "`input` is {chars} characters; the limit is {SPEECH_MAX_CHARS}."
        )));
    }
    if let Some(speed) = req.speed {
        if !SPEECH_SPEED_RANGE.contains(&speed) {
            return Err(ApiError::invalid_request(format!(
                "`speed` must be between 0.25 and 4.0, not {speed}."
            )));
        }
    }
    match req.response_format.as_deref() {
        None | Some("wav") => Ok(SpeechFormat::Wav),
        Some("pcm") => Ok(SpeechFormat::Pcm),
        Some(other) => Err(ApiError::invalid_request(format!(
            "`response_format` `{other}` is not supported; use `wav` or `pcm`."
        ))),
    }
}

// ---------------------------------------------------------------------------
// Chat
// ---------------------------------------------------------------------------

/// The body of `POST /v1/chat/completions`.
///
/// `temperature` and `max_tokens` are read so their types are checked, then
/// ignored: `LlmRequest` does not carry them yet. That is the documented
/// behaviour, because refusing them would break most clients' defaults.
/// Unknown fields are ignored for the same reason.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatRequest {
    pub model: Option<String>,
    pub messages: Vec<ChatMessage>,
    pub stream: Option<bool>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Refused when non-empty. Read here so a tool-using client gets a
    /// message that says why, not a reply that ignored its tools.
    pub tools: Option<Value>,
}

/// One message in a chat request.
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    #[serde(default)]
    pub content: Option<ChatContent>,
    /// Refused when present and non-null. An assistant turn that called a
    /// tool has no text to replay, so accepting it would silently drop the
    /// call.
    #[serde(default)]
    pub tool_calls: Option<Value>,
}

/// A message's content: a plain string, or an array of typed parts.
///
/// The parts are kept as raw JSON and checked in [`flatten_messages`], so
/// that a non-text part is refused with a message about text, not with
/// serde's "data did not match any variant".
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(untagged)]
pub enum ChatContent {
    Text(String),
    Parts(Vec<Value>),
}

/// Parses a chat body and refuses the parts of it this route does not do.
///
/// Refused here: tools, and an empty `messages`. The per-message refusals
/// (tool calls, role `tool`, non-text parts) are in [`flatten_messages`],
/// which is the one place a message's content is read.
pub fn parse_chat(body: &[u8]) -> Result<ChatRequest, ApiError> {
    let req: ChatRequest = serde_json::from_slice(body).map_err(|e| {
        ApiError::invalid_request(format!("The request body is not valid JSON: {e}."))
    })?;
    if let Some(tools) = &req.tools {
        let empty = tools.is_null() || tools.as_array().is_some_and(|a| a.is_empty());
        if !empty {
            return Err(ApiError::invalid_request(
                "Tool calls are not supported on this endpoint; remove `tools`.",
            ));
        }
    }
    if req.messages.is_empty() {
        return Err(ApiError::invalid_request(
            "`messages` must contain at least one message.",
        ));
    }
    Ok(req)
}

/// Flattens a conversation into the single prompt `LlmRequest` takes.
///
/// Each message becomes a `System:`, `User:` or `Assistant:` paragraph, and
/// the prompt ends with a bare `Assistant:` so the model continues as the
/// assistant. This is the same flattening the providers that only expose a
/// completion endpoint receive, so the two paths produce the same prompt.
///
/// Refused: a message with tool calls, role `tool`, any role other than
/// `system`, `developer`, `user` and `assistant`, and any content part that
/// is not `{"type":"text","text":…}`.
pub fn flatten_messages(messages: &[ChatMessage]) -> Result<String, ApiError> {
    if messages.is_empty() {
        return Err(ApiError::invalid_request(
            "`messages` must contain at least one message.",
        ));
    }
    let mut out = String::new();
    for message in messages {
        if message.tool_calls.as_ref().is_some_and(|t| !t.is_null()) {
            return Err(ApiError::invalid_request(
                "Tool calls are not supported on this endpoint.",
            ));
        }
        let label = match message.role.as_str() {
            "system" | "developer" => "System",
            "user" => "User",
            "assistant" => "Assistant",
            "tool" => {
                return Err(ApiError::invalid_request(
                    "Messages with role `tool` are not supported on this endpoint.",
                ))
            }
            other => {
                return Err(ApiError::invalid_request(format!(
                    "The role `{other}` is not supported; use system, user or assistant."
                )))
            }
        };
        out.push_str(label);
        out.push_str(": ");
        out.push_str(&message_text(message)?);
        out.push_str("\n\n");
    }
    out.push_str("Assistant:");
    Ok(out)
}

/// The text of one message, with text parts joined by a newline.
fn message_text(message: &ChatMessage) -> Result<String, ApiError> {
    match &message.content {
        None => Ok(String::new()),
        Some(ChatContent::Text(text)) => Ok(text.clone()),
        Some(ChatContent::Parts(parts)) => {
            let mut texts: Vec<&str> = Vec::with_capacity(parts.len());
            for part in parts {
                let is_text = part.get("type").and_then(Value::as_str) == Some("text");
                match part.get("text").and_then(Value::as_str) {
                    Some(text) if is_text => texts.push(text),
                    _ => {
                        return Err(ApiError::invalid_request(
                            "Only text content is supported; a message contains a non-text part.",
                        ))
                    }
                }
            }
            Ok(texts.join("\n"))
        }
    }
}

/// The non-streamed completion body.
///
/// `usage` is written only when the provider reported it, mirroring
/// `LlmResponse::usage`. A made-up count would be worse than a missing one.
pub fn chat_completion_body(
    id: &str,
    created: u64,
    model: &str,
    reply: &str,
    usage: Option<TokenUsage>,
) -> Value {
    let mut body = json!({
        "id": id,
        "object": "chat.completion",
        "created": created,
        "model": model,
        "choices": [{
            "index": 0,
            "message": {"role": "assistant", "content": reply},
            "finish_reason": "stop",
        }],
    });
    if let Some(usage) = usage {
        body["usage"] = json!({
            "prompt_tokens": usage.prompt,
            "completion_tokens": usage.completion,
            "total_tokens": usage.total(),
        });
    }
    body
}

/// The streamed completion body: one chunk with the whole reply, a final
/// chunk with `finish_reason: "stop"`, then `data: [DONE]`.
///
/// This is the wire format of a real stream, so a client that expects
/// progressive tokens still parses it. It just gets the text all at once.
/// Real token streaming is the named follow-up, and it needs a streaming
/// method on `LlmEngine`, not a change here.
pub fn chat_stream_body(id: &str, created: u64, model: &str, reply: &str) -> Vec<u8> {
    let chunk = |delta: Value, finish_reason: Value| {
        json!({
            "id": id,
            "object": "chat.completion.chunk",
            "created": created,
            "model": model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish_reason}],
        })
    };
    let mut out = String::new();
    push_event(
        &mut out,
        &chunk(json!({"role": "assistant", "content": reply}), Value::Null),
    );
    push_event(&mut out, &chunk(json!({}), json!("stop")));
    out.push_str("data: [DONE]\n\n");
    out.into_bytes()
}

fn push_event(out: &mut String, event: &Value) {
    out.push_str("data: ");
    out.push_str(&event.to_string());
    out.push_str("\n\n");
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn fields(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn listed() -> Vec<ListedModel> {
        vec![
            ListedModel {
                id: model_id("whisper-local", Some("base")),
                engine_id: "whisper-local".into(),
                model: Some("base".into()),
                capability: Capability::Stt,
                is_default: true,
            },
            ListedModel {
                id: model_id("sherpa-tts", None),
                engine_id: "sherpa-tts".into(),
                model: None,
                capability: Capability::Tts,
                is_default: false,
            },
        ]
    }

    fn seg(start_ms: u64, end_ms: u64, text: &str, speaker: Option<&str>) -> TranscriptSegment {
        TranscriptSegment {
            start_ms,
            end_ms,
            text: text.into(),
            speaker: speaker.map(str::to_string),
        }
    }

    fn text_of(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("renderer output is UTF-8")
    }

    // --- errors ------------------------------------------------------------

    #[test]
    fn each_status_has_its_code_kind_and_retry_hint() {
        let cases = [
            (
                ApiError::invalid_request("x"),
                400,
                "invalid_request_error",
                None,
                None,
            ),
            (
                ApiError::auth("x"),
                401,
                "authentication_error",
                Some("invalid_api_key"),
                None,
            ),
            (
                ApiError::model_not_found("m"),
                404,
                "invalid_request_error",
                Some("model_not_found"),
                None,
            ),
            (
                ApiError::not_configured("x"),
                409,
                "invalid_request_error",
                Some("not_configured"),
                None,
            ),
            (
                ApiError::busy("x", 3),
                409,
                "invalid_request_error",
                Some("busy"),
                Some(3),
            ),
            (
                ApiError::too_large(1024),
                413,
                "invalid_request_error",
                Some("request_too_large"),
                None,
            ),
            (
                ApiError::rate_limited(7),
                429,
                "rate_limit_error",
                Some("rate_limit_exceeded"),
                Some(7),
            ),
            (
                ApiError::provider("x"),
                502,
                "api_error",
                Some("provider_error"),
                None,
            ),
        ];
        for (err, status, kind, code, retry) in cases {
            assert_eq!(err.status, status, "status for {err:?}");
            assert_eq!(err.kind, kind, "kind for {err:?}");
            assert_eq!(err.code.as_deref(), code, "code for {err:?}");
            assert_eq!(err.retry_after, retry, "retry_after for {err:?}");
        }
    }

    #[test]
    fn error_body_golden() {
        let body = ApiError::invalid_request("Nope.").body();
        assert_eq!(
            body.to_string(),
            r#"{"error":{"code":null,"message":"Nope.","type":"invalid_request_error"}}"#
        );
        let body = ApiError::model_not_found("tts-9").body();
        assert_eq!(
            body.to_string(),
            r#"{"error":{"code":"model_not_found","message":"The model `tts-9` does not exist or is not available for this endpoint.","type":"invalid_request_error"}}"#
        );
    }

    #[test]
    fn redact_removes_a_secret_from_a_provider_error() {
        let token = "kea_tok_0123456789abcdef";
        let raw = format!("Upstream rejected key {token} with 401.");
        let err = ApiError::provider(redact(&raw, token));
        let body = err.body().to_string();
        assert!(!body.contains(token), "body leaked the secret: {body}");
        assert!(body.contains("[redacted]"));
        assert_eq!(redact("abc", ""), "abc", "an empty secret is a no-op");
    }

    // --- model names -------------------------------------------------------

    #[test]
    fn model_id_format_is_pinned() {
        assert_eq!(
            model_id("whisper-local", Some("base")),
            "whisper-local/base"
        );
        assert_eq!(model_id("sherpa-tts", None), "sherpa-tts");
    }

    #[test]
    fn omitted_default_and_aliases_resolve_to_the_bound_model() {
        let l = listed();
        for requested in [None, Some(""), Some("default")] {
            assert_eq!(
                resolve_model(requested, Capability::Stt, &l),
                Ok(ModelChoice::Bound)
            );
        }
        assert_eq!(
            resolve_model(Some("whisper-1"), Capability::Stt, &l),
            Ok(ModelChoice::Bound)
        );
        assert_eq!(
            resolve_model(Some("tts-1"), Capability::Tts, &l),
            Ok(ModelChoice::Bound)
        );
        assert_eq!(
            resolve_model(Some("tts-1-hd"), Capability::Tts, &l),
            Ok(ModelChoice::Bound)
        );
        assert_eq!(
            resolve_model(Some("gpt-4o-mini"), Capability::Llm, &l),
            Ok(ModelChoice::Bound)
        );
    }

    #[test]
    fn a_listed_id_resolves_to_its_engine_and_model() {
        let l = listed();
        assert_eq!(
            resolve_model(Some("whisper-local/base"), Capability::Stt, &l),
            Ok(ModelChoice::Listed {
                engine_id: "whisper-local".into(),
                model: Some("base".into()),
            })
        );
        assert_eq!(
            resolve_model(Some("sherpa-tts"), Capability::Tts, &l),
            Ok(ModelChoice::Listed {
                engine_id: "sherpa-tts".into(),
                model: None,
            })
        );
    }

    #[test]
    fn wrong_capability_is_model_not_found() {
        let l = listed();
        let err = resolve_model(Some("whisper-local/base"), Capability::Tts, &l).unwrap_err();
        assert_eq!(err.status, 404);
        assert_eq!(err.code.as_deref(), Some("model_not_found"));
        // A chat model name on the speech route, and a speech alias on chat.
        assert!(resolve_model(Some("gpt-4o"), Capability::Tts, &l).is_err());
        assert!(resolve_model(Some("tts-1"), Capability::Llm, &l).is_err());
        assert!(resolve_model(Some("sherpa-tts"), Capability::Stt, &l).is_err());
    }

    #[test]
    fn unknown_name_is_model_not_found() {
        let err = resolve_model(Some("whisper-9"), Capability::Stt, &listed()).unwrap_err();
        assert_eq!(err.status, 404);
        assert!(err.message.contains("whisper-9"));
    }

    #[test]
    fn models_list_golden() {
        let mut l = listed();
        l[0].is_default = true;
        assert_eq!(
            models_list_body(&l).to_string(),
            r#"{"data":[{"created":0,"id":"whisper-local/base","kea_capability":"stt","kea_default":true,"object":"model","owned_by":"kea"},{"created":0,"id":"sherpa-tts","kea_capability":"tts","kea_default":false,"object":"model","owned_by":"kea"}],"object":"list"}"#
        );
    }

    // --- transcription -----------------------------------------------------

    #[test]
    fn form_defaults_and_fields() {
        let form = TranscriptionForm::from_fields(&fields(&[("model", "whisper-1")])).unwrap();
        assert_eq!(form.model.as_deref(), Some("whisper-1"));
        assert_eq!(form.response_format, TranscriptionFormat::Json);
        assert!(!form.speakers);
        assert_eq!(form.language, None);

        let form = TranscriptionForm::from_fields(&fields(&[
            ("response_format", "srt"),
            ("speakers", "true"),
            ("language", ""),
            ("prompt", "names"),
        ]))
        .unwrap();
        assert_eq!(form.response_format, TranscriptionFormat::Srt);
        assert!(form.speakers);
        assert_eq!(form.language, None, "an empty value is absent");
        assert_eq!(form.prompt.as_deref(), Some("names"));
    }

    #[test]
    fn form_refuses_unknown_format_and_bad_flag() {
        let err =
            TranscriptionForm::from_fields(&fields(&[("response_format", "mp3")])).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("json, text, verbose_json, srt or vtt"));

        let err = TranscriptionForm::from_fields(&fields(&[("speakers", "yes")])).unwrap_err();
        assert_eq!(err.status, 400);
    }

    fn two_speakers() -> Vec<TranscriptSegment> {
        vec![
            seg(0, 1500, "Hello there.", Some("Person 1")),
            seg(1500, 3200, "Hi!", Some("Person 2")),
        ]
    }

    #[test]
    fn json_and_text_golden() {
        let (ct, body) = render_transcription(
            TranscriptionFormat::Json,
            "Hello there. Hi!",
            &two_speakers(),
        );
        assert_eq!(ct, "application/json");
        assert_eq!(text_of(body), r#"{"text":"Hello there. Hi!"}"#);

        let (ct, body) = render_transcription(
            TranscriptionFormat::Text,
            "Hello there. Hi!",
            &two_speakers(),
        );
        assert_eq!(ct, "text/plain; charset=utf-8");
        assert_eq!(text_of(body), "Hello there. Hi!");
    }

    #[test]
    fn verbose_json_golden_with_and_without_speakers() {
        let (ct, body) = render_transcription(
            TranscriptionFormat::VerboseJson,
            "Hello there. Hi!",
            &two_speakers(),
        );
        assert_eq!(ct, "application/json");
        assert_eq!(
            text_of(body),
            r#"{"segments":[{"end":1.5,"id":0,"speaker":"Person 1","start":0.0,"text":"Hello there."},{"end":3.2,"id":1,"speaker":"Person 2","start":1.5,"text":"Hi!"}],"task":"transcribe","text":"Hello there. Hi!"}"#
        );

        let plain = vec![seg(0, 1500, "Hello there.", None)];
        let (_, body) =
            render_transcription(TranscriptionFormat::VerboseJson, "Hello there.", &plain);
        assert_eq!(
            text_of(body),
            r#"{"segments":[{"end":1.5,"id":0,"start":0.0,"text":"Hello there."}],"task":"transcribe","text":"Hello there."}"#,
            "no speaker key unless speakers were asked for"
        );
    }

    #[test]
    fn verbose_json_with_no_segments_has_an_empty_list() {
        let (_, body) = render_transcription(TranscriptionFormat::VerboseJson, "", &[]);
        assert_eq!(
            text_of(body),
            r#"{"segments":[],"task":"transcribe","text":""}"#
        );
    }

    #[test]
    fn srt_golden_labels_speakers_in_the_cue_text() {
        let (ct, body) = render_transcription(TranscriptionFormat::Srt, "", &two_speakers());
        assert_eq!(ct, "application/x-subrip");
        assert_eq!(
            text_of(body),
            "1\n00:00:00,000 --> 00:00:01,500\nPerson 1: Hello there.\n\n2\n00:00:01,500 --> 00:00:03,200\nPerson 2: Hi!\n\n"
        );
    }

    #[test]
    fn vtt_golden_labels_speakers_and_skips_empty_segments() {
        let segs = vec![
            seg(0, 1500, "Hello there.", Some("Person 1")),
            seg(1500, 2500, "  ", Some("Person 2")),
        ];
        let (ct, body) = render_transcription(TranscriptionFormat::Vtt, "", &segs);
        assert_eq!(ct, "text/vtt; charset=utf-8");
        assert_eq!(
            text_of(body),
            "WEBVTT\n\n00:00:00.000 --> 00:00:01.500\nPerson 1: Hello there.\n\n"
        );
    }

    // --- speech ------------------------------------------------------------

    fn speech(input: Option<&str>, speed: Option<f32>, format: Option<&str>) -> SpeechRequest {
        SpeechRequest {
            model: None,
            input: input.map(str::to_string),
            voice: None,
            speed,
            response_format: format.map(str::to_string),
        }
    }

    #[test]
    fn speech_defaults_to_wav() {
        assert_eq!(
            validate_speech(&speech(Some("Hello"), None, None)),
            Ok(SpeechFormat::Wav)
        );
        assert_eq!(SpeechFormat::Wav.content_type(), "audio/wav");
        assert_eq!(
            validate_speech(&speech(Some("Hello"), None, Some("pcm"))),
            Ok(SpeechFormat::Pcm)
        );
    }

    #[test]
    fn speech_input_is_required_nonblank_and_capped() {
        for input in [None, Some(""), Some("   ")] {
            let err = validate_speech(&speech(input, None, None)).unwrap_err();
            assert_eq!(err.status, 400);
        }
        let at_cap = "a".repeat(SPEECH_MAX_CHARS);
        assert!(validate_speech(&speech(Some(&at_cap), None, None)).is_ok());
        let over = "a".repeat(SPEECH_MAX_CHARS + 1);
        let err = validate_speech(&speech(Some(&over), None, None)).unwrap_err();
        assert!(err.message.contains("4097"));
        assert!(err.message.contains("4096"));
    }

    #[test]
    fn speech_speed_range_is_inclusive() {
        assert!(validate_speech(&speech(Some("a"), Some(0.25), None)).is_ok());
        assert!(validate_speech(&speech(Some("a"), Some(4.0), None)).is_ok());
        assert_eq!(
            validate_speech(&speech(Some("a"), Some(0.2), None))
                .unwrap_err()
                .status,
            400
        );
        assert_eq!(
            validate_speech(&speech(Some("a"), Some(4.5), None))
                .unwrap_err()
                .status,
            400
        );
    }

    #[test]
    fn speech_refuses_other_formats_and_lists_the_supported_two() {
        let err = validate_speech(&speech(Some("a"), None, Some("mp3"))).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("wav"));
        assert!(err.message.contains("pcm"));
    }

    #[test]
    fn speech_body_parses_and_ignores_unknown_fields() {
        let req = parse_speech(br#"{"input":"Hi","voice":"af","instructions":"be calm"}"#).unwrap();
        assert_eq!(req.input.as_deref(), Some("Hi"));
        assert_eq!(req.voice.as_deref(), Some("af"));
        assert_eq!(parse_speech(b"not json").unwrap_err().status, 400);
    }

    // --- chat --------------------------------------------------------------

    fn msg(role: &str, content: &str) -> ChatMessage {
        ChatMessage {
            role: role.into(),
            content: Some(ChatContent::Text(content.into())),
            tool_calls: None,
        }
    }

    #[test]
    fn flatten_golden() {
        let messages = vec![
            msg("system", "Be brief."),
            msg("user", "Hi"),
            msg("assistant", "Hello"),
            msg("user", "Name?"),
        ];
        assert_eq!(
            flatten_messages(&messages).unwrap(),
            "System: Be brief.\n\nUser: Hi\n\nAssistant: Hello\n\nUser: Name?\n\nAssistant:"
        );
    }

    #[test]
    fn flatten_accepts_developer_and_text_parts() {
        let parts = ChatMessage {
            role: "user".into(),
            content: Some(ChatContent::Parts(vec![
                json!({"type": "text", "text": "one"}),
                json!({"type": "text", "text": "two"}),
            ])),
            tool_calls: None,
        };
        let messages = vec![msg("developer", "Rules."), parts];
        assert_eq!(
            flatten_messages(&messages).unwrap(),
            "System: Rules.\n\nUser: one\ntwo\n\nAssistant:"
        );
    }

    #[test]
    fn flatten_refuses_tool_calls_tool_role_and_non_text_parts() {
        let with_calls = ChatMessage {
            role: "assistant".into(),
            content: None,
            tool_calls: Some(json!([{"id": "c1"}])),
        };
        assert_eq!(flatten_messages(&[with_calls]).unwrap_err().status, 400);

        let null_calls = ChatMessage {
            role: "assistant".into(),
            content: Some(ChatContent::Text("ok".into())),
            tool_calls: Some(Value::Null),
        };
        assert!(
            flatten_messages(&[null_calls]).is_ok(),
            "a null tool_calls is absent"
        );

        let err = flatten_messages(&[msg("tool", "result")]).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("tool"));

        let err = flatten_messages(&[msg("function", "x")]).unwrap_err();
        assert_eq!(err.status, 400);

        let image = ChatMessage {
            role: "user".into(),
            content: Some(ChatContent::Parts(vec![
                json!({"type": "text", "text": "look"}),
                json!({"type": "image_url", "image_url": {"url": "https://x"}}),
            ])),
            tool_calls: None,
        };
        let err = flatten_messages(&[image]).unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("Only text content is supported"));
    }

    #[test]
    fn chat_body_refuses_tools_and_empty_messages_but_ignores_unknown_fields() {
        let ok = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hi"}],"logprobs":true,"user":"me","temperature":0.2,"max_tokens":50,"stream":null}"#;
        let req = parse_chat(ok).unwrap();
        assert_eq!(req.stream, None);
        assert_eq!(req.temperature, Some(0.2));
        assert_eq!(req.max_tokens, Some(50));

        let with_tools =
            br#"{"messages":[{"role":"user","content":"Hi"}],"tools":[{"type":"function"}]}"#;
        assert_eq!(parse_chat(with_tools).unwrap_err().status, 400);
        let empty_tools = br#"{"messages":[{"role":"user","content":"Hi"}],"tools":[]}"#;
        assert!(parse_chat(empty_tools).is_ok());

        let no_messages = br#"{"messages":[]}"#;
        assert_eq!(parse_chat(no_messages).unwrap_err().status, 400);
    }

    #[test]
    fn chat_body_is_refused_on_a_bad_message_shape() {
        let bad = br#"{"messages":[{"content":"no role"}]}"#;
        assert_eq!(parse_chat(bad).unwrap_err().status, 400);
    }

    #[test]
    fn completion_golden_with_and_without_usage() {
        let usage = TokenUsage {
            prompt: 3,
            completion: 2,
        };
        assert_eq!(
            chat_completion_body(
                "chatcmpl-1",
                1_700_000_000,
                "gpt-4o",
                "Hi there",
                Some(usage)
            )
            .to_string(),
            r#"{"choices":[{"finish_reason":"stop","index":0,"message":{"content":"Hi there","role":"assistant"}}],"created":1700000000,"id":"chatcmpl-1","model":"gpt-4o","object":"chat.completion","usage":{"completion_tokens":2,"prompt_tokens":3,"total_tokens":5}}"#
        );
        assert_eq!(
            chat_completion_body("chatcmpl-1", 1_700_000_000, "gpt-4o", "Hi there", None)
                .to_string(),
            r#"{"choices":[{"finish_reason":"stop","index":0,"message":{"content":"Hi there","role":"assistant"}}],"created":1700000000,"id":"chatcmpl-1","model":"gpt-4o","object":"chat.completion"}"#,
            "no usage key when the provider reported none"
        );
    }

    #[test]
    fn stream_golden_is_one_chunk_a_stop_then_done() {
        assert_eq!(
            text_of(chat_stream_body(
                "chatcmpl-1",
                1_700_000_000,
                "gpt-4o",
                "Hi there"
            )),
            concat!(
                r#"data: {"choices":[{"delta":{"content":"Hi there","role":"assistant"},"finish_reason":null,"index":0}],"created":1700000000,"id":"chatcmpl-1","model":"gpt-4o","object":"chat.completion.chunk"}"#,
                "\n\n",
                r#"data: {"choices":[{"delta":{},"finish_reason":"stop","index":0}],"created":1700000000,"id":"chatcmpl-1","model":"gpt-4o","object":"chat.completion.chunk"}"#,
                "\n\n",
                "data: [DONE]\n\n",
            )
        );
        assert_eq!(SSE_CONTENT_TYPE, "text/event-stream");
    }

    /// A speaker turn long enough to split must carry its label on every cue.
    #[test]
    fn every_cue_of_a_long_turn_is_labelled() {
        let text = "word ".repeat(60);
        let segments = vec![TranscriptSegment {
            start_ms: 0,
            end_ms: 20_000,
            text: text.trim().to_string(),
            speaker: Some("Person 1".into()),
        }];
        let (_, body) = render_transcription(TranscriptionFormat::Srt, &text, &segments);
        let body = String::from_utf8(body).unwrap();
        let cues: Vec<&str> = body
            .split("\n\n")
            .filter(|c| !c.trim().is_empty())
            .collect();
        assert!(cues.len() > 1, "expected the turn to split: {body}");
        for cue in cues {
            let text_line = cue.lines().nth(2).unwrap_or("");
            assert!(text_line.starts_with("Person 1: "), "unlabelled cue: {cue}");
        }
    }
}
