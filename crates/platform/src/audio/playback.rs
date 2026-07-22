//! PCM playback via rodio (macOS default output device).
//!
//! # Manual verification (macOS)
//! 1. Build and run KEA on hardware with speakers/headphones.
//! 2. Synthesize or capture a short mono PCM buffer (e.g. 440 Hz tone at 48 kHz).
//! 3. Call `AudioIo::play` on `MacAudioIo` — audio should be audible.
//! 4. Unit tests do **not** assert real audio output; they cover pure helpers only.

use super::{AudioIoError, PcmFrame};

/// Clamp mono samples to \[-1.0, 1.0\] before handing to the audio output device.
pub fn clamp_pcm_samples(samples: &[f32]) -> Vec<f32> {
    samples.iter().map(|s| s.clamp(-1.0, 1.0)).collect()
}

/// Validate a PCM frame is suitable for playback.
pub fn validate_pcm_for_playback(frame: &PcmFrame) -> Result<(), AudioIoError> {
    if frame.samples.is_empty() {
        return Err(AudioIoError::Other("empty pcm buffer".into()));
    }
    if frame.sample_rate_hz == 0 {
        return Err(AudioIoError::Other("invalid sample rate".into()));
    }
    Ok(())
}

/// Prepare clamped samples for rodio playback.
pub fn pcm_samples_for_playback(frame: &PcmFrame) -> Result<Vec<f32>, AudioIoError> {
    validate_pcm_for_playback(frame)?;
    Ok(clamp_pcm_samples(&frame.samples))
}

/// Play mono PCM on the default output device (blocking; intended for `spawn_blocking`).
#[cfg(target_os = "macos")]
pub fn play_pcm_blocking(pcm: &PcmFrame) -> Result<(), AudioIoError> {
    use rodio::{buffer::SamplesBuffer, OutputStream, Sink};

    let samples = pcm_samples_for_playback(pcm)?;
    let (_stream, stream_handle) = OutputStream::try_default()
        .map_err(|e| AudioIoError::Other(format!("audio output unavailable: {e}")))?;
    let sink = Sink::try_new(&stream_handle)
        .map_err(|e| AudioIoError::Other(format!("audio sink unavailable: {e}")))?;
    let source = SamplesBuffer::new(1, pcm.sample_rate_hz, samples);
    sink.append(source);
    sink.sleep_until_end();
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn play_pcm_blocking(pcm: &PcmFrame) -> Result<(), AudioIoError> {
    let _ = pcm;
    Err(AudioIoError::Other("playback not supported on this platform".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_pcm_samples_limits_out_of_range_values() {
        let clamped = clamp_pcm_samples(&[2.0, -2.0, 0.5]);
        assert_eq!(clamped, vec![1.0, -1.0, 0.5]);
    }

    #[test]
    fn validate_pcm_rejects_empty_buffer() {
        let frame = PcmFrame {
            samples: vec![],
            sample_rate_hz: 48_000,
        };
        assert!(validate_pcm_for_playback(&frame).is_err());
    }

    #[test]
    fn pcm_samples_for_playback_clamps_values() {
        let frame = PcmFrame {
            samples: vec![1.5, -0.5],
            sample_rate_hz: 24_000,
        };
        let out = pcm_samples_for_playback(&frame).unwrap();
        assert_eq!(out, vec![1.0, -0.5]);
    }
}
