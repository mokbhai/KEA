//! The chunked file-transcription driver.
//!
//! Chunking is not a model limit — whisper windows internally. It is what
//! makes progress reportable, cancellation granular and peak memory bounded,
//! and it is the reason every engine can produce at least chunk-granularity
//! cues even when it reports no timing of its own.
//!
//! Pure over its collaborators: the engine, the progress sink and the cancel
//! check are all injected, so the whole driver is testable with a fake engine
//! and no file, no device and no pool.

use kea_engines::traits::{AudioPcm, EngineError, SttEngine, SttOpts, SttSegment};

/// How long a chunk aims to be. Thirty seconds is whisper's own window, so a
/// chunk is one decode rather than several, and it is short enough that a
/// cancel lands within a few seconds.
pub const DEFAULT_CHUNK_SECS: u32 = 30;

/// One chunk's extent, in samples and in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChunkSpan {
    pub start_sample: usize,
    pub end_sample: usize,
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Turns interior cut indices into the spans the driver decodes.
///
/// Separate from the cut search (which lives in `kea-platform`, next to the
/// audio primitives it uses) because *this* half is what has to be right:
/// a span whose `start_ms` is wrong shifts every cue after it, silently.
pub fn plan_chunks(total_samples: usize, rate_hz: u32, cuts: &[usize]) -> Vec<ChunkSpan> {
    if total_samples == 0 || rate_hz == 0 {
        return Vec::new();
    }
    let ms_at = |sample: usize| (sample as u64 * 1_000) / rate_hz as u64;

    let mut bounds: Vec<usize> = Vec::with_capacity(cuts.len() + 2);
    bounds.push(0);
    for &cut in cuts {
        if cut > *bounds.last().unwrap() && cut < total_samples {
            bounds.push(cut);
        }
    }
    bounds.push(total_samples);

    bounds
        .windows(2)
        .map(|w| ChunkSpan {
            start_sample: w[0],
            end_sample: w[1],
            start_ms: ms_at(w[0]),
            end_ms: ms_at(w[1]),
        })
        .collect()
}

/// What the driver reports as it works.
///
/// A trait rather than two closures: the Tauri command needs both callbacks
/// to close over the same `AppHandle` and job id, and two closures each
/// capturing it is two clones of the same state.
pub trait TranscribeSink: Send + Sync {
    /// A chunk finished. `done_ms`/`total_ms` are audio time, not wall time.
    fn progress(&self, chunk_index: usize, chunk_count: usize, done_ms: u64, total_ms: u64);
    /// A cue landed, already rebased onto the source timeline.
    fn segment(&self, segment: &SttSegment);
    /// Whether the job should stop. Checked between chunks — a decode already
    /// in flight cannot be interrupted, so worst-case latency is one chunk.
    fn cancelled(&self) -> bool {
        false
    }
}

/// A sink that does nothing, for callers that only want the result.
pub struct SilentSink;

impl TranscribeSink for SilentSink {
    fn progress(&self, _: usize, _: usize, _: u64, _: u64) {}
    fn segment(&self, _: &SttSegment) {}
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribeOutcome {
    pub segments: Vec<SttSegment>,
    pub text: String,
    /// True when the sink asked to stop before the last chunk. The segments
    /// already produced are kept — a cancel is "stop here", not "throw away
    /// the forty minutes you already did".
    pub cancelled: bool,
}

/// Transcribes `audio` chunk by chunk, rebasing each chunk's segments onto
/// the source timeline.
///
/// The rebase is the one thing here that fails silently: an engine reports
/// offsets relative to the buffer it was handed, so without
/// [`SttSegment::shifted`] every cue after the first chunk lands at the
/// beginning of the file and the subtitles look plausibly, uniformly wrong.
pub async fn transcribe_chunks(
    engine: &dyn SttEngine,
    audio: &AudioPcm,
    spans: &[ChunkSpan],
    opts: &SttOpts,
    sink: &dyn TranscribeSink,
) -> Result<TranscribeOutcome, EngineError> {
    let total_ms = spans.last().map(|s| s.end_ms).unwrap_or(0);
    let mut segments: Vec<SttSegment> = Vec::new();
    let mut text = String::new();
    let mut cancelled = false;

    for (index, span) in spans.iter().enumerate() {
        if sink.cancelled() {
            cancelled = true;
            break;
        }
        let chunk = AudioPcm {
            samples: audio.samples[span.start_sample..span.end_sample].to_vec(),
            sample_rate_hz: audio.sample_rate_hz,
        };
        let transcript = engine.transcribe(chunk, opts.clone()).await?;

        let piece = transcript.text.trim();
        if !piece.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(piece);
        }

        let chunk_segments: Vec<SttSegment> = if transcript.segments.is_empty() {
            // The always-available fallback: the chunk boundaries are ours,
            // so an engine that reports no timing still yields a usable cue.
            // Skipped for an empty chunk, because a cue with no text is a cue
            // the writers would drop anyway.
            if piece.is_empty() {
                Vec::new()
            } else {
                vec![SttSegment::new(span.start_ms, span.end_ms, piece)]
            }
        } else {
            transcript
                .segments
                .iter()
                .map(|s| s.shifted(span.start_ms))
                .collect()
        };

        for segment in &chunk_segments {
            sink.segment(segment);
        }
        segments.extend(chunk_segments);
        sink.progress(index, spans.len(), span.end_ms, total_ms);
    }

    Ok(TranscribeOutcome {
        segments,
        text,
        cancelled,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_engines::traits::{EngineCaps, Transcript};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;

    fn audio(secs: u32) -> AudioPcm {
        AudioPcm {
            samples: vec![0.1; 16_000 * secs as usize],
            sample_rate_hz: 16_000,
        }
    }

    /// Reports one segment per call, 200 ms into its own buffer — so a
    /// driver that forgot to rebase produces 200 ms for every chunk.
    struct OneSegmentPerCall {
        calls: AtomicUsize,
        timed: bool,
    }

    #[async_trait]
    impl SttEngine for OneSegmentPerCall {
        fn id(&self) -> &str {
            "fake"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps { models: vec![] }
        }
        async fn transcribe(
            &self,
            audio: AudioPcm,
            _opts: SttOpts,
        ) -> Result<Transcript, EngineError> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            let text = format!("chunk{n}");
            if !self.timed {
                return Ok(Transcript::text_only(text));
            }
            let len_ms = (audio.samples.len() as u64 * 1_000) / audio.sample_rate_hz as u64;
            Ok(Transcript {
                text: text.clone(),
                segments: vec![SttSegment::new(
                    200,
                    len_ms.saturating_sub(200).max(200),
                    text,
                )],
            })
        }
    }

    #[derive(Default)]
    struct RecordingSink {
        segments: Mutex<Vec<SttSegment>>,
        progress: Mutex<Vec<(usize, usize, u64, u64)>>,
        cancel_after: Option<usize>,
    }

    impl TranscribeSink for RecordingSink {
        fn progress(&self, index: usize, count: usize, done_ms: u64, total_ms: u64) {
            self.progress
                .lock()
                .unwrap()
                .push((index, count, done_ms, total_ms));
        }
        fn segment(&self, segment: &SttSegment) {
            self.segments.lock().unwrap().push(segment.clone());
        }
        fn cancelled(&self) -> bool {
            match self.cancel_after {
                Some(after) => self.progress.lock().unwrap().len() >= after,
                None => false,
            }
        }
    }

    #[test]
    fn chunk_spans_cover_the_whole_buffer_without_gaps_or_overlap() {
        let spans = plan_chunks(16_000 * 95, 16_000, &[16_000 * 30, 16_000 * 62]);
        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0].start_ms, 0);
        assert_eq!(spans[0].end_ms, 30_000);
        assert_eq!(spans[1].start_ms, 30_000);
        assert_eq!(spans[2].end_ms, 95_000);
        for pair in spans.windows(2) {
            assert_eq!(pair[0].end_sample, pair[1].start_sample);
        }
    }

    #[test]
    fn a_cut_outside_the_buffer_is_ignored_rather_than_producing_an_empty_chunk() {
        let spans = plan_chunks(1_000, 16_000, &[0, 1_000, 5_000, 400]);
        assert_eq!(spans.len(), 2, "{spans:?}");
        assert_eq!(spans[0].start_sample, 0);
        assert_eq!(spans[0].end_sample, 400);
        assert_eq!(spans[1].end_sample, 1_000);
    }

    /// The item's named silent failure: chunk 2's segments have to land at
    /// 30 s, not back at the start of the file.
    #[tokio::test]
    async fn segments_are_rebased_onto_the_source_timeline() {
        let audio = audio(95);
        let spans = plan_chunks(audio.samples.len(), 16_000, &[16_000 * 30, 16_000 * 62]);
        let engine = OneSegmentPerCall {
            calls: AtomicUsize::new(0),
            timed: true,
        };
        let sink = RecordingSink::default();
        let out = transcribe_chunks(&engine, &audio, &spans, &SttOpts::default(), &sink)
            .await
            .unwrap();

        assert_eq!(out.segments.len(), 3);
        assert_eq!(out.segments[0].start_ms, 200);
        assert_eq!(out.segments[1].start_ms, 30_200);
        assert_eq!(out.segments[2].start_ms, 62_200);
        assert!(
            out.segments
                .windows(2)
                .all(|w| w[0].start_ms < w[1].start_ms),
            "offsets must be monotonic"
        );
        assert!(out.segments.last().unwrap().end_ms <= 95_000);
        assert_eq!(out.text, "chunk0 chunk1 chunk2");
        assert!(!out.cancelled);
        assert_eq!(sink.segments.lock().unwrap().len(), 3);
        assert_eq!(
            sink.progress.lock().unwrap().last().copied(),
            Some((2, 3, 95_000, 95_000))
        );
    }

    /// An engine with no timing of its own still has to produce cues, because
    /// the chunk boundaries belong to us.
    #[tokio::test]
    async fn an_untimed_engine_falls_back_to_chunk_granularity_cues() {
        let audio = audio(60);
        let spans = plan_chunks(audio.samples.len(), 16_000, &[16_000 * 30]);
        let engine = OneSegmentPerCall {
            calls: AtomicUsize::new(0),
            timed: false,
        };
        let out = transcribe_chunks(&engine, &audio, &spans, &SttOpts::default(), &SilentSink)
            .await
            .unwrap();
        assert_eq!(out.segments.len(), 2);
        assert_eq!(out.segments[0].start_ms, 0);
        assert_eq!(out.segments[0].end_ms, 30_000);
        assert_eq!(out.segments[1].start_ms, 30_000);
        assert_eq!(out.segments[1].end_ms, 60_000);
    }

    /// Cancel keeps what was already transcribed: a cancel is "stop here",
    /// not "discard the forty minutes you already did".
    #[tokio::test]
    async fn a_cancel_after_two_chunks_keeps_those_two_and_says_so() {
        let audio = audio(95);
        let spans = plan_chunks(audio.samples.len(), 16_000, &[16_000 * 30, 16_000 * 62]);
        let engine = OneSegmentPerCall {
            calls: AtomicUsize::new(0),
            timed: true,
        };
        let sink = RecordingSink {
            cancel_after: Some(2),
            ..Default::default()
        };
        let out = transcribe_chunks(&engine, &audio, &spans, &SttOpts::default(), &sink)
            .await
            .unwrap();
        assert!(out.cancelled);
        assert_eq!(out.segments.len(), 2);
        assert_eq!(engine.calls.load(Ordering::SeqCst), 2);
        // No gap in what was written: the kept cues are still contiguous.
        assert!(out.segments[0].end_ms <= out.segments[1].start_ms);
    }
}
