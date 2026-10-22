//! The model routes: OpenAI-compatible transcription, speech, chat and the
//! model list, carried out with the engines the app already has.
//!
//! Each route here is an adapter over code that exists for the app's own
//! features. Transcription runs the same chunk driver and speaker assignment
//! file transcription uses; speech runs the bound synthesizer; chat runs the
//! bound LLM through the same `LlmEngine::complete`. Nothing here loads a
//! model, opens a microphone, starts playback or writes a file: an upload is
//! decoded in memory and dropped when the request ends.
//!
//! The functions take a [`Deps`] of references rather than an `AppState`, so
//! the tests drive them with fake engines and in-memory databases. The socket
//! and the route table live in [`super::server`]; this module knows nothing
//! about HTTP beyond the bytes it is handed and the [`Reply`] it returns.
//!
//! Errors are [`ApiError`]s, which the server writes in OpenAI's shape. A
//! busy flag or a rate limit is refused with a `Retry-After`, never queued.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use hyper::body::Bytes;
use kea_core::resolve::{CapKind, Resolution, ResolveError, SlotResolver};
use kea_core::secrets::CredentialStore;
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::store::usage::{NewUsageEvent, UsageRepo};
use kea_core::transcript::{
    assign_speakers, plan_chunks, transcribe_chunks, SilentSink, DEFAULT_CHUNK_SECS,
};
use kea_engines::traits::{
    AudioPcm, EngineError, LlmRequest, SttOpts, SttSegment, TokenUsage, TtsOpts,
};
use kea_engines::EngineRegistry;
use kea_features::API_FEATURE_ID;
use kea_infer::{
    DiarizationModels, DiarizationOpts, ModelStorage, SpeakerDiarization, DIARIZATION_EMBEDDING_ID,
    DIARIZATION_SEGMENTATION_ID,
};
use kea_platform::audio::{cut_points, decode_bytes, DECODE_SAMPLE_RATE_HZ};
use serde_json::Value;

use super::openai::{
    chat_completion_body, chat_stream_body, flatten_messages, models_list_body, parse_chat,
    parse_speech, redact, render_transcription, resolve_model, validate_speech, ApiError,
    Capability, ListedModel, ModelChoice, SpeechFormat, TranscriptSegment, TranscriptionForm,
    TranscriptionFormat, SSE_CONTENT_TYPE,
};
use super::ratelimit::TokenBucket;
use crate::commands::{try_acquire_busy, CHUNK_CUT_SEARCH_SECS};

/// How long a refused busy request is told to wait. Engines take seconds per
/// chunk, so a script retrying much sooner only spins.
const BUSY_RETRY_AFTER_SECS: u64 = 5;

/// Everything a model route reads or claims, borrowed from the app.
///
/// A struct rather than a long argument list so the server and the tests
/// build the same thing. The busy flags are `Arc`s because
/// [`try_acquire_busy`] takes them that way, and the guard it returns holds
/// a clone.
pub struct Deps<'a> {
    pub engines: &'a EngineRegistry,
    pub bindings: &'a BindingRepo,
    pub usage: &'a UsageRepo,
    /// The provider keys, read only to remove one from an error message.
    pub credentials: &'a dyn CredentialStore,
    /// Where the diarization models are installed.
    pub diarization: &'a ModelStorage,
    /// The speaker runtime, or `None` on a build that ships none.
    pub diarizer: Option<&'a dyn SpeakerDiarization>,
    pub stt_busy: &'a Arc<AtomicBool>,
    pub tts_busy: &'a Arc<AtomicBool>,
    /// The per-minute LLM limit, shared with the verbs that spend credits.
    pub limiter: &'a Mutex<TokenBucket>,
}

/// A successful answer: what the server writes as the response body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub content_type: &'static str,
    pub body: Vec<u8>,
    /// Extra headers, such as `x-kea-sample-rate` on raw speech.
    pub headers: Vec<(&'static str, String)>,
}

impl Reply {
    fn json(value: &Value) -> Self {
        Self {
            content_type: "application/json",
            body: serde_json::to_vec(value).unwrap_or_default(),
            headers: Vec::new(),
        }
    }
}

/// The speaker runtime this build ships. The file path reaches the same
/// place through `commands::run_diarization`; this is the API's handle on it.
#[cfg(feature = "sherpa")]
pub fn production_diarizer() -> Option<Box<dyn SpeakerDiarization>> {
    Some(Box::new(kea_infer::SherpaOnnxDiarization::new()))
}

/// Without the `sherpa` feature there is no diarizer, and a request for
/// speaker labels is refused with the reason, not run without them.
#[cfg(not(feature = "sherpa"))]
pub fn production_diarizer() -> Option<Box<dyn SpeakerDiarization>> {
    None
}

// ---------------------------------------------------------------------------
// Model resolution
// ---------------------------------------------------------------------------

fn cap_kind(cap: Capability) -> CapKind {
    match cap {
        Capability::Stt => CapKind::Stt,
        Capability::Tts => CapKind::Tts,
        Capability::Llm => CapKind::Llm,
    }
}

/// The phrase a 409 uses to say which slot is missing.
fn cap_noun(cap: Capability) -> &'static str {
    match cap {
        Capability::Stt => "speech-to-text",
        Capability::Tts => "text-to-speech",
        Capability::Llm => "language",
    }
}

/// `(engine id, its models)` for every engine of one capability. An engine
/// that lists no models is returned with an empty list, not dropped.
fn engine_models(engines: &EngineRegistry, cap: Capability) -> Vec<(String, Vec<String>)> {
    match cap {
        Capability::Stt => engines
            .list_stt_ids()
            .into_iter()
            .filter_map(|id| {
                let models = engines.stt(&id)?.capabilities().models;
                Some((id, models))
            })
            .collect(),
        Capability::Tts => engines
            .list_tts_ids()
            .into_iter()
            .filter_map(|id| {
                let models = engines.tts(&id)?.capabilities().models;
                Some((id, models))
            })
            .collect(),
        Capability::Llm => engines
            .list_llm_ids()
            .into_iter()
            .filter_map(|id| {
                let models = engines.llm(&id)?.capabilities().models;
                Some((id, models))
            })
            .collect(),
    }
}

/// The binding `api` resolves to for one capability, or `None` when nothing
/// is bound or resolvable. Only `Bound` counts: a `NeedsChoice` is not a
/// default anyone can be told is in use.
async fn bound_default(
    resolver: &SlotResolver<'_>,
    cap: Capability,
) -> Result<Option<Binding>, ApiError> {
    match resolver
        .resolve(API_FEATURE_ID, cap_kind(cap))
        .await
        .map_err(|e| ApiError::internal(e.to_string()))?
    {
        Resolution::Bound(binding) => Ok(Some(binding)),
        Resolution::NeedsChoice(_) | Resolution::Unresolvable => Ok(None),
    }
}

/// Every model a caller may name, across all three capabilities, with the
/// bound default marked.
///
/// The default marking matches on engine and model. A binding that names an
/// engine but no model marks that engine's first listed model, because a
/// request that omits `model` reaches it, and the engine picks that model.
pub async fn listed_models(deps: &Deps<'_>) -> Result<Vec<ListedModel>, ApiError> {
    let resolver = SlotResolver::new(deps.engines, deps.bindings);
    let mut listed = Vec::new();
    for cap in [Capability::Stt, Capability::Tts, Capability::Llm] {
        let default = bound_default(&resolver, cap).await?;
        for (engine_id, models) in engine_models(deps.engines, cap) {
            if models.is_empty() {
                let is_default = matches!(
                    &default,
                    Some(b) if b.engine_id == engine_id && b.model.is_none()
                );
                listed.push(ListedModel {
                    id: super::openai::model_id(&engine_id, None),
                    engine_id: engine_id.clone(),
                    model: None,
                    capability: cap,
                    is_default,
                });
                continue;
            }
            for (index, model) in models.iter().enumerate() {
                let is_default = match &default {
                    Some(b) if b.engine_id == engine_id => match &b.model {
                        Some(bound) => bound == model,
                        None => index == 0,
                    },
                    _ => false,
                };
                listed.push(ListedModel {
                    id: super::openai::model_id(&engine_id, Some(model)),
                    engine_id: engine_id.clone(),
                    model: Some(model.clone()),
                    capability: cap,
                    is_default,
                });
            }
        }
    }
    Ok(listed)
}

/// Turns a request's `model` into the binding one call runs with.
///
/// A listed id becomes a binding for this call only, with no provider
/// reference, so the engine uses its own default provider. It is never
/// written to the bindings table. Anything bound comes from the `api` slot,
/// and a missing binding is a 409 that names the page to visit.
async fn choose(
    deps: &Deps<'_>,
    cap: Capability,
    requested: Option<&str>,
) -> Result<Binding, ApiError> {
    let listed = listed_models(deps).await?;
    match resolve_model(requested, cap, &listed)? {
        ModelChoice::Listed { engine_id, model } => Ok(Binding {
            engine_id,
            model,
            provider_ref: None,
        }),
        ModelChoice::Bound => match SlotResolver::new(deps.engines, deps.bindings)
            .require(API_FEATURE_ID, cap_kind(cap))
            .await
        {
            Ok(binding) => Ok(binding),
            Err(ResolveError::Db(e)) => Err(ApiError::internal(e.to_string())),
            Err(_) => Err(ApiError::not_configured(format!(
                "No {} model is set for API use. Choose one under API on the Models page.",
                cap_noun(cap)
            ))),
        },
    }
}

/// An engine's own failure, as the caller sees it.
///
/// A missing model or an unset provider is a configuration problem the
/// caller can fix on the Models page, so it is a 409. Everything else is the
/// engine or the provider failing, and is a 502 carrying the engine's
/// message. That message is the only thing from the provider that reaches
/// the body, and the key the binding names is removed from it first: a
/// provider that echoes a rejected key back must not hand it to the caller.
/// A binding with no provider named has no key to look up here, so its
/// message goes through as the engine wrote it.
async fn engine_failure(deps: &Deps<'_>, binding: &Binding, err: EngineError) -> ApiError {
    let message = match err {
        EngineError::Config(message) | EngineError::ModelNotInstalled(message) => {
            return ApiError::not_configured(message)
        }
        other => other.to_string(),
    };
    let key = match binding.provider_ref.as_deref() {
        Some(provider) => deps.credentials.get(provider).await.ok().flatten(),
        None => None,
    };
    let message = match key {
        Some(key) => redact(&message, &key),
        None => message,
    };
    ApiError::provider(message)
}

fn busy(what: &str) -> ApiError {
    ApiError::busy(
        format!("Another {what} is already running. Retry when it finishes."),
        BUSY_RETRY_AFTER_SECS,
    )
}

// ---------------------------------------------------------------------------
// GET /v1/models
// ---------------------------------------------------------------------------

pub async fn list(deps: &Deps<'_>) -> Result<Reply, ApiError> {
    let listed = listed_models(deps).await?;
    Ok(Reply::json(&models_list_body(&listed)))
}

// ---------------------------------------------------------------------------
// POST /v1/audio/transcriptions
// ---------------------------------------------------------------------------

/// Transcribes an uploaded file and returns it in the requested format.
///
/// `content_type` is the request's header, which carries the multipart
/// boundary. `body` is the whole body, already read under this route's size
/// limit.
pub async fn transcriptions(
    deps: &Deps<'_>,
    content_type: Option<&str>,
    body: Bytes,
) -> Result<Reply, ApiError> {
    let (fields, file) = read_multipart(content_type, body).await?;
    let form = TranscriptionForm::from_fields(&fields)?;
    let file = file.ok_or_else(|| {
        ApiError::invalid_request(
            "The `file` part is required: send the audio as multipart field `file`.",
        )
    })?;

    let binding = choose(deps, Capability::Stt, form.model.as_deref()).await?;
    let engine = deps.engines.stt(&binding.engine_id).ok_or_else(|| {
        ApiError::not_configured(format!(
            "No speech-to-text engine '{}' is installed.",
            binding.engine_id
        ))
    })?;

    // Speaker labels only appear in these three formats. The flag is
    // accepted for the others and ignored, so a client that always sends it
    // gets the same JSON it would without it.
    let labelled = form.speakers
        && matches!(
            form.response_format,
            TranscriptionFormat::VerboseJson | TranscriptionFormat::Srt | TranscriptionFormat::Vtt
        );
    // Checked before the upload is decoded, so a request that cannot be
    // honoured fails in microseconds rather than after a long transcription.
    let speakers = if labelled {
        Some(speaker_runtime(deps)?)
    } else {
        None
    };

    let Some(_busy) = try_acquire_busy(deps.stt_busy) else {
        return Err(busy("transcription"));
    };

    // Decoding is CPU work over the whole file, so it leaves the async
    // runtime, as the file path does.
    let frame = tokio::task::spawn_blocking(move || decode_bytes(&file, DECODE_SAMPLE_RATE_HZ))
        .await
        .map_err(|e| ApiError::internal(format!("decode task failed: {e}")))?
        .map_err(|e| {
            ApiError::invalid_request(format!(
                "The uploaded file could not be decoded as audio: {e}."
            ))
        })?;
    let pcm = AudioPcm {
        samples: frame.samples,
        sample_rate_hz: frame.sample_rate_hz,
    };

    let cuts = cut_points(
        &pcm.samples,
        pcm.sample_rate_hz,
        DEFAULT_CHUNK_SECS,
        CHUNK_CUT_SEARCH_SECS,
    );
    let spans = plan_chunks(pcm.samples.len(), pcm.sample_rate_hz, &cuts);
    let opts = SttOpts {
        model: binding.model.clone(),
        language: form.language.clone(),
        provider_ref: binding.provider_ref.clone(),
        ..SttOpts::default()
    };
    // No sink that emits: an API job is not a row on the Transcripts page,
    // and the chunk driver's cancel check is never set here.
    let outcome = match transcribe_chunks(engine.as_ref(), &pcm, &spans, &opts, &SilentSink).await {
        Ok(outcome) => outcome,
        Err(e) => return Err(engine_failure(deps, &binding, e).await),
    };

    let labels = match speakers {
        None => vec![None; outcome.segments.len()],
        Some((diarizer, models)) => {
            label_speakers(diarizer, &models, pcm, &outcome.segments).await?
        }
    };
    let segments: Vec<TranscriptSegment> = outcome
        .segments
        .iter()
        .zip(labels)
        .map(|(segment, speaker)| TranscriptSegment {
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.clone(),
            speaker,
        })
        .collect();

    let (content_type, body) = render_transcription(form.response_format, &outcome.text, &segments);
    Ok(Reply {
        content_type,
        body,
        headers: Vec::new(),
    })
}

/// Splits a multipart body into its text fields and its `file` part.
///
/// The body is one in-memory chunk, so the parser is handed it as a single
/// stream item. The first `file` part wins, as the first of a repeated text
/// field does.
async fn read_multipart(
    content_type: Option<&str>,
    body: Bytes,
) -> Result<(Vec<(String, String)>, Option<Bytes>), ApiError> {
    let content_type = content_type
        .ok_or_else(|| ApiError::invalid_request("Send the audio as multipart/form-data."))?;
    let boundary = multer::parse_boundary(content_type).map_err(|_| {
        ApiError::invalid_request("Send the audio as multipart/form-data with a boundary.")
    })?;
    let stream = futures_util::stream::once(async move { Ok::<Bytes, std::io::Error>(body) });
    let mut multipart = multer::Multipart::new(stream, boundary);

    let mut fields = Vec::new();
    let mut file: Option<Bytes> = None;
    while let Some(field) = multipart.next_field().await.map_err(multipart_error)? {
        let name = field.name().unwrap_or("").to_string();
        if name == "file" {
            let bytes = field.bytes().await.map_err(multipart_error)?;
            file.get_or_insert(bytes);
        } else {
            let text = field.text().await.map_err(multipart_error)?;
            fields.push((name, text));
        }
    }
    Ok((fields, file))
}

fn multipart_error(e: multer::Error) -> ApiError {
    ApiError::invalid_request(format!("The multipart body could not be read: {e}."))
}

/// The speaker runtime and its installed models, or the 409 that says which
/// is missing.
///
/// The models are located with the same call the file path uses, against the
/// same storage roots, so "installed" means the same thing in both places.
fn speaker_runtime<'a>(
    deps: &Deps<'a>,
) -> Result<(&'a dyn SpeakerDiarization, DiarizationModels), ApiError> {
    let diarizer = deps.diarizer.ok_or_else(|| {
        ApiError::not_configured(
            "this build has no diarization runtime (the `sherpa` feature is off)",
        )
    })?;
    let models = DiarizationModels::locate(
        &deps.diarization.onnx_dir_for(DIARIZATION_SEGMENTATION_ID),
        &deps.diarization.onnx_dir_for(DIARIZATION_EMBEDDING_ID),
    )
    .map_err(|e| {
        ApiError::not_configured(format!(
            "speaker labels need the speaker models, which are not all installed: {e}. Download them on the Models page."
        ))
    })?;
    Ok((diarizer, models))
}

/// One label per segment, `Person N` from the diarizer's zero-based index.
async fn label_speakers(
    diarizer: &dyn SpeakerDiarization,
    models: &DiarizationModels,
    audio: AudioPcm,
    segments: &[SttSegment],
) -> Result<Vec<Option<String>>, ApiError> {
    if segments.is_empty() {
        return Ok(Vec::new());
    }
    let spans = diarizer
        .diarize(
            kea_infer::AudioPcm {
                samples: audio.samples,
                sample_rate_hz: audio.sample_rate_hz,
            },
            models,
            DiarizationOpts::default(),
        )
        .await
        .map_err(|e| ApiError::internal(format!("speaker separation failed: {e}")))?;
    Ok(assign_speakers(segments, &spans)
        .iter()
        .map(|key| key.as_deref().and_then(person_label))
        .collect())
}

/// `spk3` → `Person 4`. The key is the one `assign_speakers` writes, and the
/// number is shown one-based, as the meeting labels are.
fn person_label(key: &str) -> Option<String> {
    let index: u32 = key.strip_prefix("spk")?.parse().ok()?;
    Some(format!("Person {}", index + 1))
}

// ---------------------------------------------------------------------------
// POST /v1/audio/speech
// ---------------------------------------------------------------------------

/// Synthesizes `input` and returns the audio in the response body.
///
/// The audio is encoded in memory and returned. It is not played, and the
/// read-aloud path is not reached from here.
pub async fn speech(deps: &Deps<'_>, body: &[u8]) -> Result<Reply, ApiError> {
    let req = parse_speech(body)?;
    let format = validate_speech(&req)?;

    let binding = choose(deps, Capability::Tts, req.model.as_deref()).await?;
    let engine = deps.engines.tts(&binding.engine_id).ok_or_else(|| {
        ApiError::not_configured(format!(
            "No text-to-speech engine '{}' is installed.",
            binding.engine_id
        ))
    })?;

    let voice = req.voice.clone().filter(|v| !v.is_empty());
    // An engine that lists its voices is checked here, so an unknown name is
    // a 400 rather than a substitute voice. An engine that does not list them
    // gets the name as given, and its own refusal comes back as a 502.
    if let (Some(name), Some(voices)) = (
        voice.as_deref(),
        engine.voice_names(binding.model.as_deref()),
    ) {
        if !voices.iter().any(|v| v == name) {
            let available = if voices.is_empty() {
                "this model has no selectable voices".to_string()
            } else {
                format!("available voices are {}", voices.join(", "))
            };
            return Err(ApiError::invalid_request(format!(
                "The voice `{name}` is not available for this model; {available}."
            )));
        }
    }

    let Some(_busy) = try_acquire_busy(deps.tts_busy) else {
        return Err(busy("speech request"));
    };

    let text = req.input.clone().unwrap_or_default();
    let opts = TtsOpts {
        model: binding.model.clone(),
        voice,
        provider_ref: binding.provider_ref.clone(),
        speed: req.speed,
        ..TtsOpts::default()
    };
    let pcm = match engine.synthesize(&text, opts).await {
        Ok(pcm) => pcm,
        Err(e) => return Err(engine_failure(deps, &binding, e).await),
    };

    match format {
        SpeechFormat::Wav => Ok(Reply {
            content_type: format.content_type(),
            body: encode_wav(&pcm)?,
            headers: Vec::new(),
        }),
        SpeechFormat::Pcm => Ok(Reply {
            content_type: format.content_type(),
            body: encode_pcm(&pcm),
            headers: vec![("x-kea-sample-rate", pcm.sample_rate_hz.to_string())],
        }),
    }
}

/// Mono 16-bit PCM in a RIFF container, written to memory.
fn encode_wav(pcm: &AudioPcm) -> Result<Vec<u8>, ApiError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: pcm.sample_rate_hz,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut out = Vec::with_capacity(44 + pcm.samples.len() * 2);
    {
        // The writer borrows the buffer, so it is finalized inside a scope
        // that ends before the buffer is returned.
        let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut out), spec)
            .map_err(|e| ApiError::internal(format!("could not start the WAV: {e}")))?;
        for sample in &pcm.samples {
            writer
                .write_sample(to_i16(*sample))
                .map_err(|e| ApiError::internal(format!("could not write the WAV: {e}")))?;
        }
        writer
            .finalize()
            .map_err(|e| ApiError::internal(format!("could not finish the WAV: {e}")))?;
    }
    Ok(out)
}

/// Raw 16-bit little-endian mono samples. The rate travels in a header.
fn encode_pcm(pcm: &AudioPcm) -> Vec<u8> {
    pcm.samples
        .iter()
        .flat_map(|s| to_i16(*s).to_le_bytes())
        .collect()
}

/// Float samples to 16-bit, clamped rather than wrapped, so an engine that
/// overshoots 1.0 clips instead of clicking.
fn to_i16(sample: f32) -> i16 {
    (sample.clamp(-1.0, 1.0) * f32::from(i16::MAX)).round() as i16
}

// ---------------------------------------------------------------------------
// POST /v1/chat/completions
// ---------------------------------------------------------------------------

static COMPLETION_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Completes a conversation with the bound LLM.
///
/// Order matters: the body and the model are checked before a rate-limit
/// token is taken, so a malformed request does not spend one.
pub async fn chat(deps: &Deps<'_>, body: &[u8]) -> Result<Reply, ApiError> {
    let req = parse_chat(body)?;
    let prompt = flatten_messages(&req.messages)?;

    let binding = choose(deps, Capability::Llm, req.model.as_deref()).await?;
    let engine = deps.engines.llm(&binding.engine_id).ok_or_else(|| {
        ApiError::not_configured(format!(
            "No language engine '{}' is installed.",
            binding.engine_id
        ))
    })?;

    take_token(deps)?;

    let response = match engine
        .complete(LlmRequest {
            prompt,
            model: binding.model.clone(),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
    {
        Ok(response) => response,
        Err(e) => return Err(engine_failure(deps, &binding, e).await),
    };
    record_usage(deps, &binding, response.usage).await;

    let model = super::openai::model_id(&binding.engine_id, binding.model.as_deref());
    let id = completion_id();
    let created = now_secs();
    if req.stream.unwrap_or(false) {
        Ok(Reply {
            content_type: SSE_CONTENT_TYPE,
            body: chat_stream_body(&id, created, &model, &response.text),
            headers: Vec::new(),
        })
    } else {
        Ok(Reply::json(&chat_completion_body(
            &id,
            created,
            &model,
            &response.text,
            response.usage,
        )))
    }
}

/// One token from the shared per-minute bucket, or a 429 with the wait.
fn take_token(deps: &Deps<'_>) -> Result<(), ApiError> {
    let now = Instant::now();
    let mut bucket = deps.limiter.lock().unwrap_or_else(|p| p.into_inner());
    if bucket.try_take(now) {
        return Ok(());
    }
    let wait = bucket.retry_after(now).as_secs().max(1);
    Err(ApiError::rate_limited(wait))
}

/// Writes one usage row, attributed to the API. The counts are the
/// provider's, or absent when it reported none.
async fn record_usage(deps: &Deps<'_>, binding: &Binding, usage: Option<TokenUsage>) {
    let event = NewUsageEvent {
        model: binding.model.clone(),
        provider_ref: binding.provider_ref.clone(),
        prompt_tokens: usage.map(|u| i64::from(u.prompt)),
        completion_tokens: usage.map(|u| i64::from(u.completion)),
        ..NewUsageEvent::new(API_FEATURE_ID, binding.engine_id.as_str())
    };
    if let Err(error) = deps.usage.record(&event).await {
        tracing::warn!(%error, "local API: could not record token usage");
    }
}

fn completion_id() -> String {
    let n = COMPLETION_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("chatcmpl-{nanos:x}{n:x}")
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::traits::{LlmEngine, LlmResponse, SttEngine, Transcript, TtsEngine};
    use kea_engines::EngineCaps;
    use kea_infer::SpeakerSpan;

    // ---- fakes -----------------------------------------------------------

    struct FakeStt {
        id: &'static str,
        models: Vec<String>,
        segments: Vec<SttSegment>,
        text: String,
    }

    #[async_trait]
    impl SttEngine for FakeStt {
        fn id(&self) -> &str {
            self.id
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: self.models.clone(),
            }
        }
        async fn transcribe(
            &self,
            _audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            Ok(Transcript {
                text: self.text.clone(),
                segments: self.segments.clone(),
            })
        }
    }

    struct FakeTts {
        voices: Option<Vec<String>>,
    }

    #[async_trait]
    impl TtsEngine for FakeTts {
        fn id(&self) -> &str {
            "fake-tts"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["fake-voice".into()],
            }
        }
        fn voice_names(&self, _model: Option<&str>) -> Option<Vec<String>> {
            self.voices.clone()
        }
        async fn synthesize(&self, text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
            // A ramp, so the sample count and the encoding are both checkable.
            Ok(AudioPcm {
                samples: (0..text.len() * 100)
                    .map(|i| (i as f32 / 100.0).sin())
                    .collect(),
                sample_rate_hz: 22_050,
            })
        }
    }

    struct FakeLlm {
        result: Result<LlmResponse, String>,
    }

    #[async_trait]
    impl LlmEngine for FakeLlm {
        fn id(&self) -> &str {
            "fake-llm"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: Vec::new() }
        }
        async fn complete(&self, _req: LlmRequest) -> Result<LlmResponse, EngineError> {
            match &self.result {
                Ok(response) => Ok(response.clone()),
                Err(body) => Err(EngineError::http(500, body.clone())),
            }
        }
    }

    struct FakeDiarizer;

    #[async_trait]
    impl SpeakerDiarization for FakeDiarizer {
        async fn diarize(
            &self,
            _pcm: kea_infer::AudioPcm,
            _models: &DiarizationModels,
            _opts: DiarizationOpts,
        ) -> Result<Vec<SpeakerSpan>, kea_infer::error::InferError> {
            Ok(vec![
                SpeakerSpan {
                    start_ms: 0,
                    end_ms: 1_000,
                    speaker: 0,
                },
                SpeakerSpan {
                    start_ms: 1_000,
                    end_ms: 2_000,
                    speaker: 1,
                },
            ])
        }
    }

    // ---- harness ---------------------------------------------------------

    struct Harness {
        engines: EngineRegistry,
        bindings: BindingRepo,
        usage: UsageRepo,
        credentials: kea_core::secrets::InMemoryCredentialStore,
        storage: ModelStorage,
        stt_busy: Arc<AtomicBool>,
        tts_busy: Arc<AtomicBool>,
        limiter: Mutex<TokenBucket>,
        _dir: tempfile::TempDir,
    }

    async fn harness() -> Harness {
        let config = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config).await.unwrap();
        let data = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data).await.unwrap();
        let dir = tempfile::tempdir().unwrap();
        Harness {
            engines: EngineRegistry::default(),
            bindings: BindingRepo::new(config),
            usage: UsageRepo::new(data),
            credentials: kea_core::secrets::InMemoryCredentialStore::default(),
            storage: ModelStorage::new(dir.path().to_path_buf()),
            stt_busy: Arc::new(AtomicBool::new(false)),
            tts_busy: Arc::new(AtomicBool::new(false)),
            limiter: Mutex::new(TokenBucket::per_minute(1_000, Instant::now())),
            _dir: dir,
        }
    }

    impl Harness {
        fn deps<'a>(&'a self, diarizer: Option<&'a dyn SpeakerDiarization>) -> Deps<'a> {
            Deps {
                engines: &self.engines,
                bindings: &self.bindings,
                usage: &self.usage,
                credentials: &self.credentials,
                diarization: &self.storage,
                diarizer,
                stt_busy: &self.stt_busy,
                tts_busy: &self.tts_busy,
                limiter: &self.limiter,
            }
        }

        async fn bind(&self, cap: &str, engine: &str, model: Option<&str>) {
            self.bindings
                .set(
                    API_FEATURE_ID,
                    cap,
                    Binding {
                        engine_id: engine.into(),
                        model: model.map(str::to_string),
                        provider_ref: None,
                    },
                )
                .await
                .unwrap();
        }
    }

    fn segment(start_ms: u64, end_ms: u64, text: &str) -> SttSegment {
        SttSegment::new(start_ms, end_ms, text)
    }

    /// A 2-second 16 kHz mono WAV, so the decode path runs for real.
    fn wav_fixture() -> Vec<u8> {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 16_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut out = Vec::new();
        {
            let mut writer = hound::WavWriter::new(std::io::Cursor::new(&mut out), spec).unwrap();
            for i in 0..32_000u32 {
                let sample = ((i as f32 / 40.0).sin() * 8_000.0) as i16;
                writer.write_sample(sample).unwrap();
            }
            writer.finalize().unwrap();
        }
        out
    }

    const BOUNDARY: &str = "kea-test-boundary";

    /// A multipart body with text fields and an optional `file` part.
    fn multipart(fields: &[(&str, &str)], file: Option<&[u8]>) -> (String, Bytes) {
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        if let Some(bytes) = file {
            body.extend_from_slice(
                format!("--{BOUNDARY}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.wav\"\r\nContent-Type: audio/wav\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(bytes);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
        (
            format!("multipart/form-data; boundary={BOUNDARY}"),
            Bytes::from(body),
        )
    }

    fn transcription_body(fields: &[(&str, &str)]) -> (String, Bytes) {
        multipart(fields, Some(&wav_fixture()))
    }

    fn text_of(reply: &Reply) -> String {
        String::from_utf8(reply.body.clone()).unwrap()
    }

    fn json_of(reply: &Reply) -> Value {
        serde_json::from_slice(&reply.body).unwrap()
    }

    fn expect_err<T: std::fmt::Debug>(result: Result<T, ApiError>) -> ApiError {
        result.expect_err("expected an error")
    }

    /// A harness with one STT engine, bound for `api` under the model `base`.
    /// The engine returns `segments` and `text` for every upload.
    async fn with_stt(segments: Vec<SttSegment>, text: &str) -> Harness {
        let mut h = harness().await;
        h.engines.register_stt(Arc::new(FakeStt {
            id: "fake-stt",
            models: vec!["base".into()],
            segments,
            text: text.into(),
        }));
        h.bind("stt", "fake-stt", Some("base")).await;
        h
    }

    // ---- /v1/audio/transcriptions -----------------------------------------

    #[tokio::test]
    async fn json_transcription_returns_the_engine_text() {
        let h = with_stt(vec![segment(0, 1_000, "hello")], "hello there").await;
        let (ct, body) = transcription_body(&[("model", "whisper-1")]);
        let reply = transcriptions(&h.deps(None), Some(&ct), body)
            .await
            .unwrap();
        assert_eq!(reply.content_type, "application/json");
        assert_eq!(json_of(&reply), serde_json::json!({"text": "hello there"}));
    }

    #[tokio::test]
    async fn verbose_json_lists_segments_and_an_empty_list_when_there_are_none() {
        let h = with_stt(
            vec![segment(0, 1_500, "one"), segment(1_500, 2_000, "two")],
            "one two",
        )
        .await;
        let (ct, body) = transcription_body(&[("response_format", "verbose_json")]);
        let reply = transcriptions(&h.deps(None), Some(&ct), body)
            .await
            .unwrap();
        let v = json_of(&reply);
        assert_eq!(v["segments"][0]["start"], 0.0);
        assert_eq!(v["segments"][0]["end"], 1.5);
        assert_eq!(v["segments"][1]["text"], "two");
        assert!(v["segments"][0].get("speaker").is_none());

        let empty = with_stt(Vec::new(), "").await;
        let (ct, body) = transcription_body(&[("response_format", "verbose_json")]);
        let reply = transcriptions(&empty.deps(None), Some(&ct), body)
            .await
            .unwrap();
        assert_eq!(json_of(&reply)["segments"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn srt_is_a_subtitle_document_with_its_own_content_type() {
        let h = with_stt(vec![segment(0, 1_000, "hello")], "hello").await;
        let (ct, body) = transcription_body(&[("response_format", "srt")]);
        let reply = transcriptions(&h.deps(None), Some(&ct), body)
            .await
            .unwrap();
        assert_eq!(reply.content_type, "application/x-subrip");
        assert!(
            text_of(&reply).starts_with("1\n00:00:00,000 --> 00:00:01,000\nhello"),
            "{}",
            text_of(&reply)
        );
    }

    #[tokio::test]
    async fn an_undecodable_upload_is_a_400_that_names_the_failure() {
        let h = with_stt(Vec::new(), "").await;
        let (ct, body) = multipart(&[], Some(b"this is not audio at all"));
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 400);
        assert!(
            err.message.contains("could not be decoded"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn a_missing_file_part_is_a_400() {
        let h = with_stt(Vec::new(), "").await;
        let (ct, body) = multipart(&[("model", "whisper-1")], None);
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 400);
        assert!(err.message.contains("`file`"));
    }

    #[tokio::test]
    async fn a_second_transcription_while_one_runs_is_a_409_with_retry_after() {
        let h = with_stt(vec![segment(0, 1_000, "x")], "x").await;
        // Stands in for a transcription already in flight.
        let held = try_acquire_busy(&h.stt_busy).unwrap();
        let (ct, body) = transcription_body(&[]);
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 409);
        assert_eq!(err.retry_after, Some(BUSY_RETRY_AFTER_SECS));
        drop(held);
        let (ct, body) = transcription_body(&[]);
        assert!(transcriptions(&h.deps(None), Some(&ct), body).await.is_ok());
    }

    #[tokio::test]
    async fn speaker_labels_without_a_runtime_are_a_409_saying_so() {
        let h = with_stt(vec![segment(0, 1_000, "x")], "x").await;
        let (ct, body) =
            transcription_body(&[("response_format", "verbose_json"), ("speakers", "true")]);
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 409);
        assert!(
            err.message.contains("no diarization runtime"),
            "{}",
            err.message
        );
    }

    #[tokio::test]
    async fn speaker_labels_with_missing_models_are_a_409_naming_them() {
        let h = with_stt(vec![segment(0, 1_000, "x")], "x").await;
        let fake = FakeDiarizer;
        let (ct, body) =
            transcription_body(&[("response_format", "verbose_json"), ("speakers", "true")]);
        let err = expect_err(transcriptions(&h.deps(Some(&fake)), Some(&ct), body).await);
        assert_eq!(err.status, 409);
        assert!(err.message.contains("speaker"), "{}", err.message);
        assert!(err.message.contains("not"), "{}", err.message);
    }

    /// Writes the two diarization model files where the storage looks for
    /// them, so `locate` succeeds.
    fn install_diarization_models(storage: &ModelStorage) {
        for id in [DIARIZATION_SEGMENTATION_ID, DIARIZATION_EMBEDDING_ID] {
            let dir = storage.onnx_dir_for(id);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join("model.onnx"), b"stub").unwrap();
        }
    }

    #[tokio::test]
    async fn speakers_are_labelled_person_n_per_segment() {
        let h = with_stt(
            vec![segment(0, 900, "hi"), segment(1_100, 1_900, "yo")],
            "hi yo",
        )
        .await;
        install_diarization_models(&h.storage);
        let fake = FakeDiarizer;
        let (ct, body) =
            transcription_body(&[("response_format", "verbose_json"), ("speakers", "true")]);
        let reply = transcriptions(&h.deps(Some(&fake)), Some(&ct), body)
            .await
            .unwrap();
        let v = json_of(&reply);
        assert_eq!(v["segments"][0]["speaker"], "Person 1");
        assert_eq!(v["segments"][1]["speaker"], "Person 2");
    }

    #[tokio::test]
    async fn speakers_on_a_json_request_are_accepted_and_ignored() {
        // Without the runtime installed, json still succeeds: the flag only
        // means something for the formats that show labels.
        let h = with_stt(vec![segment(0, 1_000, "x")], "x").await;
        let (ct, body) = transcription_body(&[("speakers", "true")]);
        let reply = transcriptions(&h.deps(None), Some(&ct), body)
            .await
            .unwrap();
        assert_eq!(json_of(&reply), serde_json::json!({"text": "x"}));
    }

    #[tokio::test]
    async fn an_unknown_model_is_a_404_and_a_wrong_capability_is_too() {
        let h = with_stt(Vec::new(), "").await;
        let (ct, body) = transcription_body(&[("model", "tts-1-hd-typo")]);
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 404);
        assert_eq!(err.code.as_deref(), Some("model_not_found"));

        // A chat model name sent to speech-to-text is the wrong-route case:
        // `gpt-*` is an alias for chat only.
        let (ct, body) = transcription_body(&[("model", "gpt-4o")]);
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 404);
        assert_eq!(err.code.as_deref(), Some("model_not_found"));
    }

    #[tokio::test]
    async fn transcription_with_nothing_bound_is_a_409() {
        let mut h = harness().await;
        h.engines.register_stt(Arc::new(FakeStt {
            id: "a",
            models: Vec::new(),
            segments: Vec::new(),
            text: String::new(),
        }));
        h.engines.register_stt(Arc::new(FakeStt {
            id: "b",
            models: Vec::new(),
            segments: Vec::new(),
            text: String::new(),
        }));
        let (ct, body) = transcription_body(&[]);
        let err = expect_err(transcriptions(&h.deps(None), Some(&ct), body).await);
        assert_eq!(err.status, 409);
        assert!(err.message.contains("Models page"), "{}", err.message);
    }

    // ---- /v1/audio/speech --------------------------------------------------

    async fn tts_harness(voices: Option<Vec<String>>) -> Harness {
        let mut h = harness().await;
        h.engines.register_tts(Arc::new(FakeTts { voices }));
        h.bind("tts", "fake-tts", None).await;
        h
    }

    fn wav_header(bytes: &[u8]) -> (u16, u32, u16) {
        let reader = hound::WavReader::new(std::io::Cursor::new(bytes)).unwrap();
        let spec = reader.spec();
        (spec.channels, spec.sample_rate, spec.bits_per_sample)
    }

    #[tokio::test]
    async fn speech_wav_has_a_valid_header_and_one_sample_per_engine_sample() {
        let h = tts_harness(None).await;
        let reply = speech(&h.deps(None), br#"{"input":"Hello"}"#)
            .await
            .unwrap();
        assert_eq!(reply.content_type, "audio/wav");
        assert_eq!(wav_header(&reply.body), (1, 22_050, 16));
        let mut reader = hound::WavReader::new(std::io::Cursor::new(&reply.body)).unwrap();
        assert_eq!(reader.samples::<i16>().count(), "Hello".len() * 100);
    }

    #[tokio::test]
    async fn speech_pcm_is_raw_samples_with_the_rate_in_a_header() {
        let h = tts_harness(None).await;
        let reply = speech(&h.deps(None), br#"{"input":"Hi","response_format":"pcm"}"#)
            .await
            .unwrap();
        assert_eq!(reply.content_type, "application/octet-stream");
        assert_eq!(reply.body.len(), "Hi".len() * 100 * 2);
        assert_eq!(
            reply.headers,
            vec![("x-kea-sample-rate", "22050".to_string())]
        );
    }

    #[tokio::test]
    async fn an_unknown_voice_is_a_400_with_no_substitute() {
        let h = tts_harness(Some(vec!["Alex".into(), "Sam".into()])).await;
        let err = expect_err(speech(&h.deps(None), br#"{"input":"Hi","voice":"Bob"}"#).await);
        assert_eq!(err.status, 400);
        assert!(err.message.contains("`Bob`"), "{}", err.message);
        assert!(err.message.contains("Alex"), "{}", err.message);

        let ok = speech(&h.deps(None), br#"{"input":"Hi","voice":"Sam"}"#).await;
        assert!(ok.is_ok());
    }

    #[tokio::test]
    async fn a_voice_on_an_engine_that_lists_none_is_passed_through() {
        let h = tts_harness(None).await;
        assert!(speech(&h.deps(None), br#"{"input":"Hi","voice":"alloy"}"#)
            .await
            .is_ok());
    }

    #[tokio::test]
    async fn an_mp3_request_is_a_400_listing_the_supported_formats() {
        let h = tts_harness(None).await;
        let err =
            expect_err(speech(&h.deps(None), br#"{"input":"Hi","response_format":"mp3"}"#).await);
        assert_eq!(err.status, 400);
        assert!(err.message.contains("`wav` or `pcm`"), "{}", err.message);
    }

    #[tokio::test]
    async fn an_out_of_range_speed_is_a_400() {
        let h = tts_harness(None).await;
        let err = expect_err(speech(&h.deps(None), br#"{"input":"Hi","speed":5.0}"#).await);
        assert_eq!(err.status, 400);
    }

    #[tokio::test]
    async fn speech_while_another_runs_is_a_409_with_retry_after() {
        let h = tts_harness(None).await;
        let held = try_acquire_busy(&h.tts_busy).unwrap();
        let err = expect_err(speech(&h.deps(None), br#"{"input":"Hi"}"#).await);
        assert_eq!(err.status, 409);
        assert_eq!(err.retry_after, Some(BUSY_RETRY_AFTER_SECS));
        drop(held);
    }

    #[tokio::test]
    async fn speech_with_nothing_bound_is_a_409() {
        let h = harness().await;
        let err = expect_err(speech(&h.deps(None), br#"{"input":"Hi"}"#).await);
        assert_eq!(err.status, 409);
        assert!(err.message.contains("Models page"));
    }

    // ---- /v1/chat/completions ----------------------------------------------

    async fn llm_harness(result: Result<LlmResponse, String>) -> Harness {
        let mut h = harness().await;
        h.engines.register_llm(Arc::new(FakeLlm { result }));
        h.bind("llm", "fake-llm", Some("fake-model")).await;
        h
    }

    const CHAT: &[u8] = br#"{"model":"gpt-4o","messages":[{"role":"user","content":"Hi"}]}"#;

    #[tokio::test]
    async fn chat_returns_the_completion_and_records_usage_under_api() {
        let usage = TokenUsage {
            prompt: 3,
            completion: 2,
        };
        let h = llm_harness(Ok(LlmResponse {
            text: "Hello!".into(),
            usage: Some(usage),
        }))
        .await;
        let reply = chat(&h.deps(None), CHAT).await.unwrap();
        let v = json_of(&reply);
        assert_eq!(v["object"], "chat.completion");
        assert_eq!(v["choices"][0]["message"]["content"], "Hello!");
        assert_eq!(v["usage"]["total_tokens"], 5);

        let totals = h.usage.totals(1).await.unwrap();
        assert_eq!(totals.len(), 1);
        assert_eq!(totals[0].feature_id, API_FEATURE_ID);
        assert_eq!(totals[0].calls, 1);
        assert_eq!(totals[0].prompt_tokens, 3);
    }

    #[tokio::test]
    async fn a_call_the_provider_reported_no_usage_for_is_still_recorded() {
        let h = llm_harness(Ok(LlmResponse::untracked("ok"))).await;
        chat(&h.deps(None), CHAT).await.unwrap();
        let totals = h.usage.totals(1).await.unwrap();
        assert_eq!(totals[0].calls, 1);
        assert_eq!(totals[0].unreported_calls, 1);
    }

    #[tokio::test]
    async fn stream_true_is_one_content_chunk_a_stop_and_done() {
        let h = llm_harness(Ok(LlmResponse::untracked("Hello!"))).await;
        let body = br#"{"stream":true,"messages":[{"role":"user","content":"Hi"}]}"#;
        let reply = chat(&h.deps(None), body).await.unwrap();
        assert_eq!(reply.content_type, SSE_CONTENT_TYPE);
        let text = text_of(&reply);
        assert!(text.contains("\"content\":\"Hello!\""), "{text}");
        assert!(text.contains("\"finish_reason\":\"stop\""), "{text}");
        assert!(text.ends_with("data: [DONE]\n\n"), "{text}");
    }

    #[tokio::test]
    async fn chat_over_the_rate_limit_is_a_429_with_retry_after() {
        let mut h = llm_harness(Ok(LlmResponse::untracked("ok"))).await;
        h.limiter = Mutex::new(TokenBucket::per_minute(1, Instant::now()));
        assert!(chat(&h.deps(None), CHAT).await.is_ok());
        let err = expect_err(chat(&h.deps(None), CHAT).await);
        assert_eq!(err.status, 429);
        assert!(err.retry_after.unwrap_or(0) >= 1);
    }

    #[tokio::test]
    async fn a_refused_request_does_not_spend_a_rate_limit_token() {
        let mut h = llm_harness(Ok(LlmResponse::untracked("ok"))).await;
        h.limiter = Mutex::new(TokenBucket::per_minute(1, Instant::now()));
        let bad = br#"{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"x"}}]}]}"#;
        assert_eq!(expect_err(chat(&h.deps(None), bad).await).status, 400);
        assert!(
            chat(&h.deps(None), CHAT).await.is_ok(),
            "the token was still there"
        );
    }

    /// A provider that echoes the key it was sent back in its error. The
    /// caller gets the message with the key removed, whichever route ran.
    #[tokio::test]
    async fn a_key_the_provider_echoes_is_removed_from_the_502() {
        let mut h = harness().await;
        h.engines.register_llm(Arc::new(FakeLlm {
            result: Err("invalid key sk-live-123".into()),
        }));
        h.bindings
            .set(
                API_FEATURE_ID,
                "llm",
                Binding {
                    engine_id: "fake-llm".into(),
                    model: None,
                    provider_ref: Some("openai".into()),
                },
            )
            .await
            .unwrap();
        h.credentials.set("openai", "sk-live-123").await.unwrap();
        let err = expect_err(chat(&h.deps(None), CHAT).await);
        assert_eq!(err.status, 502);
        assert!(
            !err.body().to_string().contains("sk-live-123"),
            "{}",
            err.message
        );
        assert!(err.message.contains("[redacted]"), "{}", err.message);
    }

    #[tokio::test]
    async fn a_provider_failure_is_a_502_with_its_message_and_no_secret() {
        let h = llm_harness(Err("upstream rejected the request".into())).await;
        let err = expect_err(chat(&h.deps(None), CHAT).await);
        assert_eq!(err.status, 502);
        assert!(
            err.message.contains("upstream rejected the request"),
            "{}",
            err.message
        );
        let body = err.body().to_string();
        assert!(!body.contains("api_key") && !body.contains("sk-"), "{body}");
    }

    #[tokio::test]
    async fn image_content_is_a_400_saying_only_text_is_supported() {
        let h = llm_harness(Ok(LlmResponse::untracked("ok"))).await;
        let body = br#"{"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"x"}}]}]}"#;
        let err = expect_err(chat(&h.deps(None), body).await);
        assert_eq!(err.status, 400);
        assert!(err.message.contains("Only text content"), "{}", err.message);
    }

    #[tokio::test]
    async fn chat_with_nothing_bound_is_a_409() {
        let h = harness().await;
        let err = expect_err(chat(&h.deps(None), CHAT).await);
        assert_eq!(err.status, 409);
    }

    // ---- /v1/models --------------------------------------------------------

    #[tokio::test]
    async fn models_lists_every_engines_models_and_marks_the_bound_one() {
        let mut h = harness().await;
        h.engines.register_stt(Arc::new(FakeStt {
            id: "whisper",
            models: vec!["base".into(), "small".into()],
            segments: Vec::new(),
            text: String::new(),
        }));
        h.engines.register_stt(Arc::new(FakeStt {
            id: "parakeet",
            models: vec!["v3".into()],
            segments: Vec::new(),
            text: String::new(),
        }));
        h.bind("stt", "parakeet", Some("v3")).await;

        let reply = list(&h.deps(None)).await.unwrap();
        let v = json_of(&reply);
        let data = v["data"].as_array().unwrap();
        // Engine order is the registry's, so the assertions are on the set.
        let mut ids: Vec<&str> = data.iter().map(|m| m["id"].as_str().unwrap()).collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["parakeet/v3", "whisper/base", "whisper/small"]);
        let defaults: Vec<&str> = data
            .iter()
            .filter(|m| m["kea_default"] == true)
            .map(|m| m["id"].as_str().unwrap())
            .collect();
        assert_eq!(defaults, vec!["parakeet/v3"]);
        assert!(data.iter().all(|m| m["kea_capability"] == "stt"));
    }

    #[tokio::test]
    async fn models_marks_no_default_when_nothing_is_bound_or_resolvable() {
        let mut h = harness().await;
        h.engines.register_stt(Arc::new(FakeStt {
            id: "a",
            models: vec!["x".into()],
            segments: Vec::new(),
            text: String::new(),
        }));
        h.engines.register_stt(Arc::new(FakeStt {
            id: "b",
            models: vec!["y".into()],
            segments: Vec::new(),
            text: String::new(),
        }));
        let v = json_of(&list(&h.deps(None)).await.unwrap());
        assert!(v["data"]
            .as_array()
            .unwrap()
            .iter()
            .all(|m| m["kea_default"] == false));
    }

    #[tokio::test]
    async fn a_listed_id_is_named_by_a_request_without_being_saved() {
        let mut h = harness().await;
        h.engines.register_stt(Arc::new(FakeStt {
            id: "whisper",
            models: vec!["base".into()],
            segments: vec![segment(0, 1_000, "ok")],
            text: "ok".into(),
        }));
        h.engines.register_stt(Arc::new(FakeStt {
            id: "other",
            models: vec!["big".into()],
            segments: Vec::new(),
            text: String::new(),
        }));
        // Nothing bound, so only the listed id can pick the engine.
        let (ct, body) = transcription_body(&[("model", "whisper/base")]);
        let reply = transcriptions(&h.deps(None), Some(&ct), body)
            .await
            .unwrap();
        assert_eq!(json_of(&reply)["text"], "ok");
        assert!(
            h.bindings
                .get(API_FEATURE_ID, "stt")
                .await
                .unwrap()
                .is_none(),
            "a listed choice was saved"
        );
    }

    // ---- guards ------------------------------------------------------------

    /// The model routes must not write a file or start playback. A structural
    /// check over this module's non-test source, because the property is
    /// about what the code can reach, and reaching the playback path would
    /// need one of these names.
    #[test]
    fn no_model_route_writes_a_file_or_reaches_playback() {
        let source = include_str!("models.rs");
        let production = source.split("#[cfg(test)]").next().unwrap();
        for forbidden in [
            "std::fs::write",
            "File::create",
            "OpenOptions",
            "tokio::fs",
            "read_aloud",
            "trigger_tts_inner",
            "preview_playing",
            "AudioIo",
            "AVSpeech",
        ] {
            assert!(
                !production.contains(forbidden),
                "models.rs reaches `{forbidden}`"
            );
        }
    }

    #[test]
    fn person_labels_are_one_based_and_ignore_other_keys() {
        assert_eq!(person_label("spk0").as_deref(), Some("Person 1"));
        assert_eq!(person_label("spk1").as_deref(), Some("Person 2"));
        assert_eq!(person_label("nobody"), None);
    }

    #[test]
    fn wav_encoding_clips_instead_of_wrapping() {
        assert_eq!(to_i16(2.0), i16::MAX);
        assert_eq!(to_i16(-2.0), -i16::MAX);
        assert_eq!(to_i16(0.0), 0);
    }
}
