use crate::traits::{AudioPcm, EngineError};

/// The sample rate every local STT model in the catalogue is exported at.
pub const STT_SAMPLE_RATE_HZ: u32 = 16_000;

/// Encodes mono f32 PCM samples as a 16-bit PCM WAV container.
pub fn pcm_to_wav_bytes(pcm: &AudioPcm) -> Result<Vec<u8>, EngineError> {
    let pcm16: Vec<u8> = pcm
        .samples
        .iter()
        .flat_map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            let sample = if clamped < 0.0 {
                (clamped * 32768.0) as i16
            } else {
                (clamped * 32767.0) as i16
            };
            sample.to_le_bytes()
        })
        .collect();

    let data_size = pcm16.len() as u32;
    let channels: u16 = 1;
    let bits_per_sample: u16 = 16;
    let byte_rate = pcm.sample_rate_hz * channels as u32 * bits_per_sample as u32 / 8;
    let block_align = channels * bits_per_sample / 8;
    let riff_size = 36 + data_size;

    let mut wav = Vec::with_capacity(44 + pcm16.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&riff_size.to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&pcm.sample_rate_hz.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&block_align.to_le_bytes());
    wav.extend_from_slice(&bits_per_sample.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_size.to_le_bytes());
    wav.extend_from_slice(&pcm16);
    Ok(wav)
}

/// Resamples mono f32 PCM by naive linear interpolation.
///
/// There is no anti-alias filter, so downsampling aliases; it lives here
/// rather than in each engine so a quality fix lands once for every local
/// STT path.
pub fn resample_to_rate(samples: &[f32], src_rate_hz: u32, dst_rate_hz: u32) -> Vec<f32> {
    if src_rate_hz == dst_rate_hz || samples.is_empty() {
        return samples.to_vec();
    }

    let ratio = src_rate_hz as f64 / dst_rate_hz as f64;
    let out_len = ((samples.len() as f64) / ratio).ceil() as usize;
    let mut out = Vec::with_capacity(out_len);

    for i in 0..out_len {
        let src_pos = i as f64 * ratio;
        let idx = src_pos as usize;
        let frac = (src_pos - idx as f64) as f32;
        let s0 = samples.get(idx).copied().unwrap_or(0.0);
        let s1 = samples.get(idx + 1).copied().unwrap_or(s0);
        out.push(s0 + (s1 - s0) * frac);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::traits::AudioPcm;

    #[test]
    fn wav_has_riff_header_and_correct_data_size() {
        let pcm = AudioPcm {
            samples: vec![0.0, 1.0, -1.0],
            sample_rate_hz: 16_000,
        };
        let wav = pcm_to_wav_bytes(&pcm).unwrap();
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        let data_chunk_size = u32::from_le_bytes(wav[40..44].try_into().unwrap());
        assert_eq!(data_chunk_size, 6);
    }

    #[test]
    fn resample_halves_sample_count_when_halving_rate() {
        let samples: Vec<f32> = (0..100).map(|i| i as f32 / 100.0).collect();
        let out = resample_to_rate(&samples, 48_000, 24_000);
        assert_eq!(out.len(), 50);
    }

    #[test]
    fn resample_is_a_no_op_at_the_same_rate() {
        let samples: Vec<f32> = vec![0.1, -0.2, 0.3];
        let out = resample_to_rate(&samples, STT_SAMPLE_RATE_HZ, STT_SAMPLE_RATE_HZ);
        assert_eq!(out, samples);
    }
}
