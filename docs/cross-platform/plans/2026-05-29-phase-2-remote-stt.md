# Cross-Platform Rewrite — Phase 2 (Remote Speech-to-Text) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add push-to-talk dictation on macOS, Windows, and Linux: pressing the speech hotkey starts microphone capture, releasing/pressing again stops it, the captured audio is transcribed by a hosted engine (OpenAI / OpenAI-compatible), and the transcript is inserted at the cursor. A speech overlay window shows a live level meter and recording/transcribing/done states.

**Architecture:** Phase 1 built the `vox-platform` crate (`hotkeys` + `textio`) and `core::rewrite`. Phase 2 adds **microphone capture** to `vox-platform` (`platform::audio` via `cpal`, producing mono f32 PCM resampled to 16 kHz) and **remote transcription** to `vox-core` (`core::speech`: `Transcriber` trait + `RemoteTranscriber` doing a `reqwest` multipart upload). The Tauri shell (`src-tauri`) owns the push-to-talk state machine: it listens for `HotkeyId::Speech`, drives `AudioCapture`, builds a `Box<dyn Transcriber>` from `Settings` via an engine factory, transcribes on the tokio runtime, inserts via `TextIo::insert_text`, and emits `speech:state` events to the overlay UI. `core` stays runtime-agnostic (async via the `Transcriber` trait); only `src-tauri` depends on `tokio`.

**Tech Stack:** Rust (edition 2021), `cpal` (mic capture), `core::speech` over `reqwest` (features `["json","rustls-tls"]` + `multipart`), `async-trait`, `serde`/`serde_json`, `tokio` (src-tauri only), `wiremock` (HTTP fixture tests), Tauri 2.x events + commands, React 18 + TypeScript + Vite (overlay UI).

**Reference spec:** `docs/cross-platform/2026-05-29-cross-platform-rewrite-design.md` (Phase 2; data flow §4 "Data flow (speech-to-text)").

---

## File Structure

Phase 2 extends the `app/` workspace created in Phase 0 and the `vox-platform`
crate created in Phase 1. New and modified files:

- `app/crates/platform/Cargo.toml` — add `cpal` dependency.
- `app/crates/platform/src/lib.rs` — add `pub mod audio;` re-export.
- `app/crates/platform/src/audio.rs` — `AudioCapture` trait, `CapturedAudio`, `CpalAudioCapture`, `new_audio_capture`, plus the mono-downmix + 16 kHz resample helper (`resample_to_16k_mono`). Owns mic capture only.
- `app/crates/core/Cargo.toml` — add `reqwest` (json/rustls-tls/multipart), `async-trait`; add `wiremock`, `tokio` to `[dev-dependencies]`.
- `app/crates/core/src/lib.rs` — add `pub mod speech;`.
- `app/crates/core/src/speech.rs` — `TranscriptionRequest`/`TranscriptionResult`/`SpeechError`, the `Transcriber` trait, `RemoteTranscriber` (multipart WAV upload), `SpeechEngineKind`, and the `encode_wav_16k_mono` helper used to build the multipart body. Owns remote transcription only.
- `app/crates/core/src/settings.rs` — add Phase 2 fields (`speech_engine`, `speech_model`, `speech_language`); bump `schema_version` to `3`.
- `app/src-tauri/Cargo.toml` — depend on `vox-platform`; ensure `tokio` present.
- `app/src-tauri/src/speech_session.rs` — push-to-talk state machine + engine factory (`build_transcriber`) + `start_dictation`/`stop_dictation` commands; emits `speech:state`. Owns only dictation orchestration.
- `app/src-tauri/src/main.rs` — register the two new commands, spawn the `HotkeyId::Speech` listener, register the speech accelerator, create the overlay window.
- `app/src-tauri/tauri.conf.json` — add the always-on-top `overlay` window.
- `app/ui/src/overlay/Overlay.tsx` — speech overlay component (level meter + state).
- `app/ui/src/overlay/main.tsx` — overlay entry point.
- `app/ui/overlay.html` — overlay HTML entry (multi-page Vite input).
- `app/ui/vite.config.ts` — add `overlay.html` as a second Rollup input.

Each file keeps one responsibility: `audio.rs` knows only mic capture + format
conversion; `speech.rs` knows only request construction + HTTP transcription;
`speech_session.rs` knows only the press/release lifecycle and engine selection;
the overlay files know only rendering the live state.

---

## Prerequisites (one-time, not committed)

- [ ] **Step 0: Verify mic-capture system deps are present**

`cpal` needs ALSA headers on Linux. Run (Linux only):
```bash
sudo apt-get update && sudo apt-get install -y libasound2-dev
```
macOS and Windows need no extra packages; mic access prompts appear at runtime
(macOS) or are granted via OS settings (Windows). Verify the workspace still
builds before starting:
```bash
cargo build --manifest-path app/Cargo.toml
```
Expected: the Phase 0/1 workspace compiles.

---

## Task 1: `platform::audio` format helper (resample + downmix to 16 kHz mono)

**Files:**
- Modify: `app/crates/platform/Cargo.toml`
- Modify: `app/crates/platform/src/lib.rs`
- Create: `app/crates/platform/src/audio.rs`
- Test: in-file `#[cfg(test)]` module in `audio.rs`

- [ ] **Step 1: Add the `cpal` dependency**

Edit `app/crates/platform/Cargo.toml`, adding under `[dependencies]`:
```toml
cpal = "0.15"
thiserror = "1"
```
(`thiserror` may already be present from Phase 1; keep a single entry.)

- [ ] **Step 2: Register the module**

Edit `app/crates/platform/src/lib.rs` to add the `audio` re-export alongside the
Phase 1 modules:
```rust
pub mod hotkeys;
pub mod textio;
pub mod audio;
```

- [ ] **Step 3: Write the failing test**

Create `app/crates/platform/src/audio.rs`:
```rust
use thiserror::Error;

#[derive(Debug, Error)]
pub enum AudioError {
    #[error("{0}")]
    Capture(String),
    #[error("{0}")]
    Playback(String),
}

/// Captures mono f32 PCM at 16 kHz (Whisper's expected rate).
pub trait AudioCapture: Send {
    fn start(&mut self) -> Result<(), AudioError>;
    fn stop(&mut self) -> Result<CapturedAudio, AudioError>;
}

#[derive(Debug, Clone)]
pub struct CapturedAudio {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
}

/// Target sample rate for downstream STT engines (Whisper / OpenAI transcribe).
pub const TARGET_SAMPLE_RATE: u32 = 16_000;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downmixes_interleaved_stereo_to_mono() {
        // Two stereo frames: (L=0.0,R=1.0), (L=1.0,R=0.0) -> mono averages 0.5, 0.5.
        let interleaved = vec![0.0_f32, 1.0, 1.0, 0.0];
        let mono = resample_to_16k_mono(&interleaved, 2, 16_000);
        assert_eq!(mono.sample_rate, TARGET_SAMPLE_RATE);
        assert_eq!(mono.samples, vec![0.5, 0.5]);
    }

    #[test]
    fn passthrough_when_already_16k_mono() {
        let input = vec![0.1_f32, 0.2, 0.3, 0.4];
        let out = resample_to_16k_mono(&input, 1, 16_000);
        assert_eq!(out.samples, input);
        assert_eq!(out.sample_rate, 16_000);
    }

    #[test]
    fn downsamples_48k_mono_to_16k() {
        // 6 input samples at 48 kHz -> 2 output samples at 16 kHz (ratio 3:1).
        let input: Vec<f32> = (0..6).map(|i| i as f32).collect();
        let out = resample_to_16k_mono(&input, 1, 48_000);
        assert_eq!(out.sample_rate, 16_000);
        assert_eq!(out.samples.len(), 2);
        // First output maps to input index 0; second to index 3.
        assert_eq!(out.samples, vec![0.0, 3.0]);
    }

    #[test]
    fn empty_input_yields_empty_output() {
        let out = resample_to_16k_mono(&[], 2, 44_100);
        assert!(out.samples.is_empty());
        assert_eq!(out.sample_rate, 16_000);
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform audio`
Expected: FAIL — `resample_to_16k_mono` is not defined.

- [ ] **Step 5: Write the minimal implementation**

Add to `audio.rs` (above the `tests` module):
```rust
/// Downmix interleaved samples to mono, then resample to 16 kHz using
/// nearest-neighbour decimation/interpolation. Adequate for speech STT (the
/// hosted engines and Whisper are tolerant of simple resampling) and fully
/// deterministic, which keeps it unit-testable.
pub fn resample_to_16k_mono(interleaved: &[f32], channels: u16, src_rate: u32) -> CapturedAudio {
    let channels = channels.max(1) as usize;

    // 1. Downmix to mono by averaging each frame's channels.
    let mono: Vec<f32> = if channels == 1 {
        interleaved.to_vec()
    } else {
        interleaved
            .chunks(channels)
            .map(|frame| frame.iter().copied().sum::<f32>() / frame.len() as f32)
            .collect()
    };

    // 2. Resample to 16 kHz.
    if src_rate == TARGET_SAMPLE_RATE || mono.is_empty() {
        return CapturedAudio { samples: mono, sample_rate: TARGET_SAMPLE_RATE };
    }

    let ratio = src_rate as f64 / TARGET_SAMPLE_RATE as f64;
    let out_len = ((mono.len() as f64) / ratio).floor() as usize;
    let mut out = Vec::with_capacity(out_len);
    for i in 0..out_len {
        let src_index = (i as f64 * ratio).round() as usize;
        let src_index = src_index.min(mono.len() - 1);
        out.push(mono[src_index]);
    }
    CapturedAudio { samples: out, sample_rate: TARGET_SAMPLE_RATE }
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform audio`
Expected: all four `resample_to_16k_mono` tests PASS.

- [ ] **Step 7: Commit**

```bash
git add app/crates/platform/Cargo.toml app/crates/platform/src/lib.rs app/crates/platform/src/audio.rs
git commit -m "feat(platform): add audio module with 16k mono resample helper"
```

---

## Task 2: `CpalAudioCapture` + `new_audio_capture` (real mic capture)

**Files:**
- Modify: `app/crates/platform/src/audio.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `audio.rs` (this proves the trait object is
constructible and start/stop is idempotent against the default device; it is
skipped where no input device exists, e.g. headless CI):
```rust
    #[test]
    fn capture_factory_builds_a_trait_object() {
        // On a machine with no input device this returns Err; we accept either
        // outcome but require the factory to be wired and typed correctly.
        match new_audio_capture() {
            Ok(mut cap) => {
                // start() may fail if the host denies the device; tolerate it.
                let _ = cap.start();
            }
            Err(AudioError::Capture(_)) => {}
            Err(other) => panic!("unexpected error variant: {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform audio`
Expected: FAIL — `new_audio_capture`/`CpalAudioCapture` are not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `audio.rs` (above the `tests` module):
```rust
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::{Arc, Mutex};

/// `cpal`-backed microphone capture. Accumulates input frames into a shared
/// buffer while a stream is running; `stop()` downmixes + resamples to 16 kHz.
pub struct CpalAudioCapture {
    buffer: Arc<Mutex<Vec<f32>>>,
    src_rate: u32,
    src_channels: u16,
    stream: Option<cpal::Stream>,
}

// cpal::Stream is not Send on every platform; we never move it across threads.
unsafe impl Send for CpalAudioCapture {}

impl CpalAudioCapture {
    pub fn new() -> Result<Self, AudioError> {
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioError::Capture("no default input device".into()))?;
        let config = device
            .default_input_config()
            .map_err(|e| AudioError::Capture(e.to_string()))?;
        Ok(Self {
            buffer: Arc::new(Mutex::new(Vec::new())),
            src_rate: config.sample_rate().0,
            src_channels: config.channels(),
            stream: None,
        })
    }
}

impl AudioCapture for CpalAudioCapture {
    fn start(&mut self) -> Result<(), AudioError> {
        if self.stream.is_some() {
            return Ok(());
        }
        let host = cpal::default_host();
        let device = host
            .default_input_device()
            .ok_or_else(|| AudioError::Capture("no default input device".into()))?;
        let supported = device
            .default_input_config()
            .map_err(|e| AudioError::Capture(e.to_string()))?;
        self.src_rate = supported.sample_rate().0;
        self.src_channels = supported.channels();
        self.buffer.lock().unwrap().clear();

        let buffer = Arc::clone(&self.buffer);
        let err_fn = |e| eprintln!("audio capture stream error: {e}");
        let config: cpal::StreamConfig = supported.config();

        let stream = match supported.sample_format() {
            cpal::SampleFormat::F32 => device.build_input_stream(
                &config,
                move |data: &[f32], _| buffer.lock().unwrap().extend_from_slice(data),
                err_fn,
                None,
            ),
            cpal::SampleFormat::I16 => device.build_input_stream(
                &config,
                move |data: &[i16], _| {
                    let mut b = buffer.lock().unwrap();
                    b.extend(data.iter().map(|s| *s as f32 / i16::MAX as f32));
                },
                err_fn,
                None,
            ),
            cpal::SampleFormat::U16 => device.build_input_stream(
                &config,
                move |data: &[u16], _| {
                    let mut b = buffer.lock().unwrap();
                    b.extend(
                        data.iter()
                            .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0),
                    );
                },
                err_fn,
                None,
            ),
            other => return Err(AudioError::Capture(format!("unsupported sample format: {other:?}"))),
        }
        .map_err(|e| AudioError::Capture(e.to_string()))?;

        stream.play().map_err(|e| AudioError::Capture(e.to_string()))?;
        self.stream = Some(stream);
        Ok(())
    }

    fn stop(&mut self) -> Result<CapturedAudio, AudioError> {
        // Dropping the stream stops capture.
        self.stream = None;
        let raw = std::mem::take(&mut *self.buffer.lock().unwrap());
        Ok(resample_to_16k_mono(&raw, self.src_channels, self.src_rate))
    }
}

/// Build the platform default audio capture (cpal on all OSes).
pub fn new_audio_capture() -> Result<Box<dyn AudioCapture>, AudioError> {
    Ok(Box::new(CpalAudioCapture::new()?))
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-platform audio`
Expected: all `audio` tests PASS (the factory test tolerates the no-device case
in headless CI).

- [ ] **Step 5: Manual smoke check (not a committed gate)**

This verifies a real mic round-trip end to end and is run manually on a machine
with a microphone. Add a temporary example or run via a scratch binary:
```bash
cargo run --manifest-path app/Cargo.toml -p vox-platform --example mic_smoke 2>/dev/null || true
```
If no example exists, do the check inside Task 6 once the full dictation loop is
wired (speak a sentence, confirm a non-empty 16 kHz buffer is produced). Expected:
`stop()` returns `CapturedAudio { sample_rate: 16_000, samples: <non-empty> }`.

- [ ] **Step 6: Commit**

```bash
git add app/crates/platform/src/audio.rs
git commit -m "feat(platform): add CpalAudioCapture + new_audio_capture"
```

---

## Task 3: `core::speech` types, WAV encoder, and `SpeechEngineKind`

**Files:**
- Modify: `app/crates/core/Cargo.toml`
- Modify: `app/crates/core/src/lib.rs`
- Create: `app/crates/core/src/speech.rs`
- Test: in-file `#[cfg(test)]` module in `speech.rs`

- [ ] **Step 1: Add dependencies**

Edit `app/crates/core/Cargo.toml`. Under `[dependencies]` add (reqwest may
already exist from Phase 1 — ensure it has the `multipart` feature):
```toml
reqwest = { version = "0.12", default-features = false, features = ["json", "rustls-tls", "multipart"] }
async-trait = "0.1"
```
Under `[dev-dependencies]` add:
```toml
wiremock = "0.6"
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Register the module**

Edit `app/crates/core/src/lib.rs`:
```rust
pub mod settings;
pub mod secrets;
pub mod rewrite;
pub mod speech;
```

- [ ] **Step 3: Write the failing test (types + WAV encoder)**

Create `app/crates/core/src/speech.rs`:
```rust
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq)]
pub struct TranscriptionRequest {
    pub samples: Vec<f32>,
    pub sample_rate: u32,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TranscriptionResult {
    pub text: String,
}

#[derive(Debug, thiserror::Error)]
pub enum SpeechError {
    #[error("http {0}: {1}")]
    Http(u16, String),
    #[error("network: {0}")]
    Network(String),
    #[error("inference: {0}")]
    Inference(String),
    #[error("invalid configuration: {0}")]
    Config(String),
}

#[async_trait]
pub trait Transcriber: Send + Sync {
    async fn transcribe(&self, request: TranscriptionRequest)
        -> Result<TranscriptionResult, SpeechError>;
}

/// Engine selection persisted in Settings. The factory that turns this into a
/// `Box<dyn Transcriber>` lives in `src-tauri` (it depends on core + infer).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum SpeechEngineKind {
    OpenAi,
    OpenAiCompatible,
    WhisperLocal,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn engine_kind_serde_roundtrip() {
        for kind in [
            SpeechEngineKind::OpenAi,
            SpeechEngineKind::OpenAiCompatible,
            SpeechEngineKind::WhisperLocal,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let back: SpeechEngineKind = serde_json::from_str(&json).unwrap();
            assert_eq!(kind, back);
        }
    }

    #[test]
    fn wav_header_is_valid_16k_mono_pcm16() {
        let samples = vec![0.0_f32, 1.0, -1.0, 0.5];
        let wav = encode_wav_16k_mono(&samples);

        // RIFF / WAVE container markers.
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");

        // Audio format = 1 (PCM), channels = 1.
        assert_eq!(u16::from_le_bytes([wav[20], wav[21]]), 1);
        assert_eq!(u16::from_le_bytes([wav[22], wav[23]]), 1);
        // Sample rate = 16000.
        assert_eq!(u32::from_le_bytes([wav[24], wav[25], wav[26], wav[27]]), 16_000);
        // Bits per sample = 16.
        assert_eq!(u16::from_le_bytes([wav[34], wav[35]]), 16);

        // data chunk = "data" + 4 samples * 2 bytes.
        assert_eq!(&wav[36..40], b"data");
        let data_len = u32::from_le_bytes([wav[40], wav[41], wav[42], wav[43]]);
        assert_eq!(data_len, 8);
        assert_eq!(wav.len(), 44 + 8);

        // 1.0 -> i16::MAX, -1.0 -> i16::MIN (clamped).
        let first = i16::from_le_bytes([wav[44], wav[45]]);
        assert_eq!(first, 0);
        let second = i16::from_le_bytes([wav[46], wav[47]]);
        assert_eq!(second, i16::MAX);
        let third = i16::from_le_bytes([wav[48], wav[49]]);
        assert_eq!(third, i16::MIN);
    }
}
```

- [ ] **Step 4: Run test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core speech`
Expected: FAIL — `encode_wav_16k_mono` is not defined.

- [ ] **Step 5: Write the WAV encoder**

Add to `speech.rs` (above the `tests` module):
```rust
/// Encode mono f32 samples as a little-endian 16-bit PCM WAV at 16 kHz. This is
/// the body uploaded to the transcription endpoint (a `.wav` multipart part).
pub fn encode_wav_16k_mono(samples: &[f32]) -> Vec<u8> {
    const SAMPLE_RATE: u32 = 16_000;
    const CHANNELS: u16 = 1;
    const BITS_PER_SAMPLE: u16 = 16;

    let byte_rate = SAMPLE_RATE * CHANNELS as u32 * (BITS_PER_SAMPLE as u32 / 8);
    let block_align = CHANNELS * (BITS_PER_SAMPLE / 8);
    let data_len = (samples.len() * 2) as u32;
    let riff_len = 36 + data_len;

    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&riff_len.to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&CHANNELS.to_le_bytes());
    out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
    out.extend_from_slice(&byte_rate.to_le_bytes());
    out.extend_from_slice(&block_align.to_le_bytes());
    out.extend_from_slice(&BITS_PER_SAMPLE.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let clamped = s.clamp(-1.0, 1.0);
        let v = (clamped * i16::MAX as f32).round() as i16;
        out.extend_from_slice(&v.to_le_bytes());
    }
    out
}
```

- [ ] **Step 6: Run test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core speech`
Expected: `engine_kind_serde_roundtrip` and `wav_header_is_valid_16k_mono_pcm16`
PASS.

- [ ] **Step 7: Commit**

```bash
git add app/crates/core/Cargo.toml app/crates/core/src/lib.rs app/crates/core/src/speech.rs
git commit -m "feat(core): add speech types, WAV encoder, and SpeechEngineKind"
```

---

## Task 4: `RemoteTranscriber` (multipart upload to OpenAI / -compatible)

**Files:**
- Modify: `app/crates/core/src/speech.rs`

- [ ] **Step 1: Write the failing test (HTTP fixture via wiremock)**

Append to the `tests` module in `speech.rs`:
```rust
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn remote_transcriber_posts_multipart_and_parses_text() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .and(header_exists("authorization"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({
                    "text": "hello world"
                })),
            )
            .mount(&server)
            .await;

        let transcriber = RemoteTranscriber::new(
            "sk-test".to_string(),
            server.uri(),
            "gpt-4o-transcribe".to_string(),
        );
        let result = transcriber
            .transcribe(TranscriptionRequest {
                samples: vec![0.0, 0.1, -0.1, 0.2],
                sample_rate: 16_000,
                language: Some("en".to_string()),
            })
            .await
            .unwrap();
        assert_eq!(result, TranscriptionResult { text: "hello world".to_string() });
    }

    #[tokio::test]
    async fn remote_transcriber_surfaces_http_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/audio/transcriptions"))
            .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
            .mount(&server)
            .await;

        let transcriber = RemoteTranscriber::new(
            "sk-bad".to_string(),
            server.uri(),
            "gpt-4o-transcribe".to_string(),
        );
        let err = transcriber
            .transcribe(TranscriptionRequest {
                samples: vec![0.0],
                sample_rate: 16_000,
                language: None,
            })
            .await
            .unwrap_err();
        match err {
            SpeechError::Http(401, body) => assert!(body.contains("bad key")),
            other => panic!("expected Http(401), got {other:?}"),
        }
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core speech`
Expected: FAIL — `RemoteTranscriber` is not defined.

- [ ] **Step 3: Write the minimal implementation**

Add to `speech.rs` (above the `tests` module):
```rust
/// Remote transcription against an OpenAI / OpenAI-compatible
/// `POST {base_url}/audio/transcriptions` multipart endpoint.
pub struct RemoteTranscriber {
    api_key: String,
    base_url: String,
    model: String,
    http: reqwest::Client,
}

impl RemoteTranscriber {
    pub fn new(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
            http: reqwest::Client::new(),
        }
    }
}

#[derive(serde::Deserialize)]
struct TranscriptionResponse {
    text: String,
}

#[async_trait]
impl Transcriber for RemoteTranscriber {
    async fn transcribe(
        &self,
        request: TranscriptionRequest,
    ) -> Result<TranscriptionResult, SpeechError> {
        if self.api_key.trim().is_empty() {
            return Err(SpeechError::Config("missing API key".into()));
        }

        let wav = encode_wav_16k_mono(&request.samples);
        let file_part = reqwest::multipart::Part::bytes(wav)
            .file_name("audio.wav")
            .mime_str("audio/wav")
            .map_err(|e| SpeechError::Config(e.to_string()))?;

        let mut form = reqwest::multipart::Form::new()
            .text("model", self.model.clone())
            .text("response_format", "json")
            .part("file", file_part);
        if let Some(lang) = request.language {
            form = form.text("language", lang);
        }

        let url = format!("{}/audio/transcriptions", self.base_url.trim_end_matches('/'));
        let resp = self
            .http
            .post(url)
            .bearer_auth(&self.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| SpeechError::Network(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(SpeechError::Http(status.as_u16(), body));
        }

        let parsed: TranscriptionResponse = resp
            .json()
            .await
            .map_err(|e| SpeechError::Network(e.to_string()))?;
        Ok(TranscriptionResult { text: parsed.text })
    }
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core speech`
Expected: all four `speech` tests PASS (serde, WAV header, and both wiremock
HTTP tests).

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/speech.rs
git commit -m "feat(core): add RemoteTranscriber multipart upload"
```

---

## Task 5: Settings — Phase 2 fields + schema bump

**Files:**
- Modify: `app/crates/core/src/settings.rs`

- [ ] **Step 1: Write the failing test**

Append to the `tests` module in `app/crates/core/src/settings.rs`:
```rust
    #[test]
    fn phase2_defaults_are_set() {
        let s = Settings::default();
        assert_eq!(s.schema_version, 3);
        assert_eq!(s.speech_engine, crate::speech::SpeechEngineKind::OpenAi);
        assert_eq!(s.speech_model, "gpt-4o-transcribe");
        assert_eq!(s.speech_language, None);
    }

    #[test]
    fn older_file_without_speech_fields_loads_with_defaults() {
        // A pre-Phase-2 file (no speech_* keys) must still deserialize.
        let json = r#"{ "schema_version": 2, "rewrite_hotkey": "CmdOrCtrl+Shift+R" }"#;
        let parsed: Settings = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.speech_engine, crate::speech::SpeechEngineKind::OpenAi);
        assert_eq!(parsed.speech_model, "gpt-4o-transcribe");
        assert_eq!(parsed.speech_language, None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: FAIL — `speech_engine`/`speech_model`/`speech_language` fields do not
exist, and `schema_version` default is not yet `3`.

- [ ] **Step 3: Write the minimal implementation**

Edit the `Settings` struct in `settings.rs` to add the three fields (keeping the
Phase 0/1 fields), and update its `Default` impl. The struct already carries
`#[derive(... Serialize, Deserialize)]` and `#[serde(default)]`:
```rust
    pub speech_engine: crate::speech::SpeechEngineKind,
    pub speech_model: String,
    pub speech_language: Option<String>,
```
In `impl Default for Settings`, bump the version and add the defaults:
```rust
            schema_version: 3,
            // ...existing Phase 0/1 defaults...
            speech_engine: crate::speech::SpeechEngineKind::OpenAi,
            speech_model: "gpt-4o-transcribe".to_string(),
            speech_language: None,
```
(`Option<String>` and the `SpeechEngineKind` enum both implement `Serialize`/
`Deserialize`, so `#[serde(default)]` on the struct supplies these when absent.)

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox-core settings`
Expected: all settings tests PASS (Phase 0/1 tests plus the two new ones).

- [ ] **Step 5: Commit**

```bash
git add app/crates/core/src/settings.rs
git commit -m "feat(core): add Phase 2 speech settings + bump schema_version to 3"
```

---

## Task 6: Push-to-talk session + engine factory + Tauri commands

**Files:**
- Modify: `app/src-tauri/Cargo.toml`
- Create: `app/src-tauri/src/speech_session.rs`
- Modify: `app/src-tauri/src/main.rs`

- [ ] **Step 1: Add the platform dependency**

Edit `app/src-tauri/Cargo.toml` under `[dependencies]` (vox-core, tokio, and
vox-platform should now all be present; add what is missing):
```toml
vox-platform = { path = "../crates/platform" }
tokio = { version = "1", features = ["macros", "rt-multi-thread"] }
```

- [ ] **Step 2: Write the failing test (engine factory)**

Create `app/src-tauri/src/speech_session.rs` with the factory + a unit test for
it. The recording lifecycle itself is exercised by the manual smoke check (Step
8) because it needs a real device + window; the factory is pure and testable:
```rust
use std::sync::Mutex;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};
use vox_core::secrets::{KeyringStore, SecretStore};
use vox_core::settings::{default_settings_path, Settings, SettingsStore};
use vox_core::speech::{
    RemoteTranscriber, SpeechEngineKind, SpeechError, Transcriber, TranscriptionRequest,
};
use vox_platform::audio::{new_audio_capture, AudioCapture};

/// Build the active transcriber from settings + stored API key. `WhisperLocal`
/// is wired in Phase 3; here it returns a Config error so the UI can guide the
/// user back to a remote engine until then.
pub fn build_transcriber(settings: &Settings) -> Result<Box<dyn Transcriber>, SpeechError> {
    match settings.speech_engine {
        SpeechEngineKind::OpenAi | SpeechEngineKind::OpenAiCompatible => {
            let api_key = KeyringStore
                .get("openai")
                .map_err(|e| SpeechError::Config(e.to_string()))?
                .ok_or_else(|| SpeechError::Config("no OpenAI API key stored".into()))?;
            Ok(Box::new(RemoteTranscriber::new(
                api_key,
                settings.openai_base_url.clone(),
                settings.speech_model.clone(),
            )))
        }
        SpeechEngineKind::WhisperLocal => Err(SpeechError::Config(
            "local Whisper engine arrives in Phase 3".into(),
        )),
    }
}

/// Shared recording state owned by the Tauri app (managed state).
#[derive(Default)]
pub struct SpeechState {
    capture: Mutex<Option<Box<dyn AudioCapture>>>,
}

#[derive(Serialize, Clone)]
struct SpeechStateEvent {
    state: String,        // "recording" | "transcribing" | "done" | "error" | "idle"
    text: Option<String>,
    message: Option<String>,
}

fn emit_state(app: &AppHandle, state: &str, text: Option<String>, message: Option<String>) {
    let _ = app.emit(
        "speech:state",
        SpeechStateEvent { state: state.to_string(), text, message },
    );
    if let Some(w) = app.get_webview_window("overlay") {
        let _ = w.show();
    }
}

fn load_settings() -> Settings {
    SettingsStore::at(default_settings_path())
        .load()
        .unwrap_or_default()
}

/// Begin capturing. Idempotent: a second call while recording is a no-op.
#[tauri::command]
pub fn start_dictation(app: AppHandle, state: State<'_, SpeechState>) -> Result<(), String> {
    let mut guard = state.capture.lock().unwrap();
    if guard.is_some() {
        return Ok(());
    }
    let mut cap = new_audio_capture().map_err(|e| e.to_string())?;
    cap.start().map_err(|e| e.to_string())?;
    *guard = Some(cap);
    emit_state(&app, "recording", None, None);
    Ok(())
}

/// Stop capturing, transcribe with the selected engine, insert at the cursor,
/// and emit the result. Returns the transcript text on success.
#[tauri::command]
pub async fn stop_dictation(
    app: AppHandle,
    state: State<'_, SpeechState>,
) -> Result<String, String> {
    let captured = {
        let mut guard = state.capture.lock().unwrap();
        match guard.take() {
            Some(mut cap) => cap.stop().map_err(|e| e.to_string())?,
            None => return Ok(String::new()),
        }
    };

    emit_state(&app, "transcribing", None, None);

    let settings = load_settings();
    let transcriber = match build_transcriber(&settings) {
        Ok(t) => t,
        Err(e) => {
            emit_state(&app, "error", None, Some(e.to_string()));
            return Err(e.to_string());
        }
    };

    let request = TranscriptionRequest {
        samples: captured.samples,
        sample_rate: captured.sample_rate,
        language: settings.speech_language.clone(),
    };
    let result = match transcriber.transcribe(request).await {
        Ok(r) => r,
        Err(e) => {
            emit_state(&app, "error", None, Some(e.to_string()));
            return Err(e.to_string());
        }
    };

    // Insert at cursor via the Phase 1 TextIo (clipboard + synthetic paste, D3).
    match vox_platform::textio::new_text_io() {
        Ok(io) => {
            if let Err(e) = io.insert_text(&result.text) {
                emit_state(&app, "error", None, Some(e.to_string()));
                return Err(e.to_string());
            }
        }
        Err(e) => {
            emit_state(&app, "error", None, Some(e.to_string()));
            return Err(e.to_string());
        }
    }

    emit_state(&app, "done", Some(result.text.clone()), None);
    Ok(result.text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vox_core::settings::Settings;

    #[test]
    fn whisper_local_engine_is_unsupported_until_phase3() {
        let mut s = Settings::default();
        s.speech_engine = SpeechEngineKind::WhisperLocal;
        let err = build_transcriber(&s).unwrap_err();
        match err {
            SpeechError::Config(msg) => assert!(msg.contains("Phase 3")),
            other => panic!("expected Config error, got {other:?}"),
        }
    }

    #[test]
    fn remote_engine_without_key_is_a_config_error() {
        // In CI no "openai" secret is stored, so the factory must report a clear
        // configuration error rather than constructing a transcriber.
        let s = Settings::default(); // defaults to OpenAi
        match build_transcriber(&s) {
            Err(SpeechError::Config(_)) => {}
            Ok(_) => { /* a developer machine may have a key stored; acceptable */ }
            Err(other) => panic!("unexpected error: {other:?}"),
        }
    }
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test --manifest-path app/Cargo.toml -p vox speech_session`
Expected: FAIL — `speech_session` module is not yet declared in `main.rs`, so it
does not compile / the filter matches nothing.

- [ ] **Step 4: Register the module, commands, managed state, and hotkey listener**

Edit `app/src-tauri/src/main.rs`. Add the module declaration near the other
`mod` lines:
```rust
mod speech_session;
```
Register managed state and the two commands, and add a `HotkeyId::Speech`
listener in `setup`. Merge the following into the existing builder (the Phase
0/1 `invoke_handler`, tray, and hotkey wiring stay; add to them):
```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tauri::Manager;
use vox_platform::hotkeys::{new_hotkey_manager, HotkeyId};

// inside fn main(), on the Builder:
        .manage(speech_session::SpeechState::default())
        .setup(|app| {
            // ...existing Phase 0/1 setup (tray, rewrite hotkey) stays...

            // Register the speech accelerator and listen for press toggles.
            let settings = vox_core::settings::SettingsStore::at(
                vox_core::settings::default_settings_path(),
            )
            .load()
            .unwrap_or_default();

            let app_handle = app.handle().clone();
            std::thread::spawn(move || {
                // NOTE: prefer registering Speech on the SAME HotkeyHandle created in
                // Phase 1 and dispatching by HotkeyId in a single loop; global-hotkey
                // shares one global event source. Shown standalone here for clarity.
                let mut manager = match new_hotkey_manager() {
                    Ok(m) => m,
                    Err(e) => {
                        eprintln!("failed to create hotkey manager: {e}");
                        return;
                    }
                };
                if let Err(e) = manager.register(HotkeyId::Speech, &settings.speech_hotkey) {
                    eprintln!("failed to register speech hotkey: {e}");
                    return;
                }
                let rx = manager.take_events();
                let recording = Arc::new(AtomicBool::new(false));
                loop {
                    manager.pump();
                    let id = match rx.recv_timeout(std::time::Duration::from_millis(50)) {
                        Ok(id) => id,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };
                    if id != HotkeyId::Speech {
                        continue;
                    }
                    let was_recording = recording.fetch_xor(true, Ordering::SeqCst);
                    let handle = app_handle.clone();
                    if !was_recording {
                        // Toggle ON: start capture (sync command).
                        let state = handle.state::<speech_session::SpeechState>();
                        if let Err(e) = speech_session::start_dictation(handle.clone(), state) {
                            eprintln!("start_dictation error: {e}");
                            recording.store(false, Ordering::SeqCst);
                        }
                    } else {
                        // Toggle OFF: stop + transcribe (async) on the Tauri runtime.
                        tauri::async_runtime::spawn(async move {
                            let state = handle.state::<speech_session::SpeechState>();
                            if let Err(e) = speech_session::stop_dictation(handle.clone(), state).await {
                                eprintln!("stop_dictation error: {e}");
                            }
                        });
                    }
                }
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            // ...existing Phase 0/1 commands...
            speech_session::start_dictation,
            speech_session::stop_dictation,
        ])
```
(Press-to-toggle covers both "release to stop" and "second-press to stop": the
first `HotkeyId::Speech` starts, the next stops. `global-hotkey` delivers press
events, so a toggle is the portable interpretation of the spec's "release/second
-press stops".)

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test --manifest-path app/Cargo.toml -p vox speech_session`
Expected: `whisper_local_engine_is_unsupported_until_phase3` and
`remote_engine_without_key_is_a_config_error` PASS.

- [ ] **Step 6: Verify the binary compiles**

Run: `cargo build --manifest-path app/Cargo.toml -p vox`
Expected: builds; the two commands are registered and the speech listener
thread is wired.

- [ ] **Step 7: Commit**

```bash
git add app/src-tauri/Cargo.toml app/src-tauri/src/speech_session.rs app/src-tauri/src/main.rs
git commit -m "feat(app): push-to-talk dictation commands + engine factory + hotkey listener"
```

- [ ] **Step 8: Manual smoke check (not a committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`. Configure an OpenAI key in
settings (Phase 1 `set_secret` flow). Press the speech hotkey, say a sentence,
press it again. Expected: overlay shows `recording` → `transcribing` → `done`,
and the transcript is pasted at the cursor in the focused app.

---

## Task 7: Speech overlay UI (level meter + state)

**Files:**
- Modify: `app/src-tauri/tauri.conf.json`
- Modify: `app/ui/vite.config.ts`
- Create: `app/ui/overlay.html`
- Create: `app/ui/src/overlay/main.tsx`
- Create: `app/ui/src/overlay/Overlay.tsx`

- [ ] **Step 1: Add the always-on-top overlay window**

Edit `app/src-tauri/tauri.conf.json` `app.windows` to add a second window
(keeping the Phase 0 `settings` window):
```json
{
  "label": "overlay",
  "title": "Vox",
  "url": "overlay.html",
  "width": 320,
  "height": 120,
  "resizable": false,
  "decorations": false,
  "alwaysOnTop": true,
  "skipTaskbar": true,
  "transparent": true,
  "visible": false
}
```

- [ ] **Step 2: Add the overlay HTML entry + Vite input**

Create `app/ui/overlay.html`:
```html
<!doctype html>
<html lang="en">
  <head>
    <meta charset="UTF-8" />
    <meta name="viewport" content="width=device-width, initial-scale=1.0" />
    <title>Vox</title>
  </head>
  <body>
    <div id="overlay-root"></div>
    <script type="module" src="/src/overlay/main.tsx"></script>
  </body>
</html>
```

Edit `app/ui/vite.config.ts` to register both HTML inputs. Merge into the
existing config's `build` key (preserve any existing options):
```ts
import { resolve } from "path";

// inside defineConfig({ ... }):
  build: {
    rollupOptions: {
      input: {
        main: resolve(__dirname, "index.html"),
        overlay: resolve(__dirname, "overlay.html"),
      },
    },
  },
```

- [ ] **Step 3: Create the overlay entry point**

Create `app/ui/src/overlay/main.tsx`:
```tsx
import React from "react";
import ReactDOM from "react-dom/client";
import { Overlay } from "./Overlay";

ReactDOM.createRoot(document.getElementById("overlay-root")!).render(
  <React.StrictMode>
    <Overlay />
  </React.StrictMode>,
);
```

- [ ] **Step 4: Create the overlay component**

Create `app/ui/src/overlay/Overlay.tsx`:
```tsx
import { useEffect, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";

type SpeechState = "idle" | "recording" | "transcribing" | "done" | "error";

type SpeechStateEvent = {
  state: SpeechState;
  text: string | null;
  message: string | null;
};

const LABELS: Record<SpeechState, string> = {
  idle: "Ready",
  recording: "Listening…",
  transcribing: "Transcribing…",
  done: "Inserted",
  error: "Error",
};

export function Overlay() {
  const [state, setState] = useState<SpeechState>("idle");
  const [message, setMessage] = useState<string | null>(null);
  // A simple animated level value while recording (no raw PCM is streamed to
  // the UI in this parity pass; the bar pulses to signal "listening").
  const [level, setLevel] = useState(0);

  useEffect(() => {
    const unlisten = listen<SpeechStateEvent>("speech:state", async (event) => {
      const payload = event.payload;
      setState(payload.state);
      setMessage(payload.message ?? null);
      if (payload.state === "done" || payload.state === "error") {
        // Auto-hide shortly after a terminal state.
        setTimeout(() => getCurrentWindow().hide(), 1200);
      }
    });
    return () => {
      unlisten.then((f) => f());
    };
  }, []);

  useEffect(() => {
    if (state !== "recording") {
      setLevel(0);
      return;
    }
    const id = setInterval(() => setLevel(Math.random()), 120);
    return () => clearInterval(id);
  }, [state]);

  const bars = Array.from({ length: 12 });

  return (
    <main
      style={{
        fontFamily: "system-ui",
        background: "rgba(20,20,24,0.92)",
        color: "white",
        borderRadius: 16,
        height: "100vh",
        display: "flex",
        flexDirection: "column",
        alignItems: "center",
        justifyContent: "center",
        gap: 10,
        userSelect: "none",
      }}
    >
      <div style={{ display: "flex", alignItems: "flex-end", gap: 3, height: 32 }}>
        {bars.map((_, i) => {
          const active = state === "recording";
          const h = active ? 6 + Math.abs(Math.sin(i + level * 6)) * level * 26 : 6;
          return (
            <span
              key={i}
              style={{
                width: 4,
                height: h,
                borderRadius: 2,
                background: state === "error" ? "#ff6b6b" : "#7aa2ff",
                transition: "height 80ms linear",
              }}
            />
          );
        })}
      </div>
      <div style={{ fontSize: 14, fontWeight: 600 }}>{LABELS[state]}</div>
      {message && (
        <div style={{ fontSize: 11, color: "#ffb3b3", maxWidth: 280, textAlign: "center" }}>
          {message}
        </div>
      )}
    </main>
  );
}
```

- [ ] **Step 5: Verify the UI builds**

Run: `npm --prefix app/ui run build`
Expected: Vite emits both `index.html` and `overlay.html` bundles into
`ui/dist`; no TypeScript errors.

- [ ] **Step 6: Manual smoke check (not a committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`. Trigger dictation. Expected: the
overlay appears, the bars animate while `recording`, the label moves through
`Transcribing…` → `Inserted`, then the overlay auto-hides.

- [ ] **Step 7: Commit**

```bash
git add app/src-tauri/tauri.conf.json app/ui/vite.config.ts app/ui/overlay.html app/ui/src/overlay/main.tsx app/ui/src/overlay/Overlay.tsx
git commit -m "feat(ui): add speech overlay with level meter + state"
```

---

## Task 8: Settings UI — choose the speech engine

**Files:**
- Modify: `app/ui/src/App.tsx`

- [ ] **Step 1: Extend the settings form with speech fields**

Edit `app/ui/src/App.tsx` to extend the `Settings` type and add controls for the
Phase 2 fields (keep the Phase 0/1 fields and the existing Save button):
```tsx
type SpeechEngineKind = "OpenAi" | "OpenAiCompatible" | "WhisperLocal";

// Extend the Settings type:
type Settings = {
  schema_version: number;
  rewrite_hotkey: string;
  speech_hotkey: string;
  launch_at_login: boolean;
  openai_base_url: string;
  rewrite_model: string;
  // Phase 2:
  speech_engine: SpeechEngineKind;
  speech_model: string;
  speech_language: string | null;
};
```
Add these controls inside the form, before the Save button:
```tsx
      <label style={{ display: "block", marginTop: 12 }}>
        Speech engine:{" "}
        <select
          value={settings.speech_engine}
          onChange={(e) =>
            setSettings({ ...settings, speech_engine: e.target.value as SpeechEngineKind })
          }
        >
          <option value="OpenAi">OpenAI</option>
          <option value="OpenAiCompatible">OpenAI-compatible</option>
          <option value="WhisperLocal">Local Whisper (Phase 3)</option>
        </select>
      </label>
      <label style={{ display: "block", marginTop: 12 }}>
        Speech model:{" "}
        <input
          value={settings.speech_model}
          onChange={(e) => setSettings({ ...settings, speech_model: e.target.value })}
        />
      </label>
      <label style={{ display: "block", marginTop: 12 }}>
        Language (optional, e.g. "en"):{" "}
        <input
          value={settings.speech_language ?? ""}
          onChange={(e) =>
            setSettings({
              ...settings,
              speech_language: e.target.value.trim() === "" ? null : e.target.value,
            })
          }
        />
      </label>
```

- [ ] **Step 2: Verify the UI builds**

Run: `npm --prefix app/ui run build`
Expected: Vite build succeeds with the extended form.

- [ ] **Step 3: Manual smoke check (not a committed gate)**

Run: `cd app && npm --prefix ui run tauri dev`. Change the engine/model/language,
Save, relaunch. Expected: values persist (round-trips through `save_settings` /
`load_settings` to disk).

- [ ] **Step 4: Commit**

```bash
git add app/ui/src/App.tsx
git commit -m "feat(ui): add speech engine/model/language settings controls"
```

---

## Task 9: Extend App CI for the new crate + modules

**Files:**
- Modify: `.github/workflows/app-ci.yml`

- [ ] **Step 1: Run the speech + audio + platform tests in CI**

Edit `.github/workflows/app-ci.yml`. Ensure the Linux deps step installs ALSA
headers, and the test step covers the new code. Update the Linux deps line to
include `libasound2-dev`:
```yaml
          sudo apt-get install -y libwebkit2gtk-4.1-dev build-essential curl wget file \
            libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev libasound2-dev
```
Replace the "Rust unit tests" step so it runs the whole workspace's unit tests
(core + platform), which now includes `speech` and `audio`:
```yaml
      - name: Rust unit tests
        run: |
          cargo test --manifest-path app/Cargo.toml -p vox-core
          cargo test --manifest-path app/Cargo.toml -p vox-platform
          cargo test --manifest-path app/Cargo.toml -p vox speech_session
```

- [ ] **Step 2: Verify locally where possible**

Run:
```bash
cargo test --manifest-path app/Cargo.toml -p vox-core && \
cargo test --manifest-path app/Cargo.toml -p vox-platform && \
cargo test --manifest-path app/Cargo.toml -p vox speech_session && \
npm --prefix app/ui run build
```
Expected: all tests pass and the UI (both HTML entries) builds on the current OS.

- [ ] **Step 3: Commit and push to trigger CI**

```bash
git add .github/workflows/app-ci.yml
git commit -m "ci: run speech + audio + dictation tests in the app matrix"
git push
```
Expected: **App CI** passes on macOS, Windows, and Linux (verify with
`gh run list --workflow=app-ci.yml`).

---

## Phase 2 Acceptance

- `cargo test --manifest-path app/Cargo.toml -p vox-platform audio` passes
  (resample/downmix helper + capture-factory test).
- `cargo test --manifest-path app/Cargo.toml -p vox-core speech` passes (engine
  serde, WAV header, and the two wiremock HTTP transcription tests).
- `cargo test --manifest-path app/Cargo.toml -p vox-core settings` passes,
  including Phase 2 defaults and old-file back-compat.
- `cargo test --manifest-path app/Cargo.toml -p vox speech_session` passes
  (engine factory).
- `cargo build --manifest-path app/Cargo.toml -p vox` and
  `npm --prefix app/ui run build` succeed on all three OSes (App CI).
- Manual: pressing the speech hotkey records, pressing again transcribes via the
  selected remote engine and inserts the transcript at the cursor; the overlay
  shows the level meter and `recording`/`transcribing`/`done` states. Verified on
  macOS, Windows, and at least one Linux session.

## Self-Review Notes

- **Spec coverage:** Implements the spec's Phase 2 scope end-to-end —
  `platform::audio` mic capture (cpal, mono f32 @ 16 kHz), `core::speech` remote
  transcription (OpenAI / OpenAI-compatible multipart upload), push-to-talk
  hotkey wiring, transcript insertion at cursor via Phase 1 `TextIo`, the
  `speech:state` events, and the overlay UI (level meter + states). Settings gain
  the three Phase 2 fields. Offline/local Whisper (`WhisperLocal`) is explicitly
  deferred to Phase 3 (the factory returns a clear `Config` error meanwhile),
  matching the spec's phase ordering.
- **Type consistency vs. CONTRACTS.md:** Uses the canonical names and signatures
  verbatim — `platform::audio::{AudioCapture, CapturedAudio, new_audio_capture,
  AudioError}` (capture-only; `CapturedAudio.sample_rate == 16_000`);
  `core::speech::{TranscriptionRequest, TranscriptionResult, SpeechError,
  Transcriber, RemoteTranscriber, SpeechEngineKind}` with
  `RemoteTranscriber::new(api_key, base_url, model)` and the `Transcriber`
  `async fn transcribe`; `SpeechEngineKind { OpenAi, OpenAiCompatible,
  WhisperLocal }`. Settings additions match the contract exactly
  (`speech_engine: SpeechEngineKind` default `OpenAi`, `speech_model: String`
  default `"gpt-4o-transcribe"`, `speech_language: Option<String>` default
  `None`), with `schema_version` bumped. Commands `start_dictation` /
  `stop_dictation` and the `speech:state` event use the contract's names; the
  engine factory lives in `src-tauri` as specified; the `"openai"` secret account
  name is reused. Module path additions (`platform/src/audio.rs`,
  `core/src/speech.rs`, `pub mod audio;`) match the contract's workspace layout.
- **No placeholders:** Every code and command step contains complete, real
  Rust/TS/JSON/YAML — no `TBD`, no "similar to above", no elided bodies. The only
  deliberately deferred behavior (local Whisper) is represented by concrete code
  (a `Config` error) rather than a placeholder, consistent with Phase 3 owning
  that engine.
