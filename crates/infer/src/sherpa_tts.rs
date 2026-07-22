use std::path::Path;
#[cfg(feature = "sherpa")]
use std::path::PathBuf;

use async_trait::async_trait;

use crate::error::InferError;
use crate::whisper::AudioPcm;

#[async_trait]
pub trait SherpaTtsInference: Send + Sync {
    async fn synthesize(&self, text: &str, model_dir: &Path) -> Result<AudioPcm, InferError>;
}

#[cfg(feature = "sherpa")]
pub struct SherpaOnnxTtsInference;

#[cfg(feature = "sherpa")]
impl SherpaOnnxTtsInference {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "sherpa")]
impl Default for SherpaOnnxTtsInference {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "sherpa")]
fn find_vits_model_files(model_dir: &Path) -> Result<(PathBuf, PathBuf, PathBuf), InferError> {
    let tokens = model_dir.join("tokens.txt");
    if !tokens.is_file() {
        return Err(InferError::Other(format!(
            "missing tokens.txt in {}",
            model_dir.display()
        )));
    }

    let data_dir = model_dir.join("espeak-ng-data");
    if !data_dir.is_dir() {
        return Err(InferError::Other(format!(
            "missing espeak-ng-data in {}",
            model_dir.display()
        )));
    }

    let model = std::fs::read_dir(model_dir)
        .map_err(InferError::Io)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .find(|path| {
            path.is_file()
                && path.extension().is_some_and(|ext| ext == "onnx")
                && path
                    .file_name()
                    .is_some_and(|name| !name.to_string_lossy().starts_with("encoder"))
        })
        .ok_or_else(|| {
            InferError::Other(format!(
                "no VITS .onnx model found in {}",
                model_dir.display()
            ))
        })?;

    Ok((model, tokens, data_dir))
}

#[cfg(feature = "sherpa")]
#[async_trait]
impl SherpaTtsInference for SherpaOnnxTtsInference {
    async fn synthesize(&self, text: &str, model_dir: &Path) -> Result<AudioPcm, InferError> {
        let model_dir = model_dir.to_path_buf();
        let text = text.to_string();

        tokio::task::spawn_blocking(move || {
            use sherpa_onnx::{
                GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsModelConfig,
                OfflineTtsVitsModelConfig,
            };

            let (model, tokens, data_dir) = find_vits_model_files(&model_dir)?;

            let config = OfflineTtsConfig {
                model: OfflineTtsModelConfig {
                    vits: OfflineTtsVitsModelConfig {
                        model: Some(model.to_string_lossy().into_owned()),
                        tokens: Some(tokens.to_string_lossy().into_owned()),
                        data_dir: Some(data_dir.to_string_lossy().into_owned()),
                        ..Default::default()
                    },
                    num_threads: std::thread::available_parallelism()
                        .map(|n| n.get() as i32)
                        .unwrap_or(1),
                    ..Default::default()
                },
                ..Default::default()
            };

            let tts = OfflineTts::create(&config)
                .ok_or_else(|| InferError::Other("failed to create sherpa OfflineTts".into()))?;

            let audio = tts
                .generate_with_config(&text, &GenerationConfig::default(), None::<fn(&[f32], f32) -> bool>)
                .ok_or_else(|| InferError::Other("sherpa TTS returned no audio".into()))?;

            Ok(AudioPcm {
                samples: audio.samples().to_vec(),
                sample_rate_hz: audio.sample_rate() as u32,
            })
        })
        .await
        .map_err(|e| InferError::Other(format!("sherpa tts task join failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub struct FakeSherpaTtsInference;

    #[async_trait]
    impl SherpaTtsInference for FakeSherpaTtsInference {
        async fn synthesize(&self, text: &str, _model_dir: &Path) -> Result<AudioPcm, InferError> {
            Ok(AudioPcm {
                samples: vec![0.0; text.len() * 100],
                sample_rate_hz: 22_050,
            })
        }
    }

    #[tokio::test]
    async fn fake_tts_returns_pcm() {
        let inference = FakeSherpaTtsInference;
        let pcm = inference
            .synthesize("hello", Path::new("/tmp/tts-model"))
            .await
            .unwrap();
        assert_eq!(pcm.sample_rate_hz, 22_050);
        assert_eq!(pcm.samples.len(), 500);
    }
}
