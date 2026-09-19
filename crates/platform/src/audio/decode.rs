//! Decoding an audio or video file to the mono PCM the STT engines want.
//!
//! ## Why symphonia directly, and not `rodio::Decoder`
//!
//! `rodio` is already a dependency of this crate, but only for *output*
//! (`playback.rs`); `rodio::Decoder` has never been constructed here. What it
//! would give is decided by the features rodio enables on its own symphonia
//! dependency, and the lockfile says that is `symphonia-bundle-mp3` alone.
//! So the rodio route decodes WAV, FLAC, Ogg/Vorbis and MP3 — and rejects
//! M4A, MP4, MOV, ALAC, CAF and Opus. macOS Voice Memos writes `.m4a`,
//! iPhone recordings are `.m4a`, meeting and screen recordings are `.mp4` or
//! `.mov`. A file-transcription feature that rejects all of those is not the
//! feature, so symphonia is depended on directly with its `all` feature set.
//!
//! The tradeoff paid for that: one more direct dependency and a larger
//! binary (the AAC, ALAC and ISO-MP4 code), against a decoder whose format
//! coverage is ours to state rather than a transitive consequence of
//! rodio's feature flags — which can change under us in a patch release.

use std::path::Path;

use symphonia::core::audio::{AudioBufferRef, Signal};
use symphonia::core::codecs::{DecoderOptions, CODEC_TYPE_NULL};
use symphonia::core::errors::Error as SymphoniaError;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

use super::util::resample_linear;
use super::PcmFrame;

/// The rate every STT engine in the app is fed at.
pub const DECODE_SAMPLE_RATE_HZ: u32 = 16_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DecodeError {
    #[error("could not open {path}: {message}")]
    Open { path: String, message: String },
    /// The container was read but nothing in it can be decoded — a video with
    /// no audio track, or a codec symphonia does not implement. Named
    /// separately from `Open` because the advice differs: converting the file
    /// will help here and will not help a file that is simply missing.
    #[error("no audio track this build can decode in {path}{}", detail_suffix(.detail))]
    Unsupported {
        path: String,
        detail: Option<String>,
    },
    #[error("{path} is damaged or truncated: {message}")]
    Damaged { path: String, message: String },
}

fn detail_suffix(detail: &Option<String>) -> String {
    match detail {
        Some(d) => format!(" ({d})"),
        None => String::new(),
    }
}

/// Decodes `path` to mono f32 at [`DECODE_SAMPLE_RATE_HZ`].
///
/// Blocking and synchronous — it reads a file and runs a decoder — so callers
/// run it on a blocking pool. It materializes the whole file: one hour of
/// 16 kHz mono f32 is about 230 MB, which is the memory ceiling this design
/// accepts in exchange for `cut_points` being able to look at the waveform
/// either side of a boundary. If two-hour video files become normal, this
/// signature is what has to change first, so it is worth noting here.
pub fn decode_file(path: &Path) -> Result<PcmFrame, DecodeError> {
    decode_file_at(path, DECODE_SAMPLE_RATE_HZ)
}

/// [`decode_file`] at an explicit rate, so a test can assert the resample.
pub fn decode_file_at(path: &Path, target_rate_hz: u32) -> Result<PcmFrame, DecodeError> {
    let name = path.display().to_string();
    let open = |message: String| DecodeError::Open {
        path: name.clone(),
        message,
    };

    let file = std::fs::File::open(path).map_err(|e| open(e.to_string()))?;
    let stream = MediaSourceStream::new(Box::new(file), Default::default());

    // The extension is a *hint*, never the decision: symphonia probes the
    // bytes. A `.m4a` that is really an MP3 still decodes, and a file with no
    // extension at all still probes.
    let mut hint = Hint::new();
    if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
        hint.with_extension(ext);
    }

    let probed = symphonia::default::get_probe()
        .format(
            &hint,
            stream,
            &FormatOptions {
                enable_gapless: true,
                ..Default::default()
            },
            &MetadataOptions::default(),
        )
        .map_err(|e| match e {
            SymphoniaError::Unsupported(detail) => DecodeError::Unsupported {
                path: name.clone(),
                detail: Some(detail.to_string()),
            },
            other => open(other.to_string()),
        })?;

    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != CODEC_TYPE_NULL)
        .ok_or_else(|| DecodeError::Unsupported {
            path: name.clone(),
            detail: None,
        })?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|e| DecodeError::Unsupported {
            path: name.clone(),
            detail: Some(e.to_string()),
        })?;

    let mut mono: Vec<f32> = Vec::new();
    let mut source_rate = track.codec_params.sample_rate.unwrap_or(0);

    loop {
        let packet = match format.next_packet() {
            Ok(packet) => packet,
            // Clean end of stream. symphonia signals it as an IO error with
            // `UnexpectedEof`, which is not a failure and must not be
            // reported as a damaged file.
            Err(SymphoniaError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => {
                break
            }
            Err(SymphoniaError::ResetRequired) => {
                // A new stream begins (a chained Ogg). Nothing downstream
                // handles a rate change mid-file, so stop with what we have
                // rather than splice two timelines together.
                break;
            }
            Err(e) => {
                return Err(DecodeError::Damaged {
                    path: name,
                    message: e.to_string(),
                })
            }
        };
        if packet.track_id() != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buffer) => {
                if source_rate == 0 {
                    source_rate = buffer.spec().rate;
                }
                append_mono(&buffer, &mut mono);
            }
            // A corrupted or skipped packet is recoverable by design in
            // symphonia: drop it and keep going, because one bad frame in a
            // 40-minute recording must not cost the whole transcript.
            Err(SymphoniaError::DecodeError(_)) | Err(SymphoniaError::IoError(_)) => continue,
            Err(e) => {
                return Err(DecodeError::Damaged {
                    path: name,
                    message: e.to_string(),
                })
            }
        }
    }

    if mono.is_empty() || source_rate == 0 {
        return Err(DecodeError::Unsupported {
            path: name,
            detail: Some("the audio track decoded to no samples".into()),
        });
    }

    let decoded = PcmFrame {
        samples: mono,
        sample_rate_hz: source_rate,
    };
    Ok(resample_linear(&decoded, target_rate_hz))
}

/// Appends one decoded buffer as mono f32, whatever sample format it is in.
///
/// `util::downmix_to_mono` is the capture path's primitive and is deliberately
/// *not* reused here: it takes an interleaved slice, and symphonia decodes
/// into planar buffers — one slice per channel. Interleaving a buffer only to
/// average it back down would double the allocation on every packet of a
/// two-hour file. Resampling *is* shared: `resample_linear` is the same
/// function the capture path uses.
fn append_mono(buffer: &AudioBufferRef<'_>, out: &mut Vec<f32>) {
    macro_rules! planar_to_mono {
        ($buf:expr, $conv:expr) => {{
            let buf = $buf;
            let channels = buf.spec().channels.count().max(1);
            let frames = buf.frames();
            out.reserve(frames);
            for frame in 0..frames {
                let mut sum = 0.0f32;
                for channel in 0..channels {
                    sum += $conv(buf.chan(channel)[frame]);
                }
                out.push(sum / channels as f32);
            }
        }};
    }

    match buffer {
        AudioBufferRef::F32(buf) => planar_to_mono!(buf, |s: f32| s),
        AudioBufferRef::F64(buf) => planar_to_mono!(buf, |s: f64| s as f32),
        AudioBufferRef::S32(buf) => {
            planar_to_mono!(buf, |s: i32| s as f32 / i32::MAX as f32)
        }
        AudioBufferRef::S24(buf) => {
            planar_to_mono!(buf, |s: symphonia::core::sample::i24| s.inner() as f32
                / 8_388_608.0)
        }
        AudioBufferRef::S16(buf) => {
            planar_to_mono!(buf, |s: i16| s as f32 / i16::MAX as f32)
        }
        AudioBufferRef::S8(buf) => planar_to_mono!(buf, |s: i8| s as f32 / i8::MAX as f32),
        AudioBufferRef::U32(buf) => {
            planar_to_mono!(buf, |s: u32| (s as f32 / u32::MAX as f32) * 2.0 - 1.0)
        }
        AudioBufferRef::U24(buf) => {
            planar_to_mono!(buf, |s: symphonia::core::sample::u24| (s.inner() as f32
                / 16_777_215.0)
                * 2.0
                - 1.0)
        }
        AudioBufferRef::U16(buf) => {
            planar_to_mono!(buf, |s: u16| (s as f32 / u16::MAX as f32) * 2.0 - 1.0)
        }
        AudioBufferRef::U8(buf) => {
            planar_to_mono!(buf, |s: u8| (s as f32 / u8::MAX as f32) * 2.0 - 1.0)
        }
    }
}

/// Whether the extension looks like something worth handing to the decoder.
///
/// Advisory only — [`decode_file`] probes the bytes and will happily decode a
/// mislabelled file — but a drop zone has to reject a dragged `.pdf` before
/// it spends a second opening it, and this is what the UI filter is built on.
pub fn is_probably_decodable(path: &Path) -> bool {
    const KNOWN: &[&str] = &[
        "wav", "wave", "mp3", "m4a", "m4b", "mp4", "mov", "aac", "flac", "ogg", "oga", "opus",
        "caf", "aiff", "aif", "aifc", "mka", "mkv", "webm", "alac", "amr", "3gp",
    ];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| KNOWN.contains(&e.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 16-bit PCM WAV of `secs` seconds of a sine at `freq_hz`, written
    /// byte by byte so the test needs no encoder and no committed binary.
    fn sine_wav(path: &Path, rate: u32, channels: u16, secs: f32, freq_hz: f32) {
        let frames = (rate as f32 * secs) as u32;
        let data_len = frames * channels as u32 * 2;
        let mut out: Vec<u8> = Vec::with_capacity(44 + data_len as usize);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes()); // PCM
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        out.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
        out.extend_from_slice(&(channels * 2).to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let t = i as f32 / rate as f32;
            let value = (std::f32::consts::TAU * freq_hz * t).sin() * 0.5;
            let sample = (value * i16::MAX as f32) as i16;
            for _ in 0..channels {
                out.extend_from_slice(&sample.to_le_bytes());
            }
        }
        std::fs::write(path, out).unwrap();
    }

    #[test]
    fn a_one_second_wav_decodes_to_one_second_at_the_target_rate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tone.wav");
        sine_wav(&path, 44_100, 1, 1.0, 440.0);

        let pcm = decode_file(&path).unwrap();
        assert_eq!(pcm.sample_rate_hz, DECODE_SAMPLE_RATE_HZ);
        let expected = DECODE_SAMPLE_RATE_HZ as i64;
        let got = pcm.samples.len() as i64;
        assert!(
            (got - expected).abs() <= 1,
            "expected ~{expected} samples, got {got}"
        );
        // Not silence: a decoder that produced the right *count* of zeros
        // would pass a length-only assertion.
        assert!(super::super::util::rms_level(&pcm.samples) > 0.1);
    }

    /// Stereo has to fold to mono, not be handed through as twice the frames.
    #[test]
    fn a_stereo_file_decodes_to_the_same_length_as_mono() {
        let dir = tempfile::tempdir().unwrap();
        let mono_path = dir.path().join("mono.wav");
        let stereo_path = dir.path().join("stereo.wav");
        sine_wav(&mono_path, 16_000, 1, 0.5, 440.0);
        sine_wav(&stereo_path, 16_000, 2, 0.5, 440.0);

        let mono = decode_file(&mono_path).unwrap();
        let stereo = decode_file(&stereo_path).unwrap();
        assert_eq!(mono.samples.len(), stereo.samples.len());
    }

    #[test]
    fn a_file_that_is_not_audio_is_refused_rather_than_decoded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("notes.txt");
        std::fs::write(&path, b"this is not audio, it is prose").unwrap();
        let err = decode_file(&path).unwrap_err();
        assert!(
            matches!(
                err,
                DecodeError::Unsupported { .. } | DecodeError::Open { .. }
            ),
            "{err:?}"
        );
    }

    /// A half-written download must produce an error, never a panic.
    #[test]
    fn a_truncated_file_errors_instead_of_panicking() {
        let dir = tempfile::tempdir().unwrap();
        let full = dir.path().join("full.wav");
        sine_wav(&full, 16_000, 1, 0.5, 440.0);
        let bytes = std::fs::read(&full).unwrap();
        let cut = dir.path().join("cut.wav");
        std::fs::write(&cut, &bytes[..bytes.len() / 3]).unwrap();

        // Either it errors, or it decodes the bytes that are there. What it
        // must not do is panic or hand back a frame at rate 0.
        if let Ok(pcm) = decode_file(&cut) {
            assert!(pcm.sample_rate_hz > 0);
        }
    }

    #[test]
    fn a_missing_file_names_itself_in_the_error() {
        let err = decode_file(Path::new("/nonexistent/nope.wav")).unwrap_err();
        assert!(err.to_string().contains("nope.wav"), "{err}");
    }

    #[test]
    fn the_extension_filter_admits_video_containers_and_rejects_documents() {
        for name in ["a.m4a", "b.MP4", "c.mov", "d.wav", "e.flac", "f.opus"] {
            assert!(is_probably_decodable(Path::new(name)), "{name}");
        }
        for name in ["a.pdf", "b.txt", "c", "d.srt"] {
            assert!(!is_probably_decodable(Path::new(name)), "{name}");
        }
    }
}
