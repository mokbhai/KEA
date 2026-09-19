use std::path::Path;

use async_trait::async_trait;

use crate::error::InferError;
pub use crate::types::{AudioPcm, WhisperOpts};

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

/// Which whisper.cpp backend this binary was compiled with.
///
/// `metal` and `coreml` are separate axes, not alternatives: `metal` moves the
/// whole graph onto the GPU (and is what turns on whisper-rs's internal `_gpu`
/// feature, hence `use_gpu`), while `coreml` only swaps in a CoreML encoder and
/// leaves `use_gpu` alone.
#[cfg(feature = "whisper")]
fn compiled_backend() -> &'static str {
    match (
        cfg!(feature = "whisper-metal"),
        cfg!(feature = "whisper-coreml"),
    ) {
        (true, true) => "metal+coreml",
        (true, false) => "metal",
        (false, true) => "coreml",
        (false, false) => "cpu",
    }
}

/// Logged once per process, because a Metal build and a CPU build are otherwise
/// indistinguishable at runtime until someone compares decode times. This is the
/// line the Logs page shows to answer "did the Metal build actually take?".
#[cfg(feature = "whisper")]
fn log_backend_once() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        tracing::info!(
            backend = compiled_backend(),
            "whisper: compiled inference backend is {}",
            compiled_backend()
        );
    });
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
        let pcm_rate_hz = pcm.sample_rate_hz;
        let samples = pcm.samples;
        let language = opts.language;

        tokio::task::spawn_blocking(move || {
            use whisper_rs::{
                FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters,
            };

            log_backend_once();

            // `WhisperContextParameters::default()` sets `use_gpu` from
            // whisper-rs's own internal `_gpu` feature, which `metal` turns on.
            // So the default is already the right answer and must NOT be
            // overridden here: forcing `use_gpu: true` in a build with no GPU
            // backend compiled in asks for a backend that is not there.
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

            // Timed rather than asserted: wall-clock assertions are flaky in CI,
            // but the ratio is the only honest way to see a backend change.
            let audio_ms = if pcm_rate_hz > 0 {
                (samples.len() as u64 * 1_000) / pcm_rate_hz as u64
            } else {
                0
            };
            let started = std::time::Instant::now();

            state
                .full(params, &samples)
                .map_err(|e| InferError::Other(format!("whisper inference failed: {e}")))?;

            let decode_ms = started.elapsed().as_millis() as u64;
            tracing::debug!(
                backend = compiled_backend(),
                decode_ms,
                audio_ms,
                "whisper: decoded {}ms of audio in {}ms",
                audio_ms,
                decode_ms
            );

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

    #[cfg(feature = "whisper")]
    #[test]
    fn compiled_backend_is_one_of_the_known_names() {
        assert!(
            matches!(compiled_backend(), "cpu" | "metal" | "coreml" | "metal+coreml"),
            "unexpected backend name: {}",
            compiled_backend()
        );
    }

    /// The coupling that actually matters, and the one a broken feature forward
    /// would hide: `kea-app/whisper-metal` -> `kea-engines` -> `kea-infer` ->
    /// `whisper-rs/metal` -> whisper-rs's internal `_gpu`, which is what makes
    /// `WhisperContextParameters::default()` come back with `use_gpu: true`.
    ///
    /// Worth asserting because the failure is silent: drop any link in that
    /// chain and the build still succeeds, the transcript is still correct, and
    /// the only symptom is that decoding stayed on the CPU.
    #[cfg(feature = "whisper")]
    #[test]
    fn metal_feature_reaches_whisper_rs_gpu_default() {
        let params = whisper_rs::WhisperContextParameters::default();
        assert_eq!(
            params.use_gpu,
            cfg!(feature = "whisper-metal"),
            "whisper-metal must reach whisper-rs's _gpu feature (got use_gpu={}, \
             whisper-metal={})",
            params.use_gpu,
            cfg!(feature = "whisper-metal")
        );
    }
}
