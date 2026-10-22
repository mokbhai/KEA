# Tasks

## 1. Foundations

- [x] 1.1 Add `decode_bytes(&[u8], target_rate_hz)` beside `decode_file_at` in `crates/platform/src/audio/decode.rs` (symphonia over a `Cursor`); verify unit tests decode a WAV fixture from bytes identically to the path decoder and reject garbage bytes with a decode error
- [x] 1.2 Register an `api` feature id for bindings (stt/tts/llm slots) so the Models page can show it; verify a `SlotResolver` test that an unbound `api` falls back to the capability default
- [x] 1.3 Add `multer` (multipart) and `hound` (WAV) as `kea-app` dependencies; verify `cargo build -p kea-app` and `cargo tree -d` shows no new duplicate major versions

## 2. OpenAI shapes (`src-tauri/src/api/openai.rs`, pure)

- [ ] 2.1 Request types for transcriptions (form fields), speech, and chat (ignoring unknown fields), plus the error type with the status mapping (400/401/404/409/413/429/502); verify unit tests for each status and that error bodies never contain the token
- [ ] 2.2 Model-name resolution (omitted/`default`/aliases/listed id/`model_not_found`) and the `<engine_id>/<model>` id format; verify unit tests including the wrong-capability case
- [ ] 2.3 Response renderers: transcription `json`/`text`/`verbose_json`/`srt`/`vtt` (via `SubtitleFormat`, with optional `speaker`), the chat completion, and the single-chunk SSE stream; verify golden-string tests for each format
- [ ] 2.4 Message flattening (system/user/assistant → role-tagged prompt) that refuses tool calls and non-text parts; verify unit tests

## 3. Model execution

- [ ] 3.1 Transcription: decode bytes, `plan_chunks`/`transcribe_chunks` with an event-free sink, optional whole-file diarization + `assign_speakers` rendered as `Person N`, the `api_stt_busy` flag; verify tests with a fake STT engine (segments, empty segments, `409` when busy, `409` when the diarization models are missing with `speakers=true`)
- [ ] 3.2 Speech: resolve voice (unknown → `400`), speed range 0.25–4.0, 4,096-character input cap, synthesize, encode WAV/PCM with `x-kea-sample-rate`, the `api_tts_busy` flag, no playback; verify tests with a fake TTS engine that WAV headers and sample count are correct and that the playback path is never called
- [ ] 3.3 Chat: resolve the LLM, take a rate-limit token (`429` + `Retry-After`), complete, record usage under feature `api`, map provider errors to `502`; verify tests with a fake LLM for success, stream, rate limit and provider failure, and that a usage row is written
- [ ] 3.4 `GET /v1/models` from the registry plus bindings, marking per-capability defaults; verify a test with two STT engines and one bound

## 4. Server and transport

- [ ] 4.1 Add the four routes to the route table, with a per-route body limit (25 MB for transcriptions, 1 MB otherwise) returning `413`; verify the route-table test and a socket test that an oversized body gets `413`
- [ ] 4.2 Accept `Authorization: Bearer` alongside `x-kea-token` in `auth.rs` with the same constant-time compare, and rewrite the module doc's transport section for the opt-in port; verify auth unit tests for both headers, a wrong bearer, and `Origin` still refused
- [ ] 4.3 TCP listener: optional `127.0.0.1:<port>` accept loop over the same service, a `watch`-based stop that lets in-flight requests finish, bind errors returned to the caller, and TCP-only operation on non-Unix; verify tests that bind an ephemeral port, authenticate via bearer, refuse `Host: attacker.example`, and stop accepting after disable
- [ ] 4.4 Settings `api.tcp.enabled` (default off) and `api.tcp.port` (default 8765) in `api/settings.rs`, applied live; verify a settings test that the defaults read as off/8765 and a port conflict is reported without stopping the socket

## 5. CLI

- [ ] 5.1 `kea stt FILE|-` with `--format/--language/--model/--speakers`; verify by running against a fake-backed debug build (or a `curl` mock) that text, srt and stdin inputs work
- [ ] 5.2 `kea tts TEXT|-` with `--voice/--speed/--model/--out`, the terminal guard, and stdout when piped; verify the guard exits non-zero in a TTY and that `--out` writes a valid WAV
- [ ] 5.3 `kea llm PROMPT|-` with `--system/--model`, appending piped stdin; and `kea models [--json]`; verify `echo ctx | kea llm "q"` sends both parts and `kea models` marks defaults
- [ ] 5.4 `--api-url`/`KEA_API_URL` transport switch, bearer auth on both transports, error-message extraction and the existing exit codes; update `usage()`; verify `sh -n` and a manual run with the API off and on

## 6. UI

- [ ] 6.1 General page: "Allow localhost port" toggle and port field, with the token-capability warning, inline bind errors, and the "only transport on this platform" note on Windows; verify a `GeneralPage` test for warning visibility and the conflict message
- [ ] 6.2 General page API blurb: base URL, the OpenAI SDK snippet, the supported formats, and that `temperature`/`max_tokens` are ignored; verify a snapshot/text test
- [ ] 6.3 Models page: an "API" binding row for stt/tts/llm; verify a test that it shows "uses default" when unbound

## 7. Verification

- [ ] 7.1 Run `cargo test --workspace`, `cargo clippy --workspace --all-targets`, `cargo fmt --check`, and the UI test and typecheck scripts; all pass
- [ ] 7.2 End-to-end with the official OpenAI Python SDK (`base_url=http://127.0.0.1:8765/v1`, KEA token as the API key): transcribe a file (with and without `speakers`), synthesize speech to a WAV, run a chat completion with and without `stream=True`, and list models; confirm a browser `fetch` to the port from a web page is refused
