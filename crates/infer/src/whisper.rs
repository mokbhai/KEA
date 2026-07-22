use std::path::Path;

use async_trait::async_trait;

use crate::error::InferError;

#[derive(Debug, Clone, PartialEq)]
pub struct AudioPcm {
    pub samples: Vec<f32>,
    pub sample_rate_hz: u32,
}

#[derive(Debug, Clone, Default)]
pub struct WhisperOpts {
    pub language: Option<String>,
}

#[async_trait]
pub trait WhisperInference: Send + Sync {
    async fn transcribe(
        &self,
        pcm: AudioPcm,
        model_path: &Path,
        opts: WhisperOpts,
    ) -> Result<String, InferError>;
}

#[cfg(feature = "whisper")]
pub struct WhisperRsInference;

#[cfg(feature = "whisper")]
impl WhisperRsInference {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "whisper")]
impl Default for WhisperRsInference {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "whisper")]
#[async_trait]
impl WhisperInference for WhisperRsInference {
    async fn transcribe(
        &self,
        pcm: AudioPcm,
        model_path: &Path,
        opts: WhisperOpts,
    ) -> Result<String, InferError> {
        let model_path = model_path.to_path_buf();
        let samples = pcm.samples;
        let language = opts.language;

        tokio::task::spawn_blocking(move || {
            use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

            let ctx = WhisperContext::new_with_params(
                model_path.to_string_lossy().as_ref(),
                WhisperContextParameters::default(),
            )
            .map_err(|e| InferError::Other(format!("failed to load whisper model: {e}")))?;

            let mut state = ctx
                .create_state()
                .map_err(|e| InferError::Other(format!("failed to create whisper state: {e}")))?;

            let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            params.set_n_threads(
                std::thread::available_parallelism()
                    .map(|n| n.get())
                    .unwrap_or(1) as i32,
            );
            params.set_translate(false);
            params.set_print_special(false);
            params.set_print_progress(false);
            params.set_print_realtime(false);
            params.set_print_timestamps(false);

            if let Some(ref lang) = language {
                params.set_language(Some(lang.as_str()));
            }

            state
                .full(params, &samples)
                .map_err(|e| InferError::Other(format!("whisper inference failed: {e}")))?;

            let num_segments = state
                .full_n_segments()
                .map_err(|e| InferError::Other(format!("failed to read segments: {e}")))?;

            let mut text = String::new();
            for i in 0..num_segments {
                let segment = state
                    .full_get_segment_text(i)
                    .map_err(|e| InferError::Other(format!("failed to read segment: {e}")))?;
                if !text.is_empty() && !segment.is_empty() {
                    text.push(' ');
                }
                text.push_str(&segment);
            }

            Ok(text)
        })
        .await
        .map_err(|e| InferError::Other(format!("whisper task join failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeWhisperInference;

    #[async_trait]
    impl WhisperInference for FakeWhisperInference {
        async fn transcribe(
            &self,
            pcm: AudioPcm,
            _model_path: &Path,
            _opts: WhisperOpts,
        ) -> Result<String, InferError> {
            Ok(format!("heard {} samples", pcm.samples.len()))
        }
    }

    #[tokio::test]
    async fn fake_inference_returns_sample_count() {
        let inference = FakeWhisperInference;
        let out = inference
            .transcribe(
                AudioPcm {
                    samples: vec![0.0; 100],
                    sample_rate_hz: 16_000,
                },
                Path::new("/tmp/model.gguf"),
                WhisperOpts::default(),
            )
            .await
            .unwrap();
        assert_eq!(out, "heard 100 samples");
    }
}
