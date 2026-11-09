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

/// Play `pcm`, stopping early if `cancel` is set.
///
/// A separate entry point rather than a flag on [`play_pcm_blocking`] because
/// the two differ in what they own: the plain version hands the sink to
/// `sleep_until_end` and never looks at it again, which is why nothing outside
/// can stop it. This one keeps the sink and polls, so a spoken answer can be
/// cut off the moment the user has heard enough — or has heard that their
/// question was misunderstood, which is the more common reason.
///
/// Polls rather than blocking on a channel so the caller needs only an
/// `AtomicBool`, which is what a hotkey handler in another task can set
/// without plumbing a sender through the session.
#[cfg(target_os = "macos")]
pub fn play_pcm_cancellable(
    pcm: &PcmFrame,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), AudioIoError> {
    use rodio::{buffer::SamplesBuffer, OutputStream, Sink};
    use std::sync::atomic::Ordering;

    // Checked before any device is opened: a cancel that arrived while the
    // answer was still being synthesized must not produce a burst of audio.
    if cancel.load(Ordering::Relaxed) {
        return Ok(());
    }

    let samples = pcm_samples_for_playback(pcm)?;
    let (_stream, stream_handle) = OutputStream::try_default()
        .map_err(|e| AudioIoError::Other(format!("audio output unavailable: {e}")))?;
    let sink = Sink::try_new(&stream_handle)
        .map_err(|e| AudioIoError::Other(format!("audio sink unavailable: {e}")))?;
    sink.append(SamplesBuffer::new(1, pcm.sample_rate_hz, samples));

    if drain_until_cancelled(|| sink.empty(), cancel, CANCEL_POLL) {
        sink.stop();
    }
    Ok(())
}

/// How often a playing sink is checked for a stop request.
///
/// 50 ms is the same cadence the mic level poll runs at. It is finer than one
/// output buffer, so a stop is as immediate as the device allows, and slow
/// enough to cost nothing while audio plays.
#[cfg(target_os = "macos")]
const CANCEL_POLL: std::time::Duration = std::time::Duration::from_millis(50);

/// Block until `is_done` reports the audio has drained or `cancel` is set,
/// checking every `poll`. Returns `true` when it ended because of `cancel`.
///
/// Split from the rodio sink so that "a stop ends playback" can be asserted
/// without an output device: tests hand it a sink that never empties.
pub fn drain_until_cancelled(
    mut is_done: impl FnMut() -> bool,
    cancel: &std::sync::atomic::AtomicBool,
    poll: std::time::Duration,
) -> bool {
    use std::sync::atomic::Ordering;
    while !is_done() {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        std::thread::sleep(poll);
    }
    false
}

#[cfg(not(target_os = "macos"))]
pub fn play_pcm_cancellable(
    pcm: &PcmFrame,
    cancel: &std::sync::atomic::AtomicBool,
) -> Result<(), AudioIoError> {
    let _ = (pcm, cancel);
    Err(AudioIoError::Other(
        "playback not supported on this platform".into(),
    ))
}

#[cfg(not(target_os = "macos"))]
pub fn play_pcm_blocking(pcm: &PcmFrame) -> Result<(), AudioIoError> {
    let _ = pcm;
    Err(AudioIoError::Other(
        "playback not supported on this platform".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_stop_ends_a_sink_that_would_never_drain() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        use std::time::{Duration, Instant};

        let cancel = Arc::new(AtomicBool::new(false));
        let setter = Arc::clone(&cancel);
        let started = Instant::now();
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(30));
            setter.store(true, Ordering::Relaxed);
        });
        // A sink that never empties: only the stop can end this.
        let stopped = drain_until_cancelled(|| false, &cancel, Duration::from_millis(5));
        handle.join().unwrap();
        assert!(stopped);
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn audio_that_drains_on_its_own_is_not_reported_as_stopped() {
        use std::sync::atomic::AtomicBool;
        use std::time::Duration;

        let cancel = AtomicBool::new(false);
        let mut polls = 0;
        let stopped = drain_until_cancelled(
            || {
                polls += 1;
                polls > 3
            },
            &cancel,
            Duration::from_millis(1),
        );
        assert!(!stopped);
    }

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
