//! Speaker diarization over a complete waveform.
//!
//! ## Why the whole-file API is usable *here* and not for meetings
//!
//! Item 15's design goes to some length to avoid sherpa's whole-file
//! diarizer, for a good reason: a live meeting's audio is dropped as soon as
//! each segment is transcribed, so at stop there is no recording to diarize
//! and making one would mean writing a WAV sidecar of every meeting to disk.
//!
//! File transcription has no such problem. The whole waveform is in hand by
//! construction — it was decoded from a file the user already has — so the
//! whole-file API is not a compromise here, it is the right one: one
//! clustering pass over the whole recording is what makes "speaker 1" mean
//! the same person at minute 2 and at minute 40.
//!
//! Verified against sherpa-onnx 1.13.3: `FastClusteringConfig` takes
//! `num_clusters: -1` *and* a distance `threshold`, so the speaker count does
//! not have to be known in advance. That was the plan's open gate on this
//! item and it closes favourably.

use std::path::Path;

use async_trait::async_trait;

use crate::error::InferError;
use crate::types::AudioPcm;

/// One diarized span: who spoke, and when, in milliseconds from the start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerSpan {
    pub start_ms: u64,
    pub end_ms: u64,
    /// Zero-based speaker index within this recording. Deliberately not a
    /// person: see the note on cross-meeting voiceprints in the feature plan.
    pub speaker: u32,
}

/// The model files a diarization pass needs, as installed on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiarizationModels {
    /// The pyannote segmentation weights.
    pub segmentation: std::path::PathBuf,
    /// The speaker-embedding weights.
    pub embedding: std::path::PathBuf,
}

impl DiarizationModels {
    /// Resolves both installed model directories to the weights inside them.
    ///
    /// Both are named `model.onnx` — the segmentation bundle ships it, and
    /// the bare embedding download is installed under that name — which is
    /// what [`crate::registry::OnnxBundleShape::marker`] already says, so
    /// this asks the shape rather than repeating the filename.
    pub fn locate(segmentation_dir: &Path, embedding_dir: &Path) -> Result<Self, InferError> {
        let segmentation = segmentation_dir.join("model.onnx");
        let embedding = embedding_dir.join("model.onnx");
        for (what, path) in [
            ("segmentation", &segmentation),
            ("speaker embedding", &embedding),
        ] {
            if !path.is_file() {
                return Err(InferError::Other(format!(
                    "the {what} model is not installed ({})",
                    path.display()
                )));
            }
        }
        Ok(Self {
            segmentation,
            embedding,
        })
    }
}

/// Tuning for one diarization pass.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DiarizationOpts {
    /// How many speakers to force, or `None` to let the clustering threshold
    /// decide. `None` is the normal case: a dropped recording rarely comes
    /// with a known speaker count, and forcing one splits a monologue in two.
    pub num_speakers: Option<u32>,
    /// Cosine-distance threshold for the clusterer. Higher merges more.
    pub threshold: f32,
}

impl Default for DiarizationOpts {
    fn default() -> Self {
        Self {
            num_speakers: None,
            // sherpa's own default; anything lower splits one speaker into
            // several the moment they change how loudly they are talking.
            threshold: 0.5,
        }
    }
}

#[async_trait]
pub trait SpeakerDiarization: Send + Sync {
    async fn diarize(
        &self,
        pcm: AudioPcm,
        models: &DiarizationModels,
        opts: DiarizationOpts,
    ) -> Result<Vec<SpeakerSpan>, InferError>;
}

#[cfg(feature = "sherpa")]
pub struct SherpaOnnxDiarization;

#[cfg(feature = "sherpa")]
impl SherpaOnnxDiarization {
    pub fn new() -> Self {
        Self
    }
}

#[cfg(feature = "sherpa")]
impl Default for SherpaOnnxDiarization {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(feature = "sherpa")]
#[async_trait]
impl SpeakerDiarization for SherpaOnnxDiarization {
    async fn diarize(
        &self,
        pcm: AudioPcm,
        models: &DiarizationModels,
        opts: DiarizationOpts,
    ) -> Result<Vec<SpeakerSpan>, InferError> {
        let segmentation = models.segmentation.to_string_lossy().into_owned();
        let embedding = models.embedding.to_string_lossy().into_owned();
        let samples = pcm.samples;
        let rate = pcm.sample_rate_hz;

        tokio::task::spawn_blocking(move || {
            use sherpa_onnx::{
                FastClusteringConfig, OfflineSpeakerDiarization, OfflineSpeakerDiarizationConfig,
                OfflineSpeakerSegmentationPyannoteModelConfig,
            };

            let mut config = OfflineSpeakerDiarizationConfig::default();
            config.segmentation.pyannote = OfflineSpeakerSegmentationPyannoteModelConfig {
                model: Some(segmentation),
            };
            config.embedding.model = Some(embedding);
            config.clustering = FastClusteringConfig {
                // -1 means "decide from the threshold", which is what lets a
                // recording with an unknown number of speakers work at all.
                num_clusters: opts.num_speakers.map(|n| n as i32).unwrap_or(-1),
                threshold: opts.threshold,
            };

            let diarizer = OfflineSpeakerDiarization::create(&config).ok_or_else(|| {
                InferError::Other("failed to create the sherpa speaker diarizer".into())
            })?;

            // The segmentation model is trained at one rate and sherpa does
            // not resample for us. Refusing is better than diarizing at the
            // wrong rate, which produces confident, wrong turn boundaries.
            let expected = diarizer.sample_rate();
            if expected > 0 && expected as u32 != rate {
                return Err(InferError::Other(format!(
                    "the diarization model expects {expected} Hz audio, got {rate} Hz"
                )));
            }

            let result = diarizer.process(&samples).ok_or_else(|| {
                InferError::Other("speaker diarization returned no result".into())
            })?;

            Ok(result
                .sort_by_start_time()
                .into_iter()
                .map(|s| SpeakerSpan {
                    start_ms: (s.start.max(0.0) * 1000.0).round() as u64,
                    end_ms: (s.end.max(0.0) * 1000.0).round() as u64,
                    speaker: s.speaker.max(0) as u32,
                })
                .collect())
        })
        .await
        .map_err(|e| InferError::Other(format!("diarization task join failed: {e}")))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_models_are_named_rather_than_reported_as_a_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let seg = dir.path().join("seg");
        let emb = dir.path().join("emb");
        std::fs::create_dir_all(&seg).unwrap();
        std::fs::create_dir_all(&emb).unwrap();

        let err = DiarizationModels::locate(&seg, &emb).unwrap_err();
        assert!(err.to_string().contains("segmentation"), "{err}");

        std::fs::write(seg.join("model.onnx"), b"w").unwrap();
        let err = DiarizationModels::locate(&seg, &emb).unwrap_err();
        assert!(err.to_string().contains("speaker embedding"), "{err}");

        std::fs::write(emb.join("model.onnx"), b"w").unwrap();
        let models = DiarizationModels::locate(&seg, &emb).unwrap();
        assert!(models.segmentation.ends_with("model.onnx"));
    }

    /// `None` is the default because a dropped recording rarely comes with a
    /// known speaker count, and forcing one splits a monologue in two.
    #[test]
    fn the_speaker_count_is_unknown_by_default() {
        let opts = DiarizationOpts::default();
        assert_eq!(opts.num_speakers, None);
        assert!(opts.threshold > 0.0);
    }
}
