//! Feeds a WAV through the streaming recognizer and prints partials plus
//! timings.
//!
//! Not CI-able — it needs a real model bundle on disk — which is exactly why
//! it exists: the numbers this prints are the only honest answer to "is the
//! decoder fast enough, and are the endpoint thresholds right?". The
//! decode-time-per-audio-second it reports is what decides whether
//! `StreamingCfg::num_threads` is set correctly for a given machine.
//!
//! ```text
//! cargo run -p kea-infer --features sherpa --example streaming_probe -- \
//!     ~/Library/Application\ Support/ai.kea.app/models/streaming/streaming-zipformer-en-20m \
//!     some-speech.wav
//! ```
//!
//! The WAV must be 16-bit PCM mono at 16 kHz, which is what the capture path
//! resamples to anyway.

#[cfg(not(feature = "sherpa"))]
fn main() {
    eprintln!("streaming_probe needs the `sherpa` feature: cargo run -p kea-infer --features sherpa --example streaming_probe -- <model-dir> <wav>");
}

#[cfg(feature = "sherpa")]
fn main() {
    use kea_infer::{
        AudioPcm, SherpaOnnxStreamingInference, SherpaStreamingInference, StreamingCfg,
    };
    use std::path::Path;
    use std::time::Instant;

    let mut args = std::env::args().skip(1);
    let (Some(model_dir), Some(wav)) = (args.next(), args.next()) else {
        eprintln!("usage: streaming_probe <model-dir> <wav>");
        std::process::exit(2);
    };

    let samples = match read_wav_mono_16k(Path::new(&wav)) {
        Ok(samples) => samples,
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    };
    let audio_seconds = samples.len() as f64 / 16_000.0;
    println!(
        "audio:    {:.2}s ({} samples)",
        audio_seconds,
        samples.len()
    );

    let cfg = StreamingCfg::default();
    println!("config:   {cfg:?}");

    let opened = Instant::now();
    let mut session = match SherpaOnnxStreamingInference::new().open(Path::new(&model_dir), cfg) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("failed to open the recognizer: {e}");
            std::process::exit(1);
        }
    };
    println!("open:     {:?}", opened.elapsed());

    // 20 ms chunks: the size a capture callback hands over, so the timings
    // reflect the real per-frame cost rather than one big batch.
    const CHUNK: usize = 320;
    let started = Instant::now();
    let mut partials = 0usize;
    let mut endpoints = 0usize;
    for chunk in samples.chunks(CHUNK) {
        let at = started.elapsed();
        match session.accept(AudioPcm {
            samples: chunk.to_vec(),
            sample_rate_hz: 16_000,
        }) {
            Ok(Some(text)) => {
                partials += 1;
                println!("  {:>8.3}s  {text}", at.as_secs_f64());
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("decode failed: {e}");
                std::process::exit(1);
            }
        }
        if session.endpointed() {
            endpoints += 1;
            println!("  {:>8.3}s  -- endpoint --", at.as_secs_f64());
        }
    }
    let decode = started.elapsed();

    match session.finish() {
        Ok(text) => println!("final:    {text}"),
        Err(e) => eprintln!("finish failed: {e}"),
    }

    println!("partials: {partials}");
    // An endpoint per frame is what a zero rule threshold produces; seeing
    // that here is the signal that the config never reached sherpa.
    println!(
        "endpoints: {endpoints} ({:.2} per audio second)",
        endpoints as f64 / audio_seconds.max(f64::EPSILON)
    );
    println!("decode:   {:?}", decode);
    println!(
        "realtime factor: {:.3} (decode seconds per audio second)",
        decode.as_secs_f64() / audio_seconds.max(f64::EPSILON)
    );
}

/// A minimal 16-bit PCM WAV reader: enough for a probe, and one less
/// dependency than pulling a crate in for it.
#[cfg(feature = "sherpa")]
fn read_wav_mono_16k(path: &std::path::Path) -> Result<Vec<f32>, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    if bytes.len() < 44 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(format!("{} is not a RIFF/WAVE file", path.display()));
    }

    let mut pos = 12;
    let mut channels = 0u16;
    let mut rate = 0u32;
    let mut bits = 0u16;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32::from_le_bytes(bytes[pos + 4..pos + 8].try_into().unwrap()) as usize;
        let body = pos + 8;
        match id {
            b"fmt " if body + 16 <= bytes.len() => {
                channels = u16::from_le_bytes(bytes[body + 2..body + 4].try_into().unwrap());
                rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into().unwrap());
                bits = u16::from_le_bytes(bytes[body + 14..body + 16].try_into().unwrap());
            }
            b"data" => {
                if channels != 1 || rate != 16_000 || bits != 16 {
                    return Err(format!(
                        "need 16-bit mono 16kHz, got {bits}-bit {channels}ch {rate}Hz"
                    ));
                }
                let end = (body + size).min(bytes.len());
                return Ok(bytes[body..end]
                    .chunks_exact(2)
                    .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / i16::MAX as f32)
                    .collect());
            }
            _ => {}
        }
        // Chunks are word-aligned; an odd size carries a pad byte.
        pos = body + size + (size % 2);
    }
    Err(format!("{} has no data chunk", path.display()))
}
