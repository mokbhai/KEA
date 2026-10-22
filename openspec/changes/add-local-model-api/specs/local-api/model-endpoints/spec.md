# Spec Delta

## Purpose

Lets other apps and scripts on the same machine use KEA's speech-to-text, text-to-speech
and LLM models through OpenAI-compatible routes on the local API, so existing SDKs and
tools work by changing only their base URL and key.

## ADDED Requirements

### Requirement: Model routes require the local API and its token
The model routes SHALL be served only while the local API is enabled, and every request
to them MUST pass the same authentication as the existing local API routes. They SHALL
be available on every transport the local API is listening on.

#### Scenario: Local API off
- **WHEN** the local API is disabled and a client calls `POST /v1/audio/speech`
- **THEN** nothing is listening, and the request fails to connect

#### Scenario: Missing token
- **WHEN** a request to `POST /v1/chat/completions` carries no valid token
- **THEN** the response is `401` and no model is invoked

### Requirement: Transcription returns the transcript to the caller
`POST /v1/audio/transcriptions` SHALL accept a `multipart/form-data` body with a `file`
part containing audio or video in any format KEA can decode, and optional `model`,
`language`, `prompt` and `response_format` fields. It SHALL return the transcript
produced by the speech-to-text model bound for API use, in the requested format:
`json` (default, `{"text": …}`), `text`, `verbose_json` (with timed segments), `srt` or
`vtt`. The audio MUST NOT be written to disk beyond the lifetime of the request.

#### Scenario: OpenAI SDK transcription
- **WHEN** an OpenAI SDK pointed at KEA uploads `meeting.m4a` with `model="whisper-1"`
- **THEN** the response is `200` with `{"text": "<transcript>"}` from the bound speech-to-text model

#### Scenario: Timed segments
- **WHEN** `response_format=verbose_json` is requested
- **THEN** the response includes `segments` with `start`, `end` (seconds) and `text` for each span the model reported, and an empty `segments` list when it reported none

#### Scenario: Subtitles
- **WHEN** `response_format=srt` is requested
- **THEN** the body is an SRT document with one cue per segment and content type `application/x-subrip`

#### Scenario: Undecodable upload
- **WHEN** the `file` part is not decodable audio
- **THEN** the response is `400` with an OpenAI-shaped error naming the decode failure

### Requirement: Transcription can label speakers
`POST /v1/audio/transcriptions` SHALL accept a KEA extension field `speakers=true`. When
given with `verbose_json`, each segment SHALL carry a `speaker` field ("Person 1",
"Person 2", …) from voice separation over that upload alone. When voice separation is
unavailable (models not installed), the request MUST fail with `409` and an error
saying which models are missing, rather than silently returning unlabelled segments.

#### Scenario: Two voices in an upload
- **WHEN** a recording with two speakers is uploaded with `speakers=true` and `response_format=verbose_json`
- **THEN** each segment carries `speaker` as "Person 1" or "Person 2"

#### Scenario: Diarization models missing
- **WHEN** `speakers=true` is sent and the diarization models are not installed
- **THEN** the response is `409` and says the speaker models must be downloaded

### Requirement: Speech synthesis returns audio instead of playing it
`POST /v1/audio/speech` SHALL accept JSON with `input` (required), and optional `model`,
`voice`, `speed` and `response_format`, synthesize `input` with the text-to-speech
model bound for API use, and return the audio in the body. Supported formats SHALL be
`wav` (default) and `pcm` (16-bit little-endian mono at the engine's rate, reported in
an `x-kea-sample-rate` header). Other formats (`mp3`, `opus`, `aac`, `flac`) MUST be
refused with `400` rather than returned in a different format. The request MUST NOT
play audio on the machine.

#### Scenario: WAV for a script
- **WHEN** a client posts `{"input": "Hello", "voice": "<an installed voice>"}`
- **THEN** the response is `200`, `audio/wav`, containing the synthesized speech, and nothing is heard on the speakers

#### Scenario: Unknown voice
- **WHEN** `voice` names a voice the bound engine does not have
- **THEN** the response is `400` naming the voice, and no fallback voice is substituted

#### Scenario: Unsupported format
- **WHEN** `response_format` is `mp3`
- **THEN** the response is `400` listing `wav` and `pcm` as the supported formats

#### Scenario: Speed
- **WHEN** `speed` is given between 0.25 and 4.0
- **THEN** it is passed to the engine as the rate multiplier, and values outside that range are refused with `400`

### Requirement: Chat completions use the bound LLM
`POST /v1/chat/completions` SHALL accept JSON with `messages` (required; `system`,
`user` and `assistant` roles with string content), and optional `model`, `stream`,
`temperature` and `max_tokens`. It SHALL send the conversation to the LLM bound for API
use and return an OpenAI-shaped completion with one choice, and `usage` when the
provider reported it. When `stream` is `true`, the response SHALL be a server-sent-event
stream that carries the whole reply in one `chat.completion.chunk` followed by
`data: [DONE]`. Fields the bound provider can't honour MUST be ignored, not refused, so
that common clients keep working. Tool calls and non-text content parts MUST be refused
with `400`.

#### Scenario: SDK chat call
- **WHEN** an OpenAI SDK sends `messages=[{"role":"user","content":"Hi"}]`
- **THEN** the response has `choices[0].message.content` from the bound LLM and `object: "chat.completion"`

#### Scenario: Streaming client
- **WHEN** `stream: true` is sent
- **THEN** the response is `text/event-stream` containing one chunk with the full reply and then `data: [DONE]`

#### Scenario: Image content
- **WHEN** a message's content is an array that contains an image part
- **THEN** the response is `400` saying only text content is supported

### Requirement: Clients can discover and name models
`GET /v1/models` SHALL list, in OpenAI's list shape, the models a caller may name in the
`model` field. Each id SHALL be one that the model routes accept, and each SHALL carry a
KEA field saying which capability it serves (`stt`, `tts` or `llm`) and whether it is
the current default for API use. On every model route, `model` SHALL resolve as follows:
- omitted, or `default`: the model bound for API use;
- a common OpenAI alias for that route (`whisper-1` for transcription; `tts-1`, `tts-1-hd` for speech; any `gpt-*` id for chat): the model bound for API use;
- a listed id for that capability: that model;
- anything else: `404` with error code `model_not_found`.

#### Scenario: Hard-coded alias
- **WHEN** a tool sends `model: "tts-1"` to `/v1/audio/speech`
- **THEN** the bound text-to-speech model is used

#### Scenario: Wrong capability
- **WHEN** a speech-to-text model id is sent to `/v1/chat/completions`
- **THEN** the response is `404` with code `model_not_found`

#### Scenario: Nothing bound
- **WHEN** no model is bound or resolvable for a capability
- **THEN** that route answers `409` saying which binding to set on the Models page, and `/v1/models` lists nothing as the default for that capability

### Requirement: Errors use OpenAI's error shape
Every error from the model routes SHALL be a JSON body `{"error": {"message", "type",
"code"}}` with an HTTP status that matches the failure: `400` invalid request, `401`
authentication, `404` unknown model, `409` not configured or busy, `413` too large,
`429` rate limited (with `Retry-After`), `502` provider failure.

#### Scenario: Provider failure
- **WHEN** the bound hosted LLM provider returns an error
- **THEN** the response is `502` with the provider's message in `error.message` and no API key or token in any field

### Requirement: Model routes have bounded cost and load
Chat completion requests SHALL draw from the same per-minute rate limit as the existing
LLM-backed API routes, and every LLM call SHALL be recorded in KEA's usage ledger
attributed to API use. Request bodies SHALL be limited to 25 MB for transcription and
1 MB for the other routes, with `413` beyond. A model route SHALL reuse an engine the
app has already loaded instead of loading a second copy. While one transcription or
speech request is running, another of the same kind SHALL get `409` with a
`Retry-After` hint rather than queueing without bound. A model request MUST NOT open
the microphone, play audio, or change the user's selection.

#### Scenario: Over the rate limit
- **WHEN** a script sends more chat completions in a minute than the configured limit
- **THEN** the excess requests get `429` with `Retry-After`

#### Scenario: Concurrent transcriptions
- **WHEN** a second transcription arrives while one is still running
- **THEN** it gets `409` with `Retry-After`, and the first completes normally

#### Scenario: Recording in progress
- **WHEN** a meeting or dictation is recording and a transcription request arrives
- **THEN** the request runs without stopping or interfering with the recording

#### Scenario: Usage is visible
- **WHEN** a chat completion succeeds against a provider that reports tokens
- **THEN** KEA's usage view shows that call attributed to API use
