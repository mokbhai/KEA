# Design

## Context

- **The local API** (`src-tauri/src/api/`) is a hyper HTTP/1.1 server.
  - It listens on a `0600` Unix socket under the app data directory.
  - A route table in `server.rs` parses requests into one `KeaAction` vocabulary (`actions.rs`), carried out by `exec.rs` through the same `*_inner` functions the hotkeys use.
  - Auth (`auth.rs`): custom `x-kea-token` header, any `Origin` refused, loopback-only `Host`, constant-time compare. It documents that TCP was rejected because of browser reachability.
  - `MAX_BODY` is 1 MB.
  - `ratelimit.rs` holds the token bucket for LLM verbs.
  - On non-Unix platforms `server::start` returns an error, so there is no API there at all.
- **Engines** sit behind `EngineRegistry` traits:
  - `SttEngine::transcribe(AudioPcm, SttOpts) -> Transcript` (with timed segments);
  - `TtsEngine::synthesize(&str, TtsOpts{voice, speed, …}) -> AudioPcm`;
  - `LlmEngine::complete(LlmRequest{prompt, model, provider_ref}) -> LlmResponse{text, usage}`.
- **Bindings** are `(feature_id, slot)`. `SlotResolver::resolve` falls back to the per-capability default, then auto-picks a single candidate.
- **File transcription already has the long-audio path** in `kea_core::transcript`:
  - `plan_chunks` and `transcribe_chunks` for long audio;
  - `SubtitleFormat` for SRT/VTT;
  - `assign_speakers` plus the whole-file sherpa diarizer behind `diarize_if_enabled`;
  - decoding through symphonia in `kea_platform::audio::decode`.
- **The `kea` CLI** is a POSIX `sh` script that wraps `curl --unix-socket`.

## Goals / Non-Goals

**Goals:**
- Model routes are thin adapters over code paths that already exist: decode, chunked STT, diarization, synthesis, completion. No second implementation of any of them.
- The OpenAI request and response shapes live in one module, so the KEA-native vocabulary isn't distorted to fit them.
- The default security posture is unchanged. The port adds exposure only when turned on.

**Non-Goals:**
- **True token streaming** for chat. `LlmEngine` returns a whole reply. Adding a streaming trait method across every provider is its own change, and the single-chunk SSE stream is enough to stop clients that default to `stream: true` from breaking.
- **MP3/Opus encoding**: no encoder is in the tree, and adding one costs licensing and size. WAV and PCM cover scripts and every player.
- **A Windows CLI.** Windows users call the TCP port with `curl` or an SDK. A PowerShell shim is a follow-up.
- **Per-client tokens, scopes or revocation per client.** There is one token, as today.

## Decisions

### 1. A new `api::openai` module owns the wire shapes; `exec` gains model verbs

`api/openai.rs` holds:
- the serde request types, with `deny_unknown_fields` off: the spec says fields we can't honour are ignored, and OpenAI clients send many;
- the response builders;
- the error shape (`{"error":{message,type,code}}`) and the mapping from the internal error to it;
- the SRT/VTT/verbose_json renderers, which delegate to `kea_core::transcript::SubtitleFormat`.

`server.rs` gains four route kinds. They don't go through `KeaAction`: the actions vocabulary is "things a URL or a hotkey can ask the app to do", and these are request/response calls with binary bodies. Folding them in would mean a `KeaAction::Speech { bytes }` variant that the `kea://` parser could never produce.

Alternative considered: a second hyper service on its own socket. That was rejected because it would duplicate auth, the listener lifecycle and the settings, which is exactly the "second path to work that already has one" that `api/mod.rs` rules out.

### 2. An `api` feature id for bindings

Model routes resolve through `SlotResolver::new(…).require_{stt,tts,llm}("api")`. With no `api` binding, the resolver already falls back to the capability default and then to the single candidate. So "use whatever KEA is set up with" works on day one, and the Models page can show an "API" row to pin something different. A `NeedsChoice` or `Unresolvable` outcome becomes `409` with the page to visit.

**Model names.** `model` maps through a small pure function in `openai.rs`:
- omitted, `default`, or an alias → the bound one;
- a listed id → that engine and model (a `Binding` built for this request only, never saved);
- anything else → `model_not_found`.

`GET /v1/models` enumerates `EngineRegistry` ids × `capabilities().models`, plus the bound default per slot. Ids are `<engine_id>/<model>`, or `<engine_id>` when an engine has one model, so a listed id is unambiguous and stable across restarts.

### 3. Transcription: decode bytes, reuse the chunk driver and the diarizer

1. `multer` parses the multipart body in memory, under a dedicated 25 MB limit for this route. `MAX_BODY` stays 1 MB for everything else.
2. A new `decode_bytes(&[u8], target_rate_hz)` sits beside `decode_file_at`. Symphonia already reads from any `MediaSource`, so this is a `Cursor` in place of a `File`. Nothing touches disk.
3. `plan_chunks` and `transcribe_chunks` run with a sink that collects segments and emits no Tauri events: an API job is not a row in the Transcripts page.
4. With `speakers=true`, the whole-file diarizer and `assign_speakers` run over the same PCM. They are the same functions `diarize_if_enabled` uses, without the setting gate, because the request asked explicitly. Speaker indices render as `Person N`, matching the meeting labels from `add-meeting-speaker-diarization`.

**Concurrency.** A new `api_stt_busy` `AtomicBool` uses the existing `try_acquire_busy`. It is separate from `file_transcribe_busy`, so a script can't make the Transcribe page report "busy", and vice versa. Neither touches the microphone flags, which satisfies "runs alongside a recording".

### 4. Speech: synthesize, encode WAV, never touch playback

The bound `TtsEngine::synthesize` returns `AudioPcm`, which is encoded with `hound` into an in-memory `Vec<u8>` (16-bit PCM WAV), or written raw for `pcm`. The route never calls the playback path the read-aloud feature uses.

**Voices.** An unknown voice is checked against the engine's voice list before synthesis. The `6fda44a` fix already made the local TTS engine refuse unknown identifiers instead of falling back; the route maps that refusal to `400`.

**Concurrency.** An `api_tts_busy` flag.

**Limits.** `input` is capped at 4,096 characters, as OpenAI's own limit is, so a runaway script can't hold the engine for minutes.

### 5. Chat: flatten messages into the existing prompt, rate-limit, record usage

`LlmRequest` is a single `prompt`. Messages are rendered into a role-tagged transcript (`System:`, `User:`, `Assistant:`, ending in `Assistant:`) by one pure function. That is the same flattening the providers that expose only a completion endpoint already receive.

Alternative considered: adding `messages` to `LlmRequest`. It is better long-term, but it changes the trait every engine implements, so it is deferred and noted as the natural follow-up together with streaming.

**Rate limit and usage.**
- Each request takes a token from the existing `TokenBucket`; `429` carries `Retry-After` from `retry_after`.
- Usage is recorded through `UsageRepo::record` with feature id `api`, so the usage view attributes it.
- `temperature` and `max_tokens` are accepted and ignored until `LlmRequest` carries them. The spec allows this, and it is documented in the `/v1/models` blurb on the General page.
- `stream: true` returns `text/event-stream` with one `chat.completion.chunk` (role and full content), a final chunk with `finish_reason: "stop"`, then `[DONE]`.

### 6. Transport: one service, two listeners

`server::start` gains an optional `SocketAddr`.
- **Unix:** the Unix listener runs as today.
- **TCP on:** a `TcpListener` bound to `127.0.0.1:<port>` runs a second accept loop over the same `service_fn`, so routes, auth and limits are shared by construction.
- **Non-Unix:** `start` no longer errors when a TCP address is given; it runs only the TCP loop.

A bind failure is returned to the settings command and shown inline. The Unix listener is unaffected.

**Settings.** `api.tcp.enabled` (absent means false) and `api.tcp.port` (absent means 8765), in the same settings table and pattern as `api.enabled`.

**Disabling the port.** The handle owns a `watch` channel. The TCP accept loop stops accepting at once; connections already accepted run to completion.

**Auth changes (`auth.rs`):**
- Accept `Authorization: Bearer <token>` as well as `x-kea-token`. Both go through the same constant-time compare. `Authorization` is not a CORS-safelisted header, so a browser still has to preflight, and the preflight still fails, because no `Access-Control-Allow-*` header is ever sent.
- The `Origin` refusal and the `Host` allow-list stay as they are. `Host` already tolerates a port.
- The module doc's "TCP was rejected" paragraph is rewritten to explain the opt-in, what it re-exposes (browser reachability, with the three checks as the remaining defence), and why it is off by default.

### 7. CLI

These are new `case` arms in `kea-cli`:
- `stt` posts `-F file=@PATH` (or `@-` for stdin).
- `tts` posts JSON built with the shim's existing escaping. It checks `[ -t 1 ]` for the terminal guard, and `--out` uses `curl -o`.
- `llm` reads stdin when it isn't a terminal and appends it to the prompt after a blank line.
- `models` formats the list with a small `sed`. `--json` passes it through.

**Transport.** `--api-url` or `KEA_API_URL` switches `curl` from `--unix-socket` to the URL. The token is sent as `Authorization: Bearer` on both transports.

**Exit codes.** These reuse the shim's existing `69` (unavailable) and `77` (no token). An API error exits `1` and prints the body's `error.message`, extracted with `sed`, so no `jq` dependency is added.

## Risks / Trade-offs

- **[The TCP port is reachable from browser pages]** → It is off by default and bound to loopback only. Three checks remain: `Origin` refused, `Host` allow-list, and a token header that forces a failing preflight. The settings warning says plainly what the token allows. The rebinding class is defended rather than impossible once the port is on, and the docs say so.
- **[Scripts spend hosted LLM credits]** → The existing token bucket, the usage ledger attributed to `api`, and the existing rate-limit setting all apply.
- **[A large upload holds memory]** → There is a 25 MB cap, one transcription at a time, and decoding happens in memory with no disk copy. 25 MB of compressed audio is about an hour; decoded 16 kHz mono f32 is about 230 MB at worst, which is the same footprint file transcription already accepts.
- **[Single-chunk "streaming" surprises a client that expects progressive tokens]** → It is the correct wire format, so clients work. They just see the text all at once. This is documented, and true streaming is the named follow-up.
- **[Ignoring `temperature` and `max_tokens` silently]** → This is listed in the in-app API docs. It is preferable to refusing, which would break most clients' default requests.
- **[Model id format drifts]** → Ids are derived from engine ids, which are already persisted in bindings and so are stable. A unit test pins the format.

## Migration Plan

- No database migration: the new settings rows only exist once written, and absent means off. No binding is required, because of the resolver fallback.
- Ships additive: existing routes, the socket path, the token file and the CLI verbs are unchanged.
- Rollback means turning off the TCP setting, or the local API entirely. Both stop every new route.
