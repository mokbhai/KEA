use std::path::Path;
#[cfg(feature = "sherpa")]
use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::InferError;
#[cfg(feature = "sherpa")]
use crate::types::{group_tokens_into_segments, TOKEN_GROUP_GAP_MS, TOKEN_GROUP_MAX_CUE_MS};
use crate::types::{AudioPcm, SttResult};

#[async_trait]
pub trait SherpaSttInference: Send + Sync {
    /// Transcribes mono PCM with the ONNX bundle in `model_dir`.
    ///
    /// There is deliberately no language parameter. The NeMo transducer this
    /// drives has no language setting — sherpa's `OfflineTransducerModelConfig`
    /// carries only the encoder/decoder/joiner paths — so a language argument
    /// could only ever be accepted and dropped, which is what this signature
    /// used to do. A parameter that looks honoured all the way down from the
    /// STT setting is worse than one that was never plumbed.
    async fn transcribe(&self, pcm: AudioPcm, model_dir: &Path) -> Result<SttResult, InferError>;
}

#[cfg(feature = "sherpa")]
pub struct SherpaOnnxSttInference;

#[cfg(feature = "sherpa")]
impl SherpaOnnxSttInference {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "sherpa")]
impl Default for SherpaOnnxSttInference {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "sherpa")]
fn find_parakeet_model_files(
    model_dir: &Path,
) -> Result<(PathBuf, PathBuf, PathBuf, PathBuf), InferError> {
    let tokens = model_dir.join("tokens.txt");
    if !tokens.is_file() {
        return Err(InferError::Other(format!(
            "missing tokens.txt in {}",
            model_dir.display()
        )));
    }

    let encoder = find_first_existing(model_dir, &["encoder.int8.onnx", "encoder.onnx"])?;
    let decoder = find_first_existing(model_dir, &["decoder.int8.onnx", "decoder.onnx"])?;
    let joiner = find_first_existing(model_dir, &["joiner.int8.onnx", "joiner.onnx"])?;
    Ok((encoder, decoder, joiner, tokens))
}

#[cfg(feature = "sherpa")]
fn find_first_existing(dir: &Path, names: &[&str]) -> Result<PathBuf, InferError> {
    for name in names {
        let path = dir.join(name);
        if path.is_file() {
            return Ok(path);
        }
    }
    Err(InferError::Other(format!(
        "none of {names:?} found in {}",
        dir.display()
    )))
}

#[cfg(feature = "sherpa")]
#[async_trait]
impl SherpaSttInference for SherpaOnnxSttInference {
    async fn transcribe(&self, pcm: AudioPcm, model_dir: &Path) -> Result<SttResult, InferError> {
        let model_dir = model_dir.to_path_buf();
        let samples = pcm.samples;
        let sample_rate = pcm.sample_rate_hz;

        tokio::task::spawn_blocking(move || {
            use sherpa_onnx::{
                OfflineRecognizer, OfflineRecognizerConfig, OfflineTransducerModelConfig,
            };

            let (encoder, decoder, joiner, tokens) = find_parakeet_model_files(&model_dir)?;

            let mut config = OfflineRecognizerConfig::default();
            config.model_config.transducer = OfflineTransducerModelConfig {
                encoder: Some(encoder.to_string_lossy().into_owned()),
                decoder: Some(decoder.to_string_lossy().into_owned()),
                joiner: Some(joiner.to_string_lossy().into_owned()),
            };
            config.model_config.tokens = Some(tokens.to_string_lossy().into_owned());
            config.model_config.model_type = Some("nemo_transducer".into());
            config.model_config.num_threads = std::thread::available_parallelism()
                .map(|n| n.get() as i32)
                .unwrap_or(1);

            let recognizer = OfflineRecognizer::create(&config).ok_or_else(|| {
                InferError::Other("failed to create sherpa OfflineRecognizer".into())
            })?;

            let stream = recognizer.create_stream();
            stream.accept_waveform(sample_rate as i32, &samples);
            recognizer.decode(&stream);

            let result = stream
                .get_result()
                .ok_or_else(|| InferError::Other("sherpa parakeet returned no result".into()))?;

            // sherpa reports one timestamp per *token*, so these have to be
            // grouped into cues before they are readable as subtitles — one
            // cue per word is not a subtitle. A model or build that reports
            // none leaves `segments` empty rather than inventing a span.
            let segments = match result.timestamps.as_deref() {
                Some(starts) => group_tokens_into_segments(
                    &result.tokens,
                    starts,
                    result.durations.as_deref(),
                    TOKEN_GROUP_GAP_MS,
                    TOKEN_GROUP_MAX_CUE_MS,
                ),
                None => Vec::new(),
            };

            Ok(SttResult {
                text: result.text,
                segments,
            })
        })
        .await
        .map_err(|e| InferError::Other(format!("sherpa stt task join failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub struct FakeSherpaSttInference;

    #[async_trait]
    impl SherpaSttInference for FakeSherpaSttInference {
        async fn transcribe(
            &self,
            pcm: AudioPcm,
            _model_dir: &Path,
        ) -> Result<SttResult, InferError> {
            Ok(SttResult::text_only(format!(
                "parakeet: {} samples",
                pcm.samples.len()
            )))
        }
    }

    #[test]
    fn fake_sherpa_stt_trait_is_usable() {
        let _ = std::any::type_name::<FakeSherpaSttInference>();
    }

    #[tokio::test]
    async fn fake_inference_returns_sample_count() {
        let inference = FakeSherpaSttInference;
        let out = inference
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                Path::new("/tmp/parakeet-model"),
            )
            .await
            .unwrap();
        assert!(out.text.contains("parakeet"));
        assert!(out.text.contains("1600"));
    }
}
