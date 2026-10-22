# Proposal

## Why

KEA ships local speech-to-text, text-to-speech and voice-separation models, and it routes LLM calls to whichever provider the user has set up. Other apps and scripts on the same machine can't use any of that. The existing local API (`/v1/rewrite`, `/v1/transcribe`, `/v1/speak`, …) exposes the app's *features*, not its models:

- `speak` reads the selection aloud and returns no audio.
- `rewrite` only accepts fixed modes.
- `transcribe` takes a file path and returns no transcript to the caller.

It is also reachable only over a Unix socket, which most SDKs and tools can't be pointed at. Exposing the models through the API shape the ecosystem already speaks (OpenAI's) would make KEA a local model server for anything on the machine, with no new client code.

## What Changes

- **OpenAI-compatible model routes on the local API.** Each route uses the model the user bound for the new `api` feature, falling back to the per-capability defaults:
  - `POST /v1/audio/transcriptions`: multipart audio in, transcript out (`json`, `text`, `verbose_json`, `srt`, `vtt`). A KEA extension field, `speakers=true`, adds per-segment speaker labels using the voice separation from `add-meeting-speaker-diarization`.
  - `POST /v1/audio/speech`: text, voice and speed in, `wav` or `pcm` audio out. Nothing is played aloud.
  - `POST /v1/chat/completions`: messages in, a reply from the bound LLM out. `stream: true` is answered as a single-chunk server-sent-event stream.
  - `GET /v1/models`: the installed and bound models callers can name.
- **Model names.** A request may omit `model`, use the OpenAI aliases clients hard-code (`whisper-1`, `tts-1`, `tts-1-hd`, `gpt-*`, which map to the bound model), or name a listed model. Any other name is a `404` `model_not_found` in OpenAI's error shape.
- **Opt-in localhost TCP port.** A separate setting, off by default, listens on `127.0.0.1:<port>`; the Unix socket stays the default transport. It reverses an earlier decision recorded in `src-tauri/src/api/auth.rs`, and only when the user turns it on. The existing defences apply on both transports:
  - the token is required, either as `Authorization: Bearer` (so OpenAI SDKs work unchanged) or as `x-kea-token`;
  - any request with an `Origin` header is refused;
  - the `Host` header must name loopback.
- **Cost and load controls.**
  - LLM requests share the existing per-minute token bucket and are recorded in the usage ledger.
  - Requests reuse the app's already-loaded engines, so a model is never loaded twice.
  - There is a request-size cap, and only one heavy request runs at a time.
- **CLI verbs** in the `kea` shim, on top of the existing ones:
  - `kea stt FILE|- [--speakers] [--format …]`
  - `kea tts TEXT|- [--voice V] [--out FILE]`
  - `kea llm PROMPT|- [--system …]`
  - `kea models`
- **Unchanged:** the existing feature routes (`/v1/rewrite`, `/v1/transcribe`, `/v1/speak`, `/v1/dictation/*`, `/v1/status`) and the `kea://` scheme.

## Capabilities

### New Capabilities
- `local-api/model-endpoints`: the OpenAI-compatible transcription, speech, chat and model-list routes, model-name resolution, the error shape, and the cost, load and size limits.
- `local-api/loopback-transport`: the opt-in TCP listener, and the authentication rules shared by both transports.
- `local-api/model-cli`: the `kea stt`, `kea tts`, `kea llm` and `kea models` commands.

### Modified Capabilities
<!-- None: openspec/specs/ has no existing local-API specs. -->

## Impact

- **`src-tauri/src/api/`**:
  - `server.rs`: routes, plus a request-body limit raised per route.
  - `auth.rs`: accepts `Authorization: Bearer`, and its threat-model notes are updated for the TCP option.
  - `exec.rs`: model execution.
  - `settings.rs`: the TCP toggle and port.
  - `token.rs`: unchanged.
  - New modules for the OpenAI request/response shapes and audio encoding.
- **`crates/core`**: an `api` feature id for bindings, so the Models page can bind it. Resolution already falls back to the capability defaults.
- **`crates/platform/src/audio/decode.rs`**: decode from bytes as well as from a path.
- **Dependencies**: a multipart parser for `audio/transcriptions` (e.g. `multer`). `hound` (already in the lockfile) for WAV output and `symphonia` (already used) for decoding.
- **`src-tauri/resources/kea-cli`**: new verbs, stdin support, and an `--api-url` / `KEA_API_URL` override for the TCP transport.
- **`ui`**: the General page gets the TCP toggle and port with a plain-language warning, the API docs blurb, and an `api` row on the Models/bindings page.
- **Security**: the TCP port makes the API reachable from browser pages. The token, Origin and Host defences stay in place, and the warning text says what the token allows. The default posture doesn't change.
- **Out of scope**:
  - realtime/streaming speech-to-text over WebSocket;
  - OpenAI `embeddings`, `images` and `responses` APIs;
  - LAN (non-loopback) listening;
  - per-client tokens or scopes.
