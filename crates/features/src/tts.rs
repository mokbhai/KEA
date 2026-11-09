use kea_core::resolve::SlotResolver;
use kea_core::store::actions::{ActionRepo, NewAction};
use kea_core::store::bindings::{Binding, BindingRepo};
use kea_core::tts::TtsSettings;
use kea_engines::traits::TtsOpts;
use kea_engines::EngineRegistry;
use kea_platform::audio::AudioIo;
use kea_platform::textio::TextIo;
use kea_platform::PcmFrame;
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::Notify;

use crate::feature::{ActionGuard, CapKind, CapSlot, Command, Feature};

/// The reason written on the ledger row of a read the user ended. It lands in
/// the error column under the `cancelled` status, so History shows a grey
/// "stopped" rather than a red failure.
pub const STOPPED_REASON: &str = "stopped by user";

/// Where one read-aloud run is, as far as stopping it is concerned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadAloudPhase {
    /// Nothing is running.
    Idle,
    /// Capturing the selection and turning it into audio. Nothing is audible
    /// yet; a stop abandons the synthesis.
    Synthesizing,
    /// Audio is coming out of the speakers. A stop silences it.
    Playing,
}

/// What a press of the read-aloud shortcut means right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadAloudPress {
    Start,
    Stop,
}

/// The shortcut is a toggle: a press while a run is synthesizing or speaking
/// is directed at that run, never a second one on top of it (two voices at
/// once) and never dropped (the user is reaching for silence).
pub fn read_aloud_press(phase: ReadAloudPhase) -> ReadAloudPress {
    match phase {
        ReadAloudPhase::Idle => ReadAloudPress::Start,
        ReadAloudPhase::Synthesizing | ReadAloudPhase::Playing => ReadAloudPress::Stop,
    }
}

/// Whether Escape stops the read.
///
/// Only while audio is playing, and only with the setting on. The global
/// Escape binding is registered for exactly that window, but the press is
/// decided here again because a press can arrive after the run ended and the
/// setting can be flipped mid-read.
pub fn escape_stops_read_aloud(phase: ReadAloudPhase, stop_with_esc: bool) -> bool {
    stop_with_esc && phase == ReadAloudPhase::Playing
}

/// The stop switch for the current read-aloud run, shared by everything that
/// can end one: the shortcut, Escape, the Stop button and the local API.
///
/// One instance lives on the app state. A run claims it with [`try_begin`]
/// and releases it by dropping the returned [`ReadAloudRun`], on every exit
/// path.
///
/// [`try_begin`]: ReadAloudControl::try_begin
pub struct ReadAloudControl {
    phase: Mutex<ReadAloudPhase>,
    stop: Arc<AtomicBool>,
    notify: Notify,
}

impl Default for ReadAloudControl {
    fn default() -> Self {
        Self {
            phase: Mutex::new(ReadAloudPhase::Idle),
            stop: Arc::new(AtomicBool::new(false)),
            notify: Notify::new(),
        }
    }
}

impl ReadAloudControl {
    pub fn phase(&self) -> ReadAloudPhase {
        *self.phase.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Claim the control for a new run, or `None` when one is already going.
    pub fn try_begin(&self) -> Option<ReadAloudRun<'_>> {
        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
        if *phase != ReadAloudPhase::Idle {
            return None;
        }
        *phase = ReadAloudPhase::Synthesizing;
        self.stop.store(false, Ordering::SeqCst);
        Some(ReadAloudRun { control: self })
    }

    /// Ask the current run to stop. Returns whether there was one to ask.
    pub fn request_stop(&self) -> bool {
        let phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
        if *phase == ReadAloudPhase::Idle {
            return false;
        }
        self.stop.store(true, Ordering::SeqCst);
        // A permit is stored when nobody is waiting yet, so a stop that lands
        // just before synthesis starts awaiting it is not lost.
        self.notify.notify_one();
        true
    }

    pub fn stop_requested(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// The flag playback polls. The same one [`request_stop`] sets.
    ///
    /// [`request_stop`]: ReadAloudControl::request_stop
    pub fn stop_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stop)
    }

    /// Resolves once a stop has been requested for the current run.
    pub async fn stopped(&self) {
        loop {
            if self.stop_requested() {
                return;
            }
            // A stale permit from an earlier run only costs one more pass
            // through the flag check above.
            self.notify.notified().await;
        }
    }

    fn set_playing(&self) {
        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
        if *phase == ReadAloudPhase::Synthesizing {
            *phase = ReadAloudPhase::Playing;
        }
    }

    fn reset(&self) {
        let mut phase = self.phase.lock().unwrap_or_else(|p| p.into_inner());
        *phase = ReadAloudPhase::Idle;
        self.stop.store(false, Ordering::SeqCst);
    }
}

/// Holds the control for the length of one run; dropping it returns the
/// control to idle, including on `?` returns, cancellation and panics.
pub struct ReadAloudRun<'a> {
    control: &'a ReadAloudControl,
}

impl Drop for ReadAloudRun<'_> {
    fn drop(&mut self) {
        self.control.reset();
    }
}

/// How a read-aloud run ended when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TtsOutcome {
    /// The audio played to its end.
    Finished,
    /// The user stopped it, during synthesis or playback.
    Stopped,
}

/// Synthesis either hands back audio with its still-open ledger row, or was
/// stopped (and its row, if one was opened, is already closed).
pub enum Synthesis {
    Ready(i64, PcmFrame),
    Stopped,
}

pub struct TtsFeature;

impl Feature for TtsFeature {
    fn id(&self) -> &str {
        "tts"
    }

    fn required_caps(&self) -> Vec<CapSlot> {
        vec![CapSlot {
            name: "tts",
            kind: CapKind::Tts,
        }]
    }

    fn commands(&self) -> Vec<Command> {
        vec![Command {
            id: "read_selection".into(),
            title: "Read Selection Aloud".into(),
            default_accelerator: Some(default_tts_accelerator().into()),
        }]
    }
}

fn default_tts_accelerator() -> &'static str {
    #[cfg(target_os = "macos")]
    {
        "Cmd+Shift+T"
    }
    #[cfg(not(target_os = "macos"))]
    {
        "CommandOrControl+Shift+T"
    }
}

/// Capture the selection, synthesize it and hand the audio back with the
/// ledger row still open. The caller plays the audio and closes the row.
pub async fn run_tts_synthesize(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    settings: &TtsSettings,
) -> Result<(i64, PcmFrame), String> {
    match run_tts_synthesize_stoppable(engines, bindings, actions, textio, settings, None).await? {
        Synthesis::Ready(id, frame) => Ok((id, frame)),
        // Unreachable without a control to stop it with.
        Synthesis::Stopped => Err("stopped".into()),
    }
}

/// [`run_tts_synthesize`] that gives up when `control` is stopped.
///
/// A stop during synthesis drops the in-flight engine call (an HTTP request
/// is aborted, a local engine's result is discarded) and closes the ledger row
/// as cancelled, so nothing is left pending in History.
pub async fn run_tts_synthesize_stoppable(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    settings: &TtsSettings,
    control: Option<&ReadAloudControl>,
) -> Result<Synthesis, String> {
    let text = textio
        .capture_selection()
        .await
        .map_err(|e| e.to_string())?;
    // A stop during capture: no row exists yet, so there is nothing to close.
    if control.is_some_and(|c| c.stop_requested()) {
        return Ok(Synthesis::Stopped);
    }
    if text.trim().is_empty() {
        return Err("no selection".into());
    }

    let resolver = SlotResolver::new(engines, bindings);
    let binding = resolver
        .require_tts("tts")
        .await
        .map_err(|e| e.to_string())?;

    let action_id = actions
        .record(NewAction {
            feature_id: "tts".into(),
            command: "read_selection".into(),
            engine_id: binding.engine_id.clone(),
            model: binding
                .model
                .clone()
                .or_else(|| settings.active_model.clone()),
            provider_ref: binding.provider_ref.clone(),
        })
        .await
        .map_err(|e| e.to_string())?;

    // From here the ledger row exists. Synthesis owns it only until it hands
    // the frame back: the caller plays the audio and closes the row.
    let guard = ActionGuard::new(actions, action_id, "tts");
    let synthesis = synthesize(engines, &binding, settings, &text);
    let result = match control {
        Some(control) => {
            tokio::select! {
                r = synthesis => Some(r),
                () = control.stopped() => None,
            }
        }
        None => Some(synthesis.await),
    };
    match result {
        Some(Ok(frame)) => Ok(Synthesis::Ready(guard.release(), frame)),
        Some(Err(e)) => Err(guard.fail(e).await),
        None => {
            guard.cancel(STOPPED_REASON).await;
            Ok(Synthesis::Stopped)
        }
    }
}

/// Synthesize text that did not come from the user's selection.
///
/// The assistant speaks its own answers, so the selection-reading entry points
/// above cannot serve it: there is no selection, and the text to speak was
/// produced a moment ago by a language model.
///
/// `feature_id` is whose `tts` slot to resolve. Passing it rather than
/// hardcoding `"tts"` is what lets the assistant have its own voice binding —
/// a user may well want a different, faster voice for spoken answers than for
/// reading a document aloud.
pub async fn speak_text_for(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    settings: &TtsSettings,
    feature_id: &str,
    text: &str,
) -> Result<PcmFrame, String> {
    if text.trim().is_empty() {
        return Err("nothing to speak".into());
    }
    let binding = SlotResolver::new(engines, bindings)
        .require_tts(feature_id)
        .await
        .map_err(|e| e.to_string())?;
    synthesize(engines, &binding, settings, text).await
}

async fn synthesize(
    engines: &EngineRegistry,
    binding: &Binding,
    settings: &TtsSettings,
    text: &str,
) -> Result<PcmFrame, String> {
    let engine_id = &binding.engine_id;
    let engine = engines
        .tts(engine_id)
        .ok_or_else(|| format!("no tts engine '{engine_id}'"))?;

    let tts_opts = TtsOpts {
        model: binding
            .model
            .clone()
            .or_else(|| settings.active_model.clone()),
        voice: settings.active_voice.clone(),
        format: None,
        provider_ref: binding.provider_ref.clone(),
        speed: Some(settings.speed),
    };

    let pcm = engine
        .synthesize(text, tts_opts)
        .await
        .map_err(|e| e.to_string())?;

    Ok(PcmFrame {
        samples: pcm.samples,
        sample_rate_hz: pcm.sample_rate_hz,
    })
}

/// Synthesizes the current selection, plays it with `play`, and closes the
/// ledger row that synthesis opened.
///
/// Playback is a callback because the two callers reach the speakers by
/// different routes: this crate hands the frame to [`AudioIo`], while the app
/// plays it on a blocking thread so the capture mutex behind its `AudioIo` is
/// not held for the length of the audio. The action lifecycle is the same
/// either way, so it lives here and not at the call sites.
///
/// `play` receives the stop flag to poll and must return soon after it is set.
/// The run is then closed as cancelled rather than ok, and
/// [`TtsOutcome::Stopped`] comes back so the caller can reset its UI.
/// `control` moves to [`ReadAloudPhase::Playing`] just before `play` runs,
/// which is the window the app holds Escape for.
pub async fn run_tts_with_player<F, Fut>(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    settings: &TtsSettings,
    control: &ReadAloudControl,
    play: F,
) -> Result<TtsOutcome, String>
where
    F: FnOnce(PcmFrame, Arc<AtomicBool>) -> Fut,
    Fut: Future<Output = Result<(), String>>,
{
    let (action_id, pcm) = match run_tts_synthesize_stoppable(
        engines,
        bindings,
        actions,
        textio,
        settings,
        Some(control),
    )
    .await?
    {
        Synthesis::Ready(id, pcm) => (id, pcm),
        Synthesis::Stopped => return Ok(TtsOutcome::Stopped),
    };

    let guard = ActionGuard::new(actions, action_id, "tts");
    // A stop that landed as synthesis finished must not start any audio.
    if control.stop_requested() {
        guard.cancel(STOPPED_REASON).await;
        return Ok(TtsOutcome::Stopped);
    }
    control.set_playing();
    match play(pcm, control.stop_flag()).await {
        Ok(()) if control.stop_requested() => {
            guard.cancel(STOPPED_REASON).await;
            Ok(TtsOutcome::Stopped)
        }
        Ok(()) => {
            guard.succeed().await;
            Ok(TtsOutcome::Finished)
        }
        Err(e) => Err(guard.fail(e).await),
    }
}

pub async fn run_tts(
    engines: &EngineRegistry,
    bindings: &BindingRepo,
    actions: &ActionRepo,
    textio: &dyn TextIo,
    audio: &dyn AudioIo,
    settings: &TtsSettings,
) -> Result<(), String> {
    let control = ReadAloudControl::default();
    let _run = control.try_begin();
    run_tts_with_player(
        engines,
        bindings,
        actions,
        textio,
        settings,
        &control,
        |pcm, stop| async move { audio.play_until(pcm, stop).await.map_err(|e| e.to_string()) },
    )
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use kea_core::store::actions::ActionStatus;
    use kea_core::store::db::{open_pool, run_config_migrations, run_data_migrations};
    use kea_engines::noop::NoopTtsEngine;
    use kea_platform::{AudioIoError, DictationState, ReplaceMode, TextIoError};
    use std::sync::{Arc, Mutex};

    struct FakeTextIo {
        selection: String,
    }

    #[async_trait]
    impl TextIo for FakeTextIo {
        async fn capture_selection(&self) -> Result<String, TextIoError> {
            Ok(self.selection.clone())
        }

        async fn replace_with_mode(
            &self,
            _text: &str,
            _mode: ReplaceMode,
        ) -> Result<(), TextIoError> {
            Ok(())
        }
    }

    struct FakePlayAudioIo {
        last_played: Mutex<Option<PcmFrame>>,
    }

    impl FakePlayAudioIo {
        fn last_played(&self) -> Option<PcmFrame> {
            self.last_played.lock().unwrap().clone()
        }
    }

    impl Default for FakePlayAudioIo {
        fn default() -> Self {
            Self {
                last_played: Mutex::new(None),
            }
        }
    }

    #[async_trait]
    impl AudioIo for FakePlayAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }

        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            Ok(PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            })
        }

        fn current_level(&self) -> f32 {
            0.0
        }

        fn state(&self) -> DictationState {
            DictationState::Idle
        }

        async fn play(&self, pcm: PcmFrame) -> Result<(), AudioIoError> {
            *self.last_played.lock().unwrap() = Some(pcm);
            Ok(())
        }
    }

    async fn test_repos() -> (BindingRepo, ActionRepo) {
        let config_pool = open_pool("sqlite::memory:").await.unwrap();
        run_config_migrations(&config_pool).await.unwrap();
        let data_pool = open_pool("sqlite::memory:").await.unwrap();
        run_data_migrations(&data_pool).await.unwrap();
        (BindingRepo::new(config_pool), ActionRepo::new(data_pool))
    }

    #[test]
    fn tts_declares_tts_slot_and_read_selection() {
        let f = TtsFeature;
        assert_eq!(f.id(), "tts");
        assert_eq!(f.required_caps()[0].name, "tts");
        assert_eq!(f.required_caps()[0].kind, CapKind::Tts);
        let cmds = f.commands();
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].id, "read_selection");
        assert_eq!(cmds[0].title, "Read Selection Aloud");
        assert!(cmds[0].default_accelerator.is_some());
    }

    #[tokio::test]
    async fn run_tts_plays_synthesized_audio() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));

        let fake_audio = FakePlayAudioIo::default();
        let fake_text = FakeTextIo {
            selection: "hello world".into(),
        };

        let (bindings, actions) = test_repos().await;
        let settings = TtsSettings::default();

        run_tts(
            &reg,
            &bindings,
            &actions,
            &fake_text,
            &fake_audio,
            &settings,
        )
        .await
        .unwrap();

        let played = fake_audio.last_played().expect("audio should be played");
        assert!(!played.samples.is_empty());
        assert_eq!(played.sample_rate_hz, 24_000);

        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].feature_id, "tts");
        assert_eq!(rows[0].command, "read_selection");
        assert_eq!(rows[0].engine_id, "noop-tts");
        assert_eq!(rows[0].status, ActionStatus::Ok);
    }

    #[tokio::test]
    async fn run_tts_synthesize_returns_pcm_no_audio_needed() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));

        let fake_text = FakeTextIo {
            selection: "hello world".into(),
        };

        let (bindings, actions) = test_repos().await;
        let settings = TtsSettings::default();

        let (action_id, pcm) = run_tts_synthesize(&reg, &bindings, &actions, &fake_text, &settings)
            .await
            .unwrap();

        assert!(!pcm.samples.is_empty());
        assert_eq!(pcm.sample_rate_hz, 24_000);
        assert!(action_id > 0);
    }

    #[tokio::test]
    async fn run_tts_rejects_empty_selection() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));

        let fake_audio = FakePlayAudioIo::default();
        let fake_text = FakeTextIo {
            selection: "   ".into(),
        };

        let (bindings, actions) = test_repos().await;

        let err = run_tts(
            &reg,
            &bindings,
            &actions,
            &fake_text,
            &fake_audio,
            &TtsSettings::default(),
        )
        .await
        .unwrap_err();
        assert_eq!(err, "no selection");
        assert!(fake_audio.last_played().is_none());
    }

    // ---- stop / toggle ---------------------------------------------------

    use kea_engines::traits::{AudioPcm, EngineCaps, EngineError, TtsEngine};
    use std::time::Duration;

    /// Synthesis that never finishes, standing in for a slow cloud request.
    struct HangingTtsEngine;

    #[async_trait]
    impl TtsEngine for HangingTtsEngine {
        fn id(&self) -> &str {
            "noop-tts"
        }
        fn capabilities(&self) -> EngineCaps {
            EngineCaps {
                models: vec!["noop".into()],
            }
        }
        async fn synthesize(&self, _text: &str, _opts: TtsOpts) -> Result<AudioPcm, EngineError> {
            std::future::pending().await
        }
    }

    /// Output that plays until told to stop, like real audio would until its
    /// buffer drains. Records when playback started and when it returned.
    #[derive(Default)]
    struct EndlessAudioIo {
        started: AtomicBool,
        returned: AtomicBool,
    }

    #[async_trait]
    impl AudioIo for EndlessAudioIo {
        async fn start_mic(
            &mut self,
        ) -> Result<tokio::sync::mpsc::Receiver<PcmFrame>, AudioIoError> {
            let (_tx, rx) = tokio::sync::mpsc::channel(1);
            Ok(rx)
        }
        async fn stop_mic(&mut self) -> Result<PcmFrame, AudioIoError> {
            Ok(PcmFrame {
                samples: vec![],
                sample_rate_hz: 16_000,
            })
        }
        fn current_level(&self) -> f32 {
            0.0
        }
        fn state(&self) -> DictationState {
            DictationState::Idle
        }
        async fn play_until(
            &self,
            _pcm: PcmFrame,
            cancel: Arc<AtomicBool>,
        ) -> Result<(), AudioIoError> {
            self.started.store(true, Ordering::SeqCst);
            while !cancel.load(Ordering::SeqCst) {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            self.returned.store(true, Ordering::SeqCst);
            Ok(())
        }
    }

    async fn wait_for(phase: ReadAloudPhase, control: &ReadAloudControl) {
        for _ in 0..1000 {
            if control.phase() == phase {
                return;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        panic!("never reached {phase:?}, stuck in {:?}", control.phase());
    }

    #[test]
    fn the_shortcut_starts_when_idle_and_stops_otherwise() {
        assert_eq!(
            read_aloud_press(ReadAloudPhase::Idle),
            ReadAloudPress::Start
        );
        assert_eq!(
            read_aloud_press(ReadAloudPhase::Synthesizing),
            ReadAloudPress::Stop
        );
        assert_eq!(
            read_aloud_press(ReadAloudPhase::Playing),
            ReadAloudPress::Stop
        );
    }

    #[test]
    fn escape_stops_only_while_playing_and_only_with_the_setting_on() {
        use ReadAloudPhase::*;
        assert!(escape_stops_read_aloud(Playing, true));
        // Synthesis has no audio to silence and Escape is not even registered
        // then; a stray press must not abort it.
        assert!(!escape_stops_read_aloud(Synthesizing, true));
        assert!(!escape_stops_read_aloud(Idle, true));
        // The switch off means Escape is ignored in every phase.
        assert!(!escape_stops_read_aloud(Playing, false));
        assert!(!escape_stops_read_aloud(Synthesizing, false));
        assert!(!escape_stops_read_aloud(Idle, false));
    }

    #[test]
    fn the_control_admits_one_run_and_resets_when_it_ends() {
        let control = ReadAloudControl::default();
        assert_eq!(control.phase(), ReadAloudPhase::Idle);
        assert!(!control.request_stop(), "nothing to stop when idle");

        let run = control.try_begin().expect("idle control admits a run");
        assert_eq!(control.phase(), ReadAloudPhase::Synthesizing);
        assert!(control.try_begin().is_none(), "no second voice on top");

        control.set_playing();
        assert_eq!(control.phase(), ReadAloudPhase::Playing);

        assert!(control.request_stop());
        assert!(control.stop_requested());

        drop(run);
        assert_eq!(control.phase(), ReadAloudPhase::Idle);
        assert!(
            !control.stop_requested(),
            "a stop must not leak into the next run"
        );
        assert!(control.try_begin().is_some());
    }

    #[tokio::test]
    async fn a_stop_requested_before_anyone_waits_is_not_lost() {
        let control = ReadAloudControl::default();
        let _run = control.try_begin().unwrap();
        control.request_stop();
        tokio::time::timeout(Duration::from_secs(1), control.stopped())
            .await
            .expect("stopped() resolves for a stop that landed first");
    }

    #[tokio::test]
    async fn stopping_playback_ends_play_and_closes_the_row_as_stopped() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));
        let audio = Arc::new(EndlessAudioIo::default());
        let text = FakeTextIo {
            selection: "hello".into(),
        };
        let (bindings, actions) = test_repos().await;
        let settings = TtsSettings::default();
        let control = ReadAloudControl::default();
        let _run = control.try_begin().unwrap();

        let run = run_tts_with_player(
            &reg,
            &bindings,
            &actions,
            &text,
            &settings,
            &control,
            |pcm, stop| {
                let audio = Arc::clone(&audio);
                async move { audio.play_until(pcm, stop).await.map_err(|e| e.to_string()) }
            },
        );
        let stopper = async {
            wait_for(ReadAloudPhase::Playing, &control).await;
            assert!(!audio.returned.load(Ordering::SeqCst), "still playing");
            assert!(control.request_stop());
        };
        let (outcome, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, stopper) })
                .await
                .expect("play() must end once stopped, not run to the end of the audio");

        assert_eq!(outcome.unwrap(), TtsOutcome::Stopped);
        assert!(audio.returned.load(Ordering::SeqCst));
        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, ActionStatus::Cancelled);
    }

    #[tokio::test]
    async fn stopping_synthesis_aborts_it_and_closes_the_row_as_stopped() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(HangingTtsEngine));
        let audio = EndlessAudioIo::default();
        let text = FakeTextIo {
            selection: "hello".into(),
        };
        let (bindings, actions) = test_repos().await;
        let settings = TtsSettings::default();
        let control = ReadAloudControl::default();
        let _run = control.try_begin().unwrap();

        let run = run_tts_with_player(
            &reg,
            &bindings,
            &actions,
            &text,
            &settings,
            &control,
            |pcm, stop| async { audio.play_until(pcm, stop).await.map_err(|e| e.to_string()) },
        );
        let stopper = async {
            // Give the run time to reach the engine, then stop mid-synthesis.
            tokio::time::sleep(Duration::from_millis(50)).await;
            assert_eq!(control.phase(), ReadAloudPhase::Synthesizing);
            assert!(control.request_stop());
        };
        let (outcome, ()) =
            tokio::time::timeout(Duration::from_secs(5), async { tokio::join!(run, stopper) })
                .await
                .expect("a stop must abort synthesis that would otherwise never return");

        assert_eq!(outcome.unwrap(), TtsOutcome::Stopped);
        assert!(
            !audio.started.load(Ordering::SeqCst),
            "nothing may play after a stop during synthesis"
        );
        let rows = actions.recent(1).await.unwrap();
        assert_eq!(rows[0].status, ActionStatus::Cancelled);
    }

    #[tokio::test]
    async fn a_finished_read_is_ok_not_stopped() {
        let mut reg = EngineRegistry::default();
        reg.register_tts(Arc::new(NoopTtsEngine));
        let audio = FakePlayAudioIo::default();
        let text = FakeTextIo {
            selection: "hello".into(),
        };
        let (bindings, actions) = test_repos().await;
        let control = ReadAloudControl::default();
        let _run = control.try_begin().unwrap();

        let outcome = run_tts_with_player(
            &reg,
            &bindings,
            &actions,
            &text,
            &TtsSettings::default(),
            &control,
            |pcm, _stop| async { audio.play(pcm).await.map_err(|e| e.to_string()) },
        )
        .await
        .unwrap();

        assert_eq!(outcome, TtsOutcome::Finished);
        assert_eq!(actions.recent(1).await.unwrap()[0].status, ActionStatus::Ok);
    }
}
