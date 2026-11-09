//! The dictation trigger loop, separated from the app so it can be tested
//! without a window server.
//!
//! The hotkey paths decide *what* to do (`commands::dictation_hotkey_action`
//! and `hold_dictation_action`); this module is the part that carries a
//! decision out and, for hold-to-talk, feeds it a stream of key edges. It is
//! generic over [`DictationOps`] so the one property that matters can be
//! asserted: **the loop is never parked behind a transcription.**
//!
//! # Why that is the whole bug
//!
//! The hold-to-talk dispatcher is a single task that handles one edge at a
//! time. When `Stop` used to run the whole pipeline inline — decode, cleanup,
//! paste, seconds of work — every edge the user produced meanwhile sat in the
//! channel. When the run finished the loop drained them back to back: a
//! `Start`, then its `Stop`, microseconds apart. The recording they opened
//! lasted no time at all, held no samples, and Whisper failed on an empty
//! buffer. Six of the seven "Input sample buffer was empty" rows in a real
//! `data.db` started within a second of the previous run finishing, and the
//! seventh is a different fault (a capture that delivered nothing).
//!
//! So [`DictationOps::stop`] has a contract: it returns once the microphone is
//! released and the audio is safely handed to processing, which carries on in
//! the background. The loop then sees every edge when it happens, and a press
//! during processing starts a real recording.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use async_trait::async_trait;
use kea_platform::{DictationState, HoldAction};
use tokio::sync::mpsc::UnboundedReceiver;

use crate::commands::{hold_dictation_action, try_acquire_busy, DictationHotkeyAction};

/// Everything the trigger loop needs from the running app.
#[async_trait]
pub trait DictationOps: Send + Sync {
    /// `(a meeting is active or finishing, what the microphone is doing)`.
    async fn gate(&self) -> (bool, DictationState);

    async fn start(&self) -> Result<(), String>;

    /// A recording that outlives the keys.
    async fn start_locked(&self) -> Result<(), String>;

    /// Release the microphone and hand the audio to processing.
    ///
    /// **Must return as soon as the microphone is free**, not when the text has
    /// been inserted: see the module docs. Whatever happens to the transcript
    /// afterwards is reported by the processing itself.
    async fn stop(&self) -> Result<(), String>;

    /// Stop and throw the audio away.
    async fn cancel(&self) -> Result<(), String>;

    /// Open or close the pre-roll capture for a modifier edge. Never starts or
    /// stops a run.
    async fn preroll_edge(&self, event: HoldAction);

    /// Surface a failure to the user.
    fn report_error(&self, message: &str);

    /// The flag that serialises these handlers, so a chord and an accelerator
    /// landing together cannot both start a run.
    fn busy_flag(&self) -> &Arc<AtomicBool>;
}

/// Carry out one decision.
///
/// Shared by the accelerator and the hold chord so they cannot drift: they ask
/// different questions (a toggle versus a directed edge) but run the same five
/// transitions, and a lock started by one has to be stoppable by the other.
pub async fn run_action(ops: &dyn DictationOps, action: DictationHotkeyAction) {
    let outcome = match action {
        DictationHotkeyAction::Start => ops.start().await,
        DictationHotkeyAction::StartLocked => ops.start_locked().await,
        DictationHotkeyAction::Stop => ops.stop().await,
        DictationHotkeyAction::Cancel => ops.cancel().await,
        DictationHotkeyAction::Ignore => Ok(()),
    };
    if let Err(error) = outcome {
        ops.report_error(&error);
    }
}

/// Drive the hold-to-talk edges until the sender goes away.
pub async fn pump_hold_events(ops: &dyn DictationOps, mut events: UnboundedReceiver<HoldAction>) {
    while let Some(event) = events.recv().await {
        // Arming opens and closes the microphone without starting a run, so it
        // is handled before the busy flag: a modifier press must not contend
        // with a recording for the flag that serialises runs.
        if matches!(event, HoldAction::Arm | HoldAction::Disarm) {
            ops.preroll_edge(event).await;
            continue;
        }

        // Same busy flag as the accelerator path, so a chord and a
        // Cmd+Shift+D landing together cannot both start a run. Held only for
        // the start or stop itself — never for the processing — so this drops
        // an edge only when another handler is mid-flip, not because a
        // transcription is running.
        let Some(_busy) = try_acquire_busy(ops.busy_flag()) else {
            tracing::debug!(?event, "hold-to-talk ignored: dictation handler in flight");
            continue;
        };

        let (meeting_active, current) = ops.gate().await;
        let action = hold_dictation_action(event, current, meeting_active);
        run_action(ops, action).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};
    use tokio::sync::mpsc::unbounded_channel;

    /// How long the fake pipeline takes to "transcribe".
    const PROCESSING: Duration = Duration::from_millis(400);

    /// A microphone and a transcription pipeline with real durations.
    ///
    /// `blocking_stop` reproduces the old shape — `stop` returns only after
    /// processing — so the failure it causes stays on record as a test rather
    /// than a story.
    struct FakeOps {
        busy: Arc<AtomicBool>,
        blocking_stop: bool,
        mic_open_since: Mutex<Option<Instant>>,
        /// How long each finished recording lasted.
        recordings: Mutex<Vec<Duration>>,
        errors: Mutex<Vec<String>>,
    }

    impl FakeOps {
        fn new(blocking_stop: bool) -> Self {
            Self {
                busy: Arc::new(AtomicBool::new(false)),
                blocking_stop,
                mic_open_since: Mutex::new(None),
                recordings: Mutex::new(Vec::new()),
                errors: Mutex::new(Vec::new()),
            }
        }

        fn recordings(&self) -> Vec<Duration> {
            self.recordings.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl DictationOps for FakeOps {
        async fn gate(&self) -> (bool, DictationState) {
            let listening = self.mic_open_since.lock().unwrap().is_some();
            (
                false,
                if listening {
                    DictationState::Listening
                } else {
                    DictationState::Idle
                },
            )
        }

        async fn start(&self) -> Result<(), String> {
            *self.mic_open_since.lock().unwrap() = Some(Instant::now());
            Ok(())
        }

        async fn start_locked(&self) -> Result<(), String> {
            self.start().await
        }

        async fn stop(&self) -> Result<(), String> {
            let since = self
                .mic_open_since
                .lock()
                .unwrap()
                .take()
                .ok_or("dictation is not listening")?;
            self.recordings.lock().unwrap().push(since.elapsed());
            if self.blocking_stop {
                tokio::time::sleep(PROCESSING).await;
            } else {
                tokio::spawn(tokio::time::sleep(PROCESSING));
            }
            Ok(())
        }

        async fn cancel(&self) -> Result<(), String> {
            *self.mic_open_since.lock().unwrap() = None;
            Ok(())
        }

        async fn preroll_edge(&self, _event: HoldAction) {}

        fn report_error(&self, message: &str) {
            self.errors.lock().unwrap().push(message.to_string());
        }

        fn busy_flag(&self) -> &Arc<AtomicBool> {
            &self.busy
        }
    }

    /// The user dictates, releases, and — while that is still being
    /// transcribed — holds the key again for 200ms. The edges are sent at the
    /// moments they happen; the question is when the loop acts on them.
    async fn dictate_twice(ops: Arc<FakeOps>) {
        let (tx, rx) = unbounded_channel();
        let pump = {
            let ops = ops.clone();
            tokio::spawn(async move { pump_hold_events(ops.as_ref(), rx).await })
        };

        tx.send(HoldAction::Start).unwrap();
        tokio::time::sleep(Duration::from_millis(150)).await;
        tx.send(HoldAction::Stop).unwrap();

        // Processing of the first run is under way. The second hold:
        tokio::time::sleep(Duration::from_millis(50)).await;
        tx.send(HoldAction::Start).unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;
        tx.send(HoldAction::Stop).unwrap();

        // Let the loop work through whatever it has queued.
        tokio::time::sleep(PROCESSING * 2).await;
        drop(tx);
        pump.await.unwrap();
    }

    /// **The regression test.** A press that lands while the previous run is
    /// still processing starts a real recording, and the release ends it after
    /// the time the key was actually held — not after however long it took the
    /// loop to get round to it.
    #[tokio::test]
    async fn a_press_during_processing_records_for_the_time_the_key_was_held() {
        let ops = Arc::new(FakeOps::new(false));
        dictate_twice(ops.clone()).await;

        let recordings = ops.recordings();
        assert_eq!(recordings.len(), 2, "both holds recorded: {recordings:?}");
        assert!(
            recordings[1] >= Duration::from_millis(150),
            "the second hold lasted ~200ms, not {:?}",
            recordings[1]
        );
        assert!(ops.errors.lock().unwrap().is_empty());
    }

    /// Why `stop` must not block, kept as a characterisation of the old shape:
    /// with processing inline the second hold's edges queue up behind it and are
    /// replayed back to back, so the recording lasts no time at all. That is a
    /// zero-sample capture, which Whisper reports as "Input sample buffer was
    /// empty".
    #[tokio::test]
    async fn a_blocking_stop_replays_a_queued_press_as_a_zero_length_recording() {
        let ops = Arc::new(FakeOps::new(true));
        dictate_twice(ops.clone()).await;

        let recordings = ops.recordings();
        assert_eq!(recordings.len(), 2);
        assert!(
            recordings[1] < Duration::from_millis(50),
            "replayed instantly, the 200ms hold became {:?}",
            recordings[1]
        );
    }

    /// An accelerator press during processing is no longer dropped either: the
    /// busy flag is free the moment the microphone is.
    #[tokio::test]
    async fn the_busy_flag_is_free_while_processing_continues() {
        let ops = Arc::new(FakeOps::new(false));
        let (tx, rx) = unbounded_channel();
        let pump = {
            let ops = ops.clone();
            tokio::spawn(async move { pump_hold_events(ops.as_ref(), rx).await })
        };
        tx.send(HoldAction::Start).unwrap();
        tx.send(HoldAction::Stop).unwrap();
        tokio::time::sleep(Duration::from_millis(50)).await;

        assert!(
            try_acquire_busy(ops.busy_flag()).is_some(),
            "the flag must not be held across the transcription"
        );
        drop(tx);
        pump.await.unwrap();
    }

    /// A failed start is reported, not swallowed.
    #[tokio::test]
    async fn a_failure_is_reported_to_the_user() {
        let ops = FakeOps::new(false);
        // Stop with nothing recording.
        run_action(&ops, DictationHotkeyAction::Stop).await;
        assert_eq!(ops.errors.lock().unwrap().len(), 1);
    }
}
