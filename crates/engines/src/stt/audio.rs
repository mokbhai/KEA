use crate::traits::{AudioPcm, EngineError};

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
}
