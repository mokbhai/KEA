//! The streaming STT engine: live partial text while the user is still talking.
//!
//! **Two-pass, and that is the point, not a compromise.** What this engine
//! produces is a display-only hypothesis. When the user stops, the offline
//! engine ([`crate::stt::parakeet`], [`crate::stt::whisper`]) re-decodes the
//! complete buffer and that result is what reaches the text field. Streaming
//! Zipformer English accuracy is materially below Parakeet TDT, so a one-pass
//! streaming design would trade the accuracy of the inserted text for feedback
//! latency. Nothing in the insertion path goes through here.
//!
//! Which is also why this engine is allowed to be lossy, to fall behind, to be
//! wrong, and not to exist at all: with no streaming model installed, `open`
//! returns [`EngineError::ModelNotInstalled`] and dictation works exactly as it
//! does without this file.

use std::sync::Arc;

use async_trait::async_trait;
use kea_infer::{
    ModelRegistry, ModelStorage, SherpaStreamingInference, StreamingCfg, STREAMING_SAMPLE_RATE_HZ,
};
use tokio::sync::{mpsc, oneshot};

use crate::stt::audio::resample_to_rate;
use crate::traits::{
    AudioPcm, EngineCaps, EngineError, Partial, StreamingSttEngine, SttOpts, SttStream, Transcript,
};

/// The engine id this registers under. Not an [`crate::traits::SttEngine`] id:
/// nothing binds to it and no slot resolves to it — it is chosen by the
/// `dictation.streaming_model` setting.
pub const STREAMING_STT_ENGINE_ID: &str = "streaming-zipformer";

/// Frames the decoder may fall behind by before frames start being dropped.
///
/// Sixteen frames is a few hundred milliseconds. An unbounded queue is the
/// tempting alternative and is worse: a decoder running below realtime would
/// grow it without limit and show partials lagging further behind every
/// second, which reads as a hang. A dropped frame costs a word in the preview;
/// an unbounded queue costs trust in the preview.
const DECODE_QUEUE_FRAMES: usize = 16;

/// How many partials may wait to be picked up. Same reasoning, and the reader
/// keeps only the newest anyway.
const PARTIAL_QUEUE_DEPTH: usize = 16;

/// Dropped frames in one session past which feeding stops altogether.
///
/// A preview assembled from a third of the audio is not a preview, it is
/// misinformation. Better to freeze on the last honest hypothesis and say so
/// once in the log than to keep showing text built from a shredded signal.
const MAX_DROPPED_FRAMES: u64 = 200;

pub struct StreamingZipformerEngine {
    inference: Arc<dyn SherpaStreamingInference>,
    storage: Arc<ModelStorage>,
    cfg: StreamingCfg,
}

impl StreamingZipformerEngine {
    pub fn new(inference: Arc<dyn SherpaStreamingInference>, storage: Arc<ModelStorage>) -> Self {
        Self {
            inference,
            storage,
            cfg: StreamingCfg::default(),
        }
    }
}

#[async_trait]
impl StreamingSttEngine for StreamingZipformerEngine {
    fn id(&self) -> &str {
        STREAMING_STT_ENGINE_ID
    }

    fn capabilities(&self) -> EngineCaps {
        EngineCaps {
            models: ModelRegistry::onnx_catalog(kea_infer::ModelKind::Streaming)
                .unwrap_or_default()
                .into_iter()
                .map(|m| m.id)
                .collect(),
        }
    }

    async fn open(&self, opts: SttOpts) -> Result<Box<dyn SttStream>, EngineError> {
        let model_id = opts
            .model
            .as_deref()
            .ok_or_else(|| EngineError::Config("streaming stt requires a model id".into()))?;

        if !self.storage.is_onnx_installed(model_id) {
            return Err(EngineError::ModelNotInstalled(format!(
                "streaming model not installed: {model_id}"
            )));
        }

        let model_dir = self.storage.onnx_dir_for(model_id);
        let inference = self.inference.clone();
        let cfg = self.cfg;

        let (audio_tx, mut audio_rx) = mpsc::channel::<kea_infer::AudioPcm>(DECODE_QUEUE_FRAMES);
        let (partial_tx, partial_rx) = mpsc::channel::<Partial>(PARTIAL_QUEUE_DEPTH);
        let (open_tx, open_rx) = oneshot::channel::<Result<(), String>>();
        let (final_tx, final_rx) = oneshot::channel::<Result<String, String>>();

        // One blocking task for the whole session, not one per frame: the
        // infer layer is synchronous precisely so the dispatch cost is paid
        // once rather than every 10 ms of audio.
        tokio::task::spawn_blocking(move || {
            let mut session = match inference.open(&model_dir, cfg) {
                Ok(session) => {
                    let _ = open_tx.send(Ok(()));
                    session
                }
                Err(e) => {
                    let _ = open_tx.send(Err(e.to_string()));
                    return;
                }
            };

            let mut segment: u32 = 0;
            while let Some(pcm) = audio_rx.blocking_recv() {
                match session.accept(pcm) {
                    Ok(text) => {
                        let endpoint = session.endpointed();
                        if let Some(text) = text {
                            // Dropped rather than queued when the reader is
                            // behind: a superseded hypothesis has no value.
                            let _ = partial_tx.try_send(Partial {
                                text,
                                segment,
                                endpoint,
                            });
                        }
                        // Counted even when the text did not change, so the
                        // segment numbers the consumer sees stay contiguous.
                        if endpoint {
                            segment = segment.saturating_add(1);
                        }
                    }
                    Err(e) => {
                        let _ = final_tx.send(Err(e.to_string()));
                        return;
                    }
                }
            }

            let _ = final_tx.send(session.finish().map_err(|e| e.to_string()));
        });

        match open_rx.await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => return Err(EngineError::Other(e)),
            Err(_) => {
                return Err(EngineError::Other(
                    "streaming session failed to start".into(),
                ))
            }
        }

        Ok(Box::new(SherpaSttStream {
            audio_tx,
            partial_rx,
            final_rx,
            dropped: 0,
            starved: false,
        }))
    }
}

struct SherpaSttStream {
    audio_tx: mpsc::Sender<kea_infer::AudioPcm>,
    partial_rx: mpsc::Receiver<Partial>,
    final_rx: oneshot::Receiver<Result<String, String>>,
    dropped: u64,
    starved: bool,
}

#[async_trait]
impl SttStream for SherpaSttStream {
    async fn accept(&mut self, audio: AudioPcm) -> Result<Option<Partial>, EngineError> {
        if !self.starved {
            // Defensive: the caller resamples, but feeding the feature
            // extractor anything but 16 kHz makes sherpa resample per chunk,
            // badly.
            let samples = resample_to_rate(
                &audio.samples,
                audio.sample_rate_hz,
                STREAMING_SAMPLE_RATE_HZ,
            );
            let pcm = kea_infer::AudioPcm {
                samples,
                sample_rate_hz: STREAMING_SAMPLE_RATE_HZ,
            };

            match self.audio_tx.try_send(pcm) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    self.dropped += 1;
                    if self.dropped > MAX_DROPPED_FRAMES {
                        self.starved = true;
                        tracing::warn!(
                            dropped = self.dropped,
                            "streaming stt is too far behind; freezing the preview for the rest \
                             of this run (the inserted text is unaffected)"
                        );
                    }
                }
                // The decode task ended — it errored, or it was finalized.
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(EngineError::Other("streaming session has ended".into()))
                }
            }
        }

        // Last wins: everything queued behind the newest hypothesis has
        // already been superseded by it.
        let mut latest = None;
        while let Ok(partial) = self.partial_rx.try_recv() {
            latest = Some(partial);
        }
        Ok(latest)
    }

    async fn finalize(self: Box<Self>) -> Result<Transcript, EngineError> {
        let this = *self;
        // Closing the feed is what tells the decode loop to flush: the frames
        // already queued are still delivered first.
        drop(this.audio_tx);
        match this.final_rx.await {
            // No timing: a streaming decode is display-only and the offline
            // pass is what produces the text that is inserted.
            Ok(Ok(text)) => Ok(Transcript::text_only(text)),
            Ok(Err(e)) => Err(EngineError::Other(e)),
            // The blocking task panicked. Display-only work must never take
            // the dictation run down with it.
            Err(_) => Err(EngineError::Other(
                "streaming session ended without a result".into(),
            )),
        }
    }
}

#[cfg(feature = "streaming")]
pub fn register_streaming_stt_engine(
    reg: &mut crate::registry::EngineRegistry,
    inference: Arc<dyn SherpaStreamingInference>,
    storage: Arc<ModelStorage>,
) {
    reg.register_streaming_stt(Arc::new(StreamingZipformerEngine::new(inference, storage)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use kea_infer::{InferError, SherpaStreamSession};
    use std::path::Path;
    use std::sync::Mutex;

    /// A session that answers with a scripted hypothesis per frame. `None`
    /// scripts "the hypothesis did not change", which is what a greedy decoder
    /// says most of the time.
    struct ScriptedSession {
        script: Vec<(Option<&'static str>, bool)>,
        index: usize,
        endpointed: bool,
        last: String,
        fail_at: Option<usize>,
        delay: std::time::Duration,
        /// When present, one decode happens per token sent. Without it the
        /// decoder runs as fast as frames arrive and several hypotheses can
        /// land between two reads — which is correct behaviour (superseded
        /// hypotheses are discarded) and useless for a test about ordering.
        gate: Option<std::sync::mpsc::Receiver<()>>,
    }

    impl SherpaStreamSession for ScriptedSession {
        fn accept(&mut self, _pcm: kea_infer::AudioPcm) -> Result<Option<String>, InferError> {
            if let Some(gate) = &self.gate {
                if gate.recv().is_err() {
                    return Ok(None);
                }
            }
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            let step = self.index;
            self.index += 1;
            if self.fail_at == Some(step) {
                return Err(InferError::Other("decoder exploded".into()));
            }
            let (text, endpoint) = self.script.get(step).copied().unwrap_or((None, false));
            self.endpointed = endpoint;
            match text {
                Some(text) => {
                    self.last = text.to_string();
                    Ok(Some(text.to_string()))
                }
                None => Ok(None),
            }
        }

        fn endpointed(&self) -> bool {
            self.endpointed
        }

        fn finish(&mut self) -> Result<String, InferError> {
            Ok(self.last.clone())
        }
    }

    struct ScriptedInference {
        script: Vec<(Option<&'static str>, bool)>,
        fail_at: Option<usize>,
        delay: std::time::Duration,
        opened_with: Mutex<Option<StreamingCfg>>,
        refuse: bool,
        gate: Mutex<Option<std::sync::mpsc::Receiver<()>>>,
    }

    impl ScriptedInference {
        fn new(script: Vec<(Option<&'static str>, bool)>) -> Self {
            Self {
                script,
                fail_at: None,
                delay: std::time::Duration::ZERO,
                opened_with: Mutex::new(None),
                refuse: false,
                gate: Mutex::new(None),
            }
        }
    }

    impl SherpaStreamingInference for ScriptedInference {
        fn open(
            &self,
            _model_dir: &Path,
            cfg: StreamingCfg,
        ) -> Result<Box<dyn SherpaStreamSession>, InferError> {
            *self.opened_with.lock().unwrap() = Some(cfg);
            if self.refuse {
                return Err(InferError::Other("no such bundle".into()));
            }
            Ok(Box::new(ScriptedSession {
                script: self.script.clone(),
                index: 0,
                endpointed: false,
                last: String::new(),
                fail_at: self.fail_at,
                delay: self.delay,
                gate: self.gate.lock().unwrap().take(),
            }))
        }
    }

    fn installed_storage(model_id: &str) -> (tempfile::TempDir, Arc<ModelStorage>) {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let model_dir = storage.onnx_dir_for(model_id);
        std::fs::create_dir_all(&model_dir).unwrap();
        std::fs::write(model_dir.join("tokens.txt"), b"tok").unwrap();
        (dir, storage)
    }

    fn frame() -> AudioPcm {
        AudioPcm {
            samples: vec![0.0; 160],
            sample_rate_hz: 16_000,
        }
    }

    /// An empty frame: it still queues, but the gated decoder will not consume
    /// it until the test releases another tick.
    fn silence() -> AudioPcm {
        AudioPcm {
            samples: Vec::new(),
            sample_rate_hz: 16_000,
        }
    }

    /// `Box<dyn SttStream>` is not `Debug`, so `unwrap_err` cannot see it.
    fn expect_open_error(result: Result<Box<dyn SttStream>, EngineError>) -> EngineError {
        match result {
            Ok(_) => panic!("expected the open to be refused"),
            Err(e) => e,
        }
    }

    fn opts(model: &str) -> SttOpts {
        SttOpts {
            model: Some(model.into()),
            ..Default::default()
        }
    }

    /// Partials arrive in order, `segment` advances only across an endpoint,
    /// and `finalize` returns the last hypothesis.
    #[tokio::test]
    async fn partials_arrive_in_order_and_segments_advance_on_endpoints() {
        let (_dir, storage) = installed_storage("streaming-zipformer-en-20m");
        let (tick, gate) = std::sync::mpsc::channel::<()>();
        let inference = ScriptedInference::new(vec![
            (Some("hello"), false),
            (Some("hello wurld"), true),
            (Some("again"), false),
        ]);
        *inference.gate.lock().unwrap() = Some(gate);
        let engine = StreamingZipformerEngine::new(Arc::new(inference), storage);
        let mut stream = engine
            .open(opts("streaming-zipformer-en-20m"))
            .await
            .unwrap();

        let mut seen = Vec::new();
        for _ in 0..3 {
            // One frame in, one decode released, one hypothesis out.
            stream.accept(frame()).await.unwrap();
            tick.send(()).unwrap();
            seen.push(next_partial(stream.as_mut()).await.expect("a partial"));
        }

        assert_eq!(seen[0].text, "hello");
        assert_eq!(seen[0].segment, 0);
        assert!(!seen[0].endpoint);
        assert_eq!(seen[1].text, "hello wurld");
        assert_eq!(seen[1].segment, 0, "the endpoint closes this segment");
        assert!(seen[1].endpoint);
        assert_eq!(seen[2].text, "again");
        assert_eq!(seen[2].segment, 1, "the next segment");
        assert!(!seen[2].endpoint);

        // Releasing the gate lets the flush run; the decoder is waiting on it.
        drop(tick);
        let transcript = stream.finalize().await.unwrap();
        assert_eq!(transcript.text, "again");
    }

    /// Waits for the hypothesis a released decode produced, without feeding
    /// more audio — feeding more would let a second hypothesis supersede it.
    async fn next_partial(stream: &mut dyn SttStream) -> Option<Partial> {
        for _ in 0..200 {
            if let Some(partial) = stream.accept(silence()).await.ok().flatten() {
                return Some(partial);
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        None
    }

    /// The config the decoder is opened with is the guarded one, not
    /// `OnlineRecognizerConfig::default()` — see `StreamingCfg`.
    #[tokio::test]
    async fn the_session_is_opened_with_endpointing_configured() {
        let (_dir, storage) = installed_storage("streaming-zipformer-en-20m");
        let inference = Arc::new(ScriptedInference::new(vec![]));
        let engine = StreamingZipformerEngine::new(inference.clone(), storage);
        let _stream = engine
            .open(opts("streaming-zipformer-en-20m"))
            .await
            .unwrap();

        let cfg = inference.opened_with.lock().unwrap().unwrap();
        assert!(cfg.enable_endpoint);
        assert!(cfg.rule1_min_trailing_silence > 0.0);
        assert!(cfg.rule2_min_trailing_silence > 0.0);
        assert!(cfg.rule3_min_utterance_length > 0.0);
    }

    /// The whole feature is optional. A model that is not on disk is the
    /// normal case, and it must be a named refusal rather than a panic or a
    /// generic failure the caller cannot tell apart.
    #[tokio::test]
    async fn an_absent_model_is_reported_as_not_installed() {
        let dir = tempfile::tempdir().unwrap();
        let storage = Arc::new(ModelStorage::new(dir.path().to_path_buf()));
        let engine =
            StreamingZipformerEngine::new(Arc::new(ScriptedInference::new(vec![])), storage);
        let err = expect_open_error(engine.open(opts("streaming-zipformer-en-20m")).await);
        assert!(matches!(err, EngineError::ModelNotInstalled(_)), "{err}");
    }

    #[tokio::test]
    async fn opening_without_a_model_id_is_a_config_error() {
        let (_dir, storage) = installed_storage("streaming-zipformer-en-20m");
        let engine =
            StreamingZipformerEngine::new(Arc::new(ScriptedInference::new(vec![])), storage);
        let err = expect_open_error(engine.open(SttOpts::default()).await);
        assert!(matches!(err, EngineError::Config(_)), "{err}");
    }

    #[tokio::test]
    async fn a_decoder_that_refuses_to_open_surfaces_the_reason() {
        let (_dir, storage) = installed_storage("streaming-zipformer-en-20m");
        let mut inference = ScriptedInference::new(vec![]);
        inference.refuse = true;
        let engine = StreamingZipformerEngine::new(Arc::new(inference), storage);
        let err = expect_open_error(engine.open(opts("streaming-zipformer-en-20m")).await);
        assert!(err.to_string().contains("no such bundle"), "{err}");
    }

    /// A decoder that dies mid-session must surface as an error, not a hang
    /// and not a panic: the dictation run it is attached to has to finish.
    #[tokio::test]
    async fn a_decoder_that_errors_mid_session_reports_it_at_finalize() {
        let (_dir, storage) = installed_storage("streaming-zipformer-en-20m");
        let mut inference = ScriptedInference::new(vec![(Some("hello"), false)]);
        inference.fail_at = Some(1);
        let engine = StreamingZipformerEngine::new(Arc::new(inference), storage);
        let mut stream = engine
            .open(opts("streaming-zipformer-en-20m"))
            .await
            .unwrap();

        // Feed past the failure point; `accept` may report the closed feed
        // itself, which is equally acceptable.
        for _ in 0..50 {
            if stream.accept(frame()).await.is_err() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }

        let err = stream.finalize().await.unwrap_err();
        assert!(
            err.to_string().contains("exploded") || err.to_string().contains("without a result"),
            "{err}"
        );
    }

    /// Backpressure: a decoder slower than realtime drops frames rather than
    /// queueing them, and never blocks the caller.
    #[tokio::test]
    async fn a_slow_decoder_drops_frames_instead_of_queueing_them() {
        let (_dir, storage) = installed_storage("streaming-zipformer-en-20m");
        let mut inference = ScriptedInference::new(vec![(Some("slow"), false); 8]);
        inference.delay = std::time::Duration::from_millis(20);
        let engine = StreamingZipformerEngine::new(Arc::new(inference), storage);
        let mut stream = engine
            .open(opts("streaming-zipformer-en-20m"))
            .await
            .unwrap();

        let started = std::time::Instant::now();
        for _ in 0..500 {
            stream.accept(frame()).await.unwrap();
        }
        // 500 frames against a 20 ms decode is ten seconds of work; if the
        // caller were being made to wait for it this would not return.
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "accept blocked on the decoder"
        );
    }
}
