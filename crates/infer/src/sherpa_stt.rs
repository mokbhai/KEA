use std::path::Path;
#[cfg(feature = "sherpa")]
use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::InferError;
use crate::whisper::{AudioPcm, WhisperOpts};

#[async_trait]
pub trait SherpaSttInference: Send + Sync {
    async fn transcribe_parakeet(
        &self,
        pcm: AudioPcm,
        model_dir: &Path,
        opts: WhisperOpts,
    ) -> Result<String, InferError>;
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
    async fn transcribe_parakeet(
        &self,
        pcm: AudioPcm,
        model_dir: &Path,
        opts: WhisperOpts,
    ) -> Result<String, InferError> {
        let model_dir = model_dir.to_path_buf();
        let samples = pcm.samples;
        let sample_rate = pcm.sample_rate_hz;
        let _language = opts.language;

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

            Ok(result.text)
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
        async fn transcribe_parakeet(
            &self,
            pcm: AudioPcm,
            _model_dir: &Path,
            _opts: WhisperOpts,
        ) -> Result<String, InferError> {
            Ok(format!("parakeet: {} samples", pcm.samples.len()))
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
            .transcribe_parakeet(
                AudioPcm {
                    samples: vec![0.0; 1600],
                    sample_rate_hz: 16_000,
                },
                Path::new("/tmp/parakeet-model"),
                WhisperOpts::default(),
            )
            .await
            .unwrap();
        assert!(out.contains("parakeet"));
        assert!(out.contains("1600"));
    }
}
