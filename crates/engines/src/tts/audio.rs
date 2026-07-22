use crate::traits::{AudioPcm, EngineError};

/// Decodes mono 16-bit PCM WAV bytes into f32 samples.
pub fn bytes_to_pcm_wav(bytes: &[u8]) -> Result<AudioPcm, EngineError> {
    if bytes.len() < 44 {
        return Err(EngineError::Config("wav too short".into()));
    }
    if &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(EngineError::Config("not a wav file".into()));
    }

    let mut offset = 12usize;
    let mut sample_rate_hz = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut pcm_data: Option<&[u8]> = None;

    while offset + 8 <= bytes.len() {
        let chunk_id = &bytes[offset..offset + 4];
        let chunk_size = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap()) as usize;
        let chunk_start = offset + 8;
        let chunk_end = chunk_start.saturating_add(chunk_size);
        if chunk_end > bytes.len() {
            return Err(EngineError::Config("invalid wav chunk".into()));
        }

        if chunk_id == b"fmt " {
            if chunk_size < 16 {
                return Err(EngineError::Config("invalid fmt chunk".into()));
            }
            channels = u16::from_le_bytes(bytes[chunk_start + 2..chunk_start + 4].try_into().unwrap());
            sample_rate_hz =
                u32::from_le_bytes(bytes[chunk_start + 4..chunk_start + 8].try_into().unwrap());
            bits_per_sample =
                u16::from_le_bytes(bytes[chunk_start + 14..chunk_start + 16].try_into().unwrap());
        } else if chunk_id == b"data" {
            pcm_data = Some(&bytes[chunk_start..chunk_end]);
        }

        offset = chunk_end + (chunk_size % 2);
    }

    let pcm_data = pcm_data.ok_or_else(|| EngineError::Config("missing data chunk".into()))?;
    if channels != 1 {
        return Err(EngineError::Config("only mono wav supported".into()));
    }
    if bits_per_sample != 16 {
        return Err(EngineError::Config("only 16-bit wav supported".into()));
    }
    if sample_rate_hz == 0 {
        return Err(EngineError::Config("missing sample rate".into()));
    }

    if pcm_data.len() % 2 != 0 {
        return Err(EngineError::Config("wav data chunk has odd length, expected 16-bit aligned".into()));
    }

    let samples: Vec<f32> = pcm_data
        .chunks_exact(2)
        .map(|chunk| {
            let sample = i16::from_le_bytes([chunk[0], chunk[1]]);
            sample as f32 / 32768.0
        })
        .collect();

    Ok(AudioPcm {
        samples,
        sample_rate_hz,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stt::audio::pcm_to_wav_bytes;
    use crate::traits::AudioPcm;

    #[test]
    fn roundtrips_mono_wav() {
        let original = AudioPcm {
            samples: vec![0.0, 0.5, -0.5],
            sample_rate_hz: 24_000,
        };
        let wav = pcm_to_wav_bytes(&original).unwrap();
        let decoded = bytes_to_pcm_wav(&wav).unwrap();
        assert_eq!(decoded.sample_rate_hz, 24_000);
        assert_eq!(decoded.samples.len(), 3);
    }

    #[test]
    fn rejects_non_wav() {
        let err = bytes_to_pcm_wav(b"not wav").unwrap_err();
        assert!(err.to_string().contains("wav"));
    }
}
