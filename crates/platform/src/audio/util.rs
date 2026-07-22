//! Pure audio helpers (resample, RMS, frame accumulation).

use super::PcmFrame;

/// Linearly resample `frame` to `target_rate_hz`.
pub fn resample_linear(frame: &PcmFrame, target_rate_hz: u32) -> PcmFrame {
    if frame.sample_rate_hz == 0 || target_rate_hz == 0 || frame.samples.is_empty() {
        return PcmFrame {
            samples: Vec::new(),
            sample_rate_hz: target_rate_hz,
        };
    }

    if frame.sample_rate_hz == target_rate_hz {
        return frame.clone();
    }

    let ratio = target_rate_hz as f64 / frame.sample_rate_hz as f64;
    let out_len = ((frame.samples.len() as f64) * ratio).round() as usize;
    if out_len == 0 {
        return PcmFrame {
            samples: Vec::new(),
            sample_rate_hz: target_rate_hz,
        };
    }

    let mut out = Vec::with_capacity(out_len);
    let max_idx = frame.samples.len().saturating_sub(1);

    for i in 0..out_len {
        let src_pos = i as f64 / ratio;
        let idx = src_pos.floor() as usize;
        let frac = src_pos - idx as f64;
        let a = frame.samples[idx.min(max_idx)];
        let b = frame.samples[(idx + 1).min(max_idx)];
        out.push(a + (b - a) * frac as f32);
    }

    PcmFrame {
        samples: out,
        sample_rate_hz: target_rate_hz,
    }
}

/// RMS level in \[0.0, 1.0\] for UI metering.
pub fn rms_level(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
    let rms = (sum_sq / samples.len() as f32).sqrt();
    rms.clamp(0.0, 1.0)
}

/// Mix two mono frames to mono (resample to the higher rate, average aligned samples).
pub fn mix_frames(mic: &PcmFrame, system: &PcmFrame) -> PcmFrame {
    let rate = mic.sample_rate_hz.max(system.sample_rate_hz);
    let mic_resampled = if mic.sample_rate_hz == rate {
        mic.clone()
    } else {
        resample_linear(mic, rate)
    };
    let sys_resampled = if system.sample_rate_hz == rate {
        system.clone()
    } else {
        resample_linear(system, rate)
    };

    let len = mic_resampled
        .samples
        .len()
        .max(sys_resampled.samples.len());
    let mut samples = Vec::with_capacity(len);
    for i in 0..len {
        let m = mic_resampled.samples.get(i).copied().unwrap_or(0.0);
        let s = sys_resampled.samples.get(i).copied().unwrap_or(0.0);
        samples.push((m + s) / 2.0);
    }

    PcmFrame {
        samples,
        sample_rate_hz: rate,
    }
}

/// Split PCM into fixed-duration chunks for segmented STT (last chunk may be shorter).
pub fn chunk_pcm_by_duration(frame: &PcmFrame, chunk_secs: u32) -> Vec<PcmFrame> {
    if chunk_secs == 0 || frame.sample_rate_hz == 0 || frame.samples.is_empty() {
        return Vec::new();
    }

    let chunk_samples = (frame.sample_rate_hz as u64 * chunk_secs as u64) as usize;
    if chunk_samples == 0 {
        return Vec::new();
    }

    frame
        .samples
        .chunks(chunk_samples)
        .map(|chunk| PcmFrame {
            samples: chunk.to_vec(),
            sample_rate_hz: frame.sample_rate_hz,
        })
        .collect()
}

/// Concatenate frames assumed to share the same sample rate.
pub fn accumulate_frames(frames: &[PcmFrame]) -> PcmFrame {
    if frames.is_empty() {
        return PcmFrame {
            samples: Vec::new(),
            sample_rate_hz: 0,
        };
    }

    let sample_rate_hz = frames[0].sample_rate_hz;
    let total: usize = frames.iter().map(|f| f.samples.len()).sum();
    let mut samples = Vec::with_capacity(total);
    for frame in frames {
        samples.extend_from_slice(&frame.samples);
    }

    PcmFrame {
        samples,
        sample_rate_hz,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resample_halves_sample_count_when_halving_rate() {
        let frame = PcmFrame {
            samples: (0..100).map(|i| i as f32 / 100.0).collect(),
            sample_rate_hz: 48_000,
        };
        let out = resample_linear(&frame, 24_000);
        assert_eq!(out.sample_rate_hz, 24_000);
        assert_eq!(out.samples.len(), 50);
    }

    #[test]
    fn rms_silence_is_zero() {
        assert_eq!(rms_level(&[0.0, 0.0, 0.0]), 0.0);
    }

    #[test]
    fn rms_full_scale_is_one() {
        assert!((rms_level(&[1.0, -1.0, 1.0]) - 1.0).abs() < 0.01);
    }

    #[test]
    fn mix_frames_averages_aligned_samples() {
        let mic = PcmFrame {
            samples: vec![1.0, 0.0],
            sample_rate_hz: 16_000,
        };
        let sys = PcmFrame {
            samples: vec![0.0, 1.0],
            sample_rate_hz: 16_000,
        };
        let mixed = mix_frames(&mic, &sys);
        assert_eq!(mixed.samples.len(), 2);
        assert!((mixed.samples[0] - 0.5).abs() < 0.01);
        assert!((mixed.samples[1] - 0.5).abs() < 0.01);
    }

    #[test]
    fn chunk_90s_audio_into_three_30s_segments() {
        let frame = PcmFrame {
            samples: vec![0.0; 16_000 * 90],
            sample_rate_hz: 16_000,
        };
        let chunks = chunk_pcm_by_duration(&frame, 30);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].samples.len(), 16_000 * 30);
        assert_eq!(chunks[2].samples.len(), 16_000 * 30);
    }

    #[test]
    fn accumulate_frames_concatenates_samples() {
        let frames = vec![
            PcmFrame {
                samples: vec![0.1, 0.2],
                sample_rate_hz: 16_000,
            },
            PcmFrame {
                samples: vec![0.3],
                sample_rate_hz: 16_000,
            },
        ];
        let out = accumulate_frames(&frames);
        assert_eq!(out.samples, vec![0.1, 0.2, 0.3]);
        assert_eq!(out.sample_rate_hz, 16_000);
    }
}
