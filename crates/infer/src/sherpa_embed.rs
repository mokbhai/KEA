//! Speaker embeddings for short windows of live meeting audio.
//!
//! ## Why windows and not `sherpa_diarize`
//!
//! A meeting never has its whole waveform: each segment is transcribed and
//! dropped. So the whole-file diarizer in [`crate::sherpa_diarize`] cannot
//! run, and its cluster indices would not mean the same person across
//! segments anyway. Embedding fixed-length windows gives a vector that can be
//! compared across the whole meeting, which is what the clustering in
//! `kea-core` needs.
//!
//! Each window gets its own extractor stream. Reusing one stream would
//! concatenate windows into a single embedding, which is the opposite of what
//! the caller asked for.

use std::path::{Path, PathBuf};

use async_trait::async_trait;

use crate::error::InferError;

/// The sample rate of the CAM++ model installed as
/// [`crate::registry::DIARIZATION_EMBEDDING_ID`]. The sherpa extractor does
/// not report its own rate, and it does not resample, so the rate is pinned
/// to the model's training rate here.
pub const SPEAKER_EMBEDDING_SAMPLE_RATE_HZ: u32 = 16_000;

/// Turns a window of mono PCM into a fixed-length speaker embedding.
///
/// Errors are per call. A caller embedding live audio should treat a failure
/// as "no evidence for this window" rather than as a reason to stop
/// recording.
#[async_trait]
pub trait SpeakerEmbedder: Send + Sync {
    async fn embed(&self, samples: Vec<f32>, sample_rate_hz: u32) -> Result<Vec<f32>, InferError>;

    /// Embeds many windows in one call, in order.
    ///
    /// The default loops over [`SpeakerEmbedder::embed`]. Implementations
    /// that pay a fixed cost per dispatch, such as a blocking-thread hop,
    /// should override it so a stop-time pass does not pay that cost per
    /// window.
    async fn embed_batch(
        &self,
        windows: Vec<Vec<f32>>,
        sample_rate_hz: u32,
    ) -> Result<Vec<Vec<f32>>, InferError> {
        let mut out = Vec::with_capacity(windows.len());
        for window in windows {
            out.push(self.embed(window, sample_rate_hz).await?);
        }
        Ok(out)
    }
}

/// Finds the embedding weights under an installed model directory.
///
/// Takes the directory rather than the id so the caller decides where models
/// live, as with [`crate::sherpa_diarize::DiarizationModels::locate`]. The
/// error names the missing model, so a user who skipped the download sees
/// "not installed" rather than a load failure.
pub fn locate_embedding_model(embedding_dir: &Path) -> Result<PathBuf, InferError> {
    let model = embedding_dir.join("model.onnx");
    if !model.is_file() {
        return Err(InferError::Other(format!(
            "the speaker embedding model is not installed ({})",
            model.display()
        )));
    }
    Ok(model)
}

#[cfg(feature = "sherpa")]
pub use sherpa::SherpaOnnxSpeakerEmbedder;

#[cfg(feature = "sherpa")]
mod sherpa {
    use std::path::Path;
    use std::sync::Arc;

    use async_trait::async_trait;
    use sherpa_onnx::{OnlineStream, SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};

    use super::{SpeakerEmbedder, SPEAKER_EMBEDDING_SAMPLE_RATE_HZ};
    use crate::error::InferError;

    /// A sherpa-onnx speaker embedder over one loaded extractor.
    ///
    /// Cheap to clone: clones share the loaded model. Load it once per
    /// meeting, not once per window.
    #[derive(Clone)]
    pub struct SherpaOnnxSpeakerEmbedder {
        extractor: Arc<SpeakerEmbeddingExtractor>,
    }

    impl SherpaOnnxSpeakerEmbedder {
        /// Loads the model at `model_path` (see
        /// [`super::locate_embedding_model`]).
        ///
        /// Loading parses the whole ONNX graph, so it runs on a blocking
        /// thread rather than on the async runtime.
        pub async fn load(model_path: &Path) -> Result<Self, InferError> {
            let model = model_path.to_string_lossy().into_owned();
            tokio::task::spawn_blocking(move || {
                let config = SpeakerEmbeddingExtractorConfig {
                    model: Some(model.clone()),
                    ..Default::default()
                };
                let extractor = SpeakerEmbeddingExtractor::create(&config).ok_or_else(|| {
                    InferError::Other(format!(
                        "failed to load the speaker embedding model from {model}"
                    ))
                })?;
                Ok(Self {
                    extractor: Arc::new(extractor),
                })
            })
            .await
            .map_err(|e| InferError::Other(format!("embedder load task join failed: {e}")))?
        }

        /// The length of every embedding this model returns.
        pub fn dim(&self) -> usize {
            self.extractor.dim() as usize
        }
    }

    /// Embeds one window on the calling thread. The extractor is shared, but
    /// each window gets a fresh stream so nothing carries over between them.
    fn embed_window(
        extractor: &SpeakerEmbeddingExtractor,
        samples: &[f32],
        sample_rate_hz: u32,
    ) -> Result<Vec<f32>, InferError> {
        let stream: OnlineStream = extractor.create_stream().ok_or_else(|| {
            InferError::Other("failed to create a speaker embedding stream".into())
        })?;
        stream.accept_waveform(sample_rate_hz as i32, samples);
        // Telling sherpa the input is complete lets it treat a short window as
        // final instead of waiting for more audio that will never come.
        stream.input_finished();
        if !extractor.is_ready(&stream) {
            return Err(InferError::Other(
                "window is too short to produce a speaker embedding".into(),
            ));
        }
        extractor
            .compute(&stream)
            .ok_or_else(|| InferError::Other("speaker embedding returned no result".into()))
    }

    /// Refuses audio at the wrong rate before any work is queued. The model
    /// was trained at one rate and sherpa does not resample, so embedding at
    /// another rate yields confident vectors that match nothing.
    fn check_rate(sample_rate_hz: u32) -> Result<(), InferError> {
        if sample_rate_hz != SPEAKER_EMBEDDING_SAMPLE_RATE_HZ {
            return Err(InferError::Other(format!(
                "the speaker embedding model expects {SPEAKER_EMBEDDING_SAMPLE_RATE_HZ} Hz audio, got {sample_rate_hz} Hz"
            )));
        }
        Ok(())
    }

    #[async_trait]
    impl SpeakerEmbedder for SherpaOnnxSpeakerEmbedder {
        async fn embed(
            &self,
            samples: Vec<f32>,
            sample_rate_hz: u32,
        ) -> Result<Vec<f32>, InferError> {
            check_rate(sample_rate_hz)?;
            let extractor = Arc::clone(&self.extractor);
            tokio::task::spawn_blocking(move || embed_window(&extractor, &samples, sample_rate_hz))
                .await
                .map_err(|e| InferError::Other(format!("embedding task join failed: {e}")))?
        }

        async fn embed_batch(
            &self,
            windows: Vec<Vec<f32>>,
            sample_rate_hz: u32,
        ) -> Result<Vec<Vec<f32>>, InferError> {
            check_rate(sample_rate_hz)?;
            let extractor = Arc::clone(&self.extractor);
            // One hop to the blocking pool for the whole batch, not one per
            // window: the stop-time pass embeds thousands of windows at once.
            tokio::task::spawn_blocking(move || {
                windows
                    .iter()
                    .map(|w| embed_window(&extractor, w, sample_rate_hz))
                    .collect()
            })
            .await
            .map_err(|e| InferError::Other(format!("embedding task join failed: {e}")))?
        }
    }
}

/// A test double that answers [`SpeakerEmbedder`] calls from a script.
///
/// Each call takes the next step in order. Once the script runs out, every
/// further call fails, so a test that embeds more windows than it scripted
/// fails loudly rather than silently reusing a vector.
///
/// Public behind `test-util` so downstream crates can drive their own tests
/// with it; it is not part of the production surface.
#[cfg(any(test, feature = "test-util"))]
pub struct ScriptedEmbedder {
    steps: std::sync::Mutex<std::collections::VecDeque<Result<Vec<f32>, String>>>,
    calls: std::sync::atomic::AtomicUsize,
}

#[cfg(any(test, feature = "test-util"))]
impl ScriptedEmbedder {
    /// `steps` is consumed front to back: `Ok` returns that vector, `Err`
    /// fails with that message.
    pub fn new(steps: Vec<Result<Vec<f32>, String>>) -> Self {
        Self {
            steps: std::sync::Mutex::new(steps.into()),
            calls: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    /// How many `embed` calls have been made, scripted or not.
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }

    fn next(&self) -> Result<Vec<f32>, InferError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let step = self
            .steps
            .lock()
            .expect("scripted embedder lock poisoned")
            .pop_front();
        match step {
            Some(Ok(vector)) => Ok(vector),
            Some(Err(message)) => Err(InferError::Other(message)),
            None => Err(InferError::Other(
                "scripted embedder has no steps left".into(),
            )),
        }
    }
}

#[cfg(any(test, feature = "test-util"))]
#[async_trait]
impl SpeakerEmbedder for ScriptedEmbedder {
    async fn embed(
        &self,
        _samples: Vec<f32>,
        _sample_rate_hz: u32,
    ) -> Result<Vec<f32>, InferError> {
        self.next()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_embedding_model_is_named_as_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let err = locate_embedding_model(dir.path()).unwrap_err();
        assert!(
            err.to_string()
                .contains("speaker embedding model is not installed"),
            "{err}"
        );

        std::fs::write(dir.path().join("model.onnx"), b"w").unwrap();
        let path = locate_embedding_model(dir.path()).unwrap();
        assert!(path.ends_with("model.onnx"));
    }

    #[tokio::test]
    async fn scripted_embedder_answers_in_order_then_fails_loudly() {
        let fake = ScriptedEmbedder::new(vec![Ok(vec![1.0, 0.0]), Err("boom".into())]);
        assert_eq!(fake.embed(vec![0.0], 16_000).await.unwrap(), vec![1.0, 0.0]);
        assert!(fake
            .embed(vec![0.0], 16_000)
            .await
            .unwrap_err()
            .to_string()
            .contains("boom"));
        assert!(fake.embed(vec![0.0], 16_000).await.is_err());
        assert_eq!(fake.calls(), 3);
    }

    #[tokio::test]
    async fn default_batch_embeds_each_window_in_order() {
        let fake = ScriptedEmbedder::new(vec![Ok(vec![1.0]), Ok(vec![2.0])]);
        let out = fake
            .embed_batch(vec![vec![0.0], vec![0.0]], 16_000)
            .await
            .unwrap();
        assert_eq!(out, vec![vec![1.0], vec![2.0]]);
    }
}
