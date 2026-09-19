//! Press-and-hold ("push-to-talk") hotkey state machine.
//!
//! The accelerator hotkeys in [`super`] go through Carbon's
//! `RegisterEventHotKey`, which cannot express a modifier-only chord and only
//! ever reports a press — the `Toggle push-to-talk` comment on
//! `DictationHotkeyAction` says as much. Hold-to-talk therefore watches raw
//! modifier-flag changes instead (see `macos_hold`), and this module is the
//! part of it that has no OS in it: given a stream of modifier snapshots and a
//! clock, decide when a recording starts and when it stops.
//!
//! Keeping the decision here is what makes the awkward cases testable — a quick
//! pass through ⌥⇧ on the way to another shortcut, ⌥⇧+arrow held for seconds
//! while selecting text by word, and a release of just one of the two keys.
//!
//! Three gestures come out of the same chord, which is why they share one
//! machine rather than three:
//!
//! * **Hold** — both keys down past [`DEFAULT_MIN_HOLD`], recording until one
//!   comes up.
//! * **Preroll** — a single key down past [`ARM_DELAY`] opens the microphone
//!   early so the speech before the hold threshold is not lost. Nothing is
//!   recorded from it unless the hold completes.
//! * **Lock** — two quick taps within [`DOUBLE_TAP_WINDOW`] start a recording
//!   that outlives the keys, ended by the next tap, by Escape, or by
//!   [`DEFAULT_LOCK_MAX`] so a forgotten lock cannot hold the mic open all day.

use std::time::{Duration, Instant};

/// How long both modifiers must stay down, with nothing else pressed, before a
/// recording starts.
///
/// The number is a compromise between two failure modes, both of which were
/// the deciding constraints rather than feel:
///
/// * Too short and ordinary typing starts recordings. Reaching for ⌥⇧← puts
///   both modifiers down for as long as it takes to move a finger to the arrow
///   key — comfortably over 100ms, and longer on a two-hand chord.
/// * Too long and the user is already mid-word when the mic opens, because
///   people start talking as soon as the keys are down.
///
/// 350ms sits above the transit time and below the point where the clipped
/// first syllable becomes the complaint. The key-down cancel below is what
/// actually protects the *held* chords (⌥⇧+arrow can be held for seconds); this
/// threshold only has to cover the gap before the other key arrives.
///
/// The second failure mode is now covered by the preroll rather than by the
/// number: see [`ARM_DELAY`].
pub const DEFAULT_MIN_HOLD: Duration = Duration::from_millis(350);

/// How long a single modifier must stay down before capture is armed.
///
/// Arming opens the microphone, so this is the difference between "the user is
/// reaching for the chord" and "the user pressed Shift to type a capital". A
/// shift keystroke is down for well under 80ms; a chord being assembled is
/// down for longer than that before the second key lands. Well below
/// [`DEFAULT_MIN_HOLD`] on purpose — the whole point is for audio to be
/// flowing before the hold threshold passes, and `cpal` needs roughly 150ms
/// from stream open to the first buffer (`examples/mic_arm_probe.rs`).
pub const ARM_DELAY: Duration = Duration::from_millis(80);

/// How long after a tap a second tap still counts as a double tap.
///
/// Long enough for a deliberate double tap of a two-key chord, which is slower
/// than a mouse double click because both keys have to come up and go down
/// again; short enough that two unrelated brushes past ⌥⇧ do not lock the mic.
pub const DOUBLE_TAP_WINDOW: Duration = Duration::from_millis(400);

/// How long a locked recording may run before it stops itself.
///
/// A lock that the user forgets is an open microphone, and unlike a hold there
/// is no key coming up to end it. Five minutes is past any dictated passage
/// and far short of an accidental all-day recording; the audio captured up to
/// the cap is transcribed rather than discarded, because the user did mean to
/// dictate it.
pub const DEFAULT_LOCK_MAX: Duration = Duration::from_secs(5 * 60);

/// Which of the two watched modifiers are currently down.
///
/// Read from the OS event's flag word rather than tracked key-by-key, so the
/// machine cannot drift out of sync when a key-up is missed (which happens
/// whenever a modifier is released while another app has a modal grab).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HoldModifiers {
    pub option: bool,
    pub shift: bool,
}

impl HoldModifiers {
    pub const NONE: Self = Self {
        option: false,
        shift: false,
    };
    pub const BOTH: Self = Self {
        option: true,
        shift: true,
    };

    /// Only the full chord arms the hold; either key alone is just a modifier.
    fn is_chord(self) -> bool {
        self.option && self.shift
    }

    /// Whether anything at all is still down.
    fn any(self) -> bool {
        self.option || self.shift
    }
}

/// What the caller should do about the dictation run.
///
/// `Arm` and `Disarm` are about the microphone only — they never start or stop
/// a recording, and the audio an armed stream collects is thrown away unless a
/// `Start` follows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldAction {
    Nothing,
    Arm,
    Disarm,
    Start,
    Stop,
    StartLocked,
    StopLocked,
    CancelLocked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing is held.
    Idle,
    /// One of the two modifiers is down. `since` is when it arrived, and
    /// `armed` records whether it has been down long enough to open the mic.
    Pending { since: Instant, armed: bool },
    /// The chord is held and still eligible; `since` is when it completed.
    Chord { since: Instant, armed: bool },
    /// The chord has been spent — disqualified by another key, or already used
    /// to stop a recording. Stays here until *every* watched modifier is up,
    /// so the user can keep ⌥⇧ down and press arrow after arrow without the
    /// last one arming a recording, and so releasing the two keys one at a
    /// time cannot be read as a fresh press of the second.
    PassedThrough,
    /// A recording is running for this hold.
    Recording,
    /// A recording started by a double tap, running with no keys held.
    Locked { since: Instant },
}

/// Decides when a held ⌥⇧ becomes a recording.
#[derive(Debug)]
pub struct HoldToTalk {
    state: State,
    min_hold: Duration,
    lock_max: Duration,
    /// When the chord was last released before reaching the hold threshold.
    /// A second chord within [`DOUBLE_TAP_WINDOW`] of it locks.
    last_tap: Option<Instant>,
}

impl HoldToTalk {
    pub fn new() -> Self {
        Self::with_timings(DEFAULT_MIN_HOLD, DEFAULT_LOCK_MAX)
    }

    pub fn with_min_hold(min_hold: Duration) -> Self {
        Self::with_timings(min_hold, DEFAULT_LOCK_MAX)
    }

    pub fn with_timings(min_hold: Duration, lock_max: Duration) -> Self {
        Self {
            state: State::Idle,
            min_hold,
            lock_max,
            last_tap: None,
        }
    }

    /// Whether a locked recording is running. Read by the app to decide
    /// whether Escape should be listened for at all.
    pub fn is_locked(&self) -> bool {
        matches!(self.state, State::Locked { .. })
    }

    /// Whether a capture stream has been armed and not yet handed over or
    /// dropped. Read by the driver before [`reset`](Self::reset), which cannot
    /// report both the abandoned recording and the abandoned arming.
    pub fn is_armed(&self) -> bool {
        matches!(
            self.state,
            State::Pending { armed: true, .. } | State::Chord { armed: true, .. }
        )
    }

    /// Feed a modifier-flags change.
    pub fn on_modifiers(&mut self, now: Instant, modifiers: HoldModifiers) -> HoldAction {
        if modifiers.is_chord() {
            return self.on_chord_complete(now);
        }
        self.on_chord_incomplete(now, modifiers)
    }

    fn on_chord_complete(&mut self, now: Instant) -> HoldAction {
        match self.state {
            // The gesture that ends a lock is the same one that starts it, so
            // the press is claimed here before anything else can read it.
            State::Locked { .. } => {
                self.state = State::PassedThrough;
                self.last_tap = None;
                HoldAction::StopLocked
            }
            State::Idle => {
                self.state = State::Chord {
                    since: now,
                    armed: false,
                };
                self.lock_on_double_tap(now)
            }
            State::Pending { armed, .. } => {
                // The clock starts when the chord completes, not when the
                // first key went down: the threshold is about how long the
                // *chord* is held. The arming carries over — that stream is
                // already open and already filling the preroll.
                self.state = State::Chord { since: now, armed };
                self.lock_on_double_tap(now)
            }
            // Re-arming on every flags event would restart the clock each time
            // an unrelated modifier (Cmd, Control, Fn) is pressed on top of a
            // held ⌥⇧, and the hold would never reach the threshold.
            _ => HoldAction::Nothing,
        }
    }

    /// Turn the chord's completion into a lock if it closely followed a tap.
    fn lock_on_double_tap(&mut self, now: Instant) -> HoldAction {
        let Some(tap) = self.last_tap else {
            return HoldAction::Nothing;
        };
        if now.duration_since(tap) > DOUBLE_TAP_WINDOW {
            return HoldAction::Nothing;
        }
        // Locking on the second *press* rather than its release: the user who
        // double-tapped is already talking, and waiting for the release would
        // clip the same first syllable the preroll exists to save.
        self.last_tap = None;
        self.state = State::Locked { since: now };
        HoldAction::StartLocked
    }

    fn on_chord_incomplete(&mut self, now: Instant, modifiers: HoldModifiers) -> HoldAction {
        // A recording keeps running with no keys held; only an explicit stop,
        // Escape or the hard cap ends it.
        if let State::Locked { .. } = self.state {
            return HoldAction::Nothing;
        }

        // Half the chord is not the chord, so this is where a hold ends —
        // releasing either key is enough.
        let spent = if modifiers.any() {
            State::PassedThrough
        } else {
            State::Idle
        };

        match std::mem::replace(&mut self.state, spent) {
            State::Idle if modifiers.any() => {
                self.state = State::Pending {
                    since: now,
                    armed: false,
                };
                HoldAction::Nothing
            }
            // Sliding from one modifier to the other without passing through
            // nothing: still a chord being assembled, and the clock it is
            // waiting on is the one already running.
            State::Pending { since, armed } if modifiers.any() => {
                self.state = State::Pending { since, armed };
                HoldAction::Nothing
            }
            State::Pending { armed, .. } => disarm_if(armed),
            State::Chord { since, armed } => {
                // Released before the threshold: nothing was ever started, so
                // nothing stops — but it counts as a tap, and two of those
                // within the window lock.
                if now.duration_since(since) < self.min_hold {
                    self.last_tap = Some(now);
                }
                disarm_if(armed)
            }
            State::Recording => {
                // A hold that ran its course is not half of a double tap.
                self.last_tap = None;
                HoldAction::Stop
            }
            _ => HoldAction::Nothing,
        }
    }

    /// Feed "some non-modifier key went down".
    ///
    /// Deliberately carries no key identity: the only thing the decision needs
    /// is that the chord is being used as a modifier for something else. The
    /// case this exists for is ⌥⇧+arrow — selecting text by word holds both
    /// modifiers for as long as the user keeps selecting, so a duration
    /// threshold alone would start a recording every time.
    pub fn on_other_key(&mut self) -> HoldAction {
        match self.state {
            State::Pending { armed, .. } | State::Chord { armed, .. } => {
                self.state = State::PassedThrough;
                disarm_if(armed)
            }
            // A keystroke during an established recording is not a reason to
            // drop it: the user may well be typing while dictating into
            // another field, and a locked recording is *expected* to outlive
            // whatever else the keyboard is doing.
            _ => HoldAction::Nothing,
        }
    }

    /// Stop a locked recording and throw its audio away. Reported by the
    /// Escape accelerator, which is registered only while a lock is running.
    pub fn cancel_lock(&mut self) -> HoldAction {
        if let State::Locked { .. } = self.state {
            self.state = State::Idle;
            self.last_tap = None;
            return HoldAction::CancelLocked;
        }
        HoldAction::Nothing
    }

    /// How long until this machine has something to say on its own, or `None`
    /// when it is waiting purely on events. Lets the driver sleep exactly as
    /// long as it needs to instead of polling.
    ///
    /// Three deadlines share it: arming a single held modifier, starting a
    /// completed hold, and the hard cap on a locked recording.
    pub fn time_until_deadline(&self, now: Instant) -> Option<Duration> {
        match self.state {
            State::Pending {
                since,
                armed: false,
            } => Some(ARM_DELAY.saturating_sub(now.duration_since(since))),
            State::Chord { since, armed } => {
                let elapsed = now.duration_since(since);
                let until_start = self.min_hold.saturating_sub(elapsed);
                if armed {
                    Some(until_start)
                } else {
                    Some(until_start.min(ARM_DELAY.saturating_sub(elapsed)))
                }
            }
            State::Locked { since } => {
                Some(self.lock_max.saturating_sub(now.duration_since(since)))
            }
            _ => None,
        }
    }

    /// Fire whichever deadline has passed. Idempotent: a second call in the
    /// same hold reports `Nothing`.
    pub fn poll(&mut self, now: Instant) -> HoldAction {
        match self.state {
            State::Pending {
                since,
                armed: false,
            } if now.duration_since(since) >= ARM_DELAY => {
                self.state = State::Pending { since, armed: true };
                HoldAction::Arm
            }
            State::Chord { since, .. } if now.duration_since(since) >= self.min_hold => {
                self.state = State::Recording;
                self.last_tap = None;
                HoldAction::Start
            }
            // Both keys landed at once, so no `Pending` ever armed. There is
            // still most of the hold threshold left to capture.
            State::Chord {
                since,
                armed: false,
            } if now.duration_since(since) >= ARM_DELAY => {
                self.state = State::Chord { since, armed: true };
                HoldAction::Arm
            }
            State::Locked { since } if now.duration_since(since) >= self.lock_max => {
                self.state = State::Idle;
                HoldAction::StopLocked
            }
            _ => HoldAction::Nothing,
        }
    }

    /// Drops any hold in progress, reporting whether a recording was running.
    ///
    /// Used when the mode is switched off, and when the dictation run this hold
    /// started ended by some other route, so the next chord is judged fresh.
    /// A lock is a recording for this purpose — switching the mode off must not
    /// strand one with no key left that can stop it.
    ///
    /// It cannot also report an abandoned *arming*; the driver reads
    /// [`is_armed`](Self::is_armed) first for that.
    pub fn reset(&mut self) -> HoldAction {
        let previous = std::mem::replace(&mut self.state, State::Idle);
        self.last_tap = None;
        match previous {
            State::Recording => HoldAction::Stop,
            State::Locked { .. } => HoldAction::StopLocked,
            _ => HoldAction::Nothing,
        }
    }
}

/// The armed stream has to be closed when the gesture it was opened for falls
/// apart, and left alone when there was none.
fn disarm_if(armed: bool) -> HoldAction {
    if armed {
        HoldAction::Disarm
    } else {
        HoldAction::Nothing
    }
}

impl Default for HoldToTalk {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OPTION_ONLY: HoldModifiers = HoldModifiers {
        option: true,
        shift: false,
    };
    const SHIFT_ONLY: HoldModifiers = HoldModifiers {
        option: false,
        shift: true,
    };

    /// Times are supplied, never slept: the machine's whole job is a duration
    /// comparison, and a test that slept would be slow and flaky for no gain.
    fn at(base: Instant, ms: u64) -> Instant {
        base + Duration::from_millis(ms)
    }

    fn machine() -> (HoldToTalk, Instant) {
        (
            HoldToTalk::with_min_hold(Duration::from_millis(350)),
            Instant::now(),
        )
    }

    /// The chord down and up again, as the hardware reports it: two separate
    /// flag events on the way down and two on the way up.
    fn tap(hold: &mut HoldToTalk, base: Instant, down_ms: u64, up_ms: u64) -> Vec<HoldAction> {
        vec![
            hold.on_modifiers(at(base, down_ms), SHIFT_ONLY),
            hold.on_modifiers(at(base, down_ms + 10), HoldModifiers::BOTH),
            hold.on_modifiers(at(base, up_ms), SHIFT_ONLY),
            hold.on_modifiers(at(base, up_ms + 10), HoldModifiers::NONE),
        ]
    }

    #[test]
    fn holding_both_modifiers_past_the_threshold_starts_recording() {
        let (mut hold, t0) = machine();
        // Real hardware reports the two keys separately, never as one event.
        assert_eq!(
            hold.on_modifiers(at(t0, 0), OPTION_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 30), HoldModifiers::BOTH),
            HoldAction::Nothing
        );
        assert_eq!(hold.poll(at(t0, 200)), HoldAction::Arm, "mic opens early");
        assert_eq!(hold.poll(at(t0, 300)), HoldAction::Nothing, "not yet");
        assert_eq!(hold.poll(at(t0, 400)), HoldAction::Start);
        // The start is reported once, not on every later poll.
        assert_eq!(hold.poll(at(t0, 600)), HoldAction::Nothing);
    }

    #[test]
    fn a_quick_tap_never_starts_a_recording() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(
            hold.on_modifiers(at(t0, 80), HoldModifiers::NONE),
            HoldAction::Nothing,
            "released before the threshold: nothing was ever started, so nothing stops"
        );
        // Even a poll well past the threshold must not resurrect the tap.
        assert_eq!(hold.poll(at(t0, 900)), HoldAction::Nothing);
    }

    #[test]
    fn releasing_both_modifiers_stops_the_recording() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 400)), HoldAction::Start);
        assert_eq!(
            hold.on_modifiers(at(t0, 2_000), HoldModifiers::NONE),
            HoldAction::Stop
        );
        assert_eq!(hold.poll(at(t0, 2_500)), HoldAction::Nothing);
    }

    #[test]
    fn releasing_one_modifier_stops_the_recording() {
        for remaining in [OPTION_ONLY, SHIFT_ONLY] {
            let (mut hold, t0) = machine();
            hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
            assert_eq!(hold.poll(at(t0, 400)), HoldAction::Start);
            assert_eq!(
                hold.on_modifiers(at(t0, 1_200), remaining),
                HoldAction::Stop,
                "half the chord is not the chord: {remaining:?}"
            );
            // Still holding the other key must not re-arm behind the stop.
            assert_eq!(hold.poll(at(t0, 3_000)), HoldAction::Nothing);
        }
    }

    #[test]
    fn a_chord_used_as_a_modifier_never_starts_a_recording() {
        // ⌥⇧← to select the previous word, held down for two seconds while the
        // user keeps selecting. Far past the threshold, and must stay silent.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(hold.on_other_key(), HoldAction::Nothing);
        assert_eq!(hold.poll(at(t0, 2_000)), HoldAction::Nothing);
        // More arrows arrive; still nothing.
        hold.on_other_key();
        assert_eq!(hold.poll(at(t0, 4_000)), HoldAction::Nothing);
        // Only letting the chord go re-qualifies the next one.
        hold.on_modifiers(at(t0, 4_100), HoldModifiers::NONE);
        hold.on_modifiers(at(t0, 5_000), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 5_400)), HoldAction::Start);
    }

    #[test]
    fn releasing_a_spent_chord_one_key_at_a_time_does_not_re_arm() {
        // ⌥⇧← then ⇧ up, ⌥ still down. The lone ⌥ looks exactly like the start
        // of a fresh chord, and reading it as one would open the mic on the way
        // out of somebody else's shortcut.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        hold.on_other_key();
        assert_eq!(
            hold.on_modifiers(at(t0, 500), OPTION_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(hold.poll(at(t0, 2_000)), HoldAction::Nothing);
        assert_eq!(
            hold.on_modifiers(at(t0, 2_100), HoldModifiers::NONE),
            HoldAction::Nothing
        );
    }

    #[test]
    fn a_key_pressed_during_a_recording_does_not_end_it() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 400)), HoldAction::Start);
        assert_eq!(hold.on_other_key(), HoldAction::Nothing);
        assert_eq!(
            hold.on_modifiers(at(t0, 900), HoldModifiers::NONE),
            HoldAction::Stop,
            "the release is still what ends it"
        );
    }

    #[test]
    fn an_extra_modifier_does_not_restart_the_clock() {
        // Cmd pressed on top of a held ⌥⇧ produces another flags event with the
        // chord still complete; re-arming there would push the start out
        // forever for anyone who rests a thumb on Cmd.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        hold.on_modifiers(at(t0, 300), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 360)), HoldAction::Start);
    }

    #[test]
    fn time_until_deadline_lets_the_driver_sleep_exactly_once() {
        let (mut hold, t0) = machine();
        assert_eq!(
            hold.time_until_deadline(t0),
            None,
            "idle waits on events only"
        );
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        // Arming comes first, so that is what the driver sleeps for.
        assert_eq!(
            hold.time_until_deadline(at(t0, 20)),
            Some(Duration::from_millis(60))
        );
        hold.poll(at(t0, 80));
        assert_eq!(
            hold.time_until_deadline(at(t0, 100)),
            Some(Duration::from_millis(250)),
            "armed: only the start is still pending"
        );
        // Past the deadline the remaining time saturates instead of underflowing.
        assert_eq!(hold.time_until_deadline(at(t0, 500)), Some(Duration::ZERO));
        hold.poll(at(t0, 500));
        assert_eq!(hold.time_until_deadline(at(t0, 600)), None, "recording now");
    }

    #[test]
    fn reset_reports_a_recording_it_had_to_abandon() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        hold.poll(at(t0, 400));
        assert_eq!(hold.reset(), HoldAction::Stop);
        assert_eq!(hold.reset(), HoldAction::Nothing, "already idle");
    }

    // --- preroll arming ---------------------------------------------------

    #[test]
    fn one_held_modifier_arms_capture_before_the_chord_completes() {
        // The whole point: audio is already flowing when the threshold passes,
        // so the first syllable is in the ring rather than lost.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), SHIFT_ONLY);
        assert_eq!(hold.poll(at(t0, 40)), HoldAction::Nothing, "too soon");
        assert!(!hold.is_armed());
        assert_eq!(hold.poll(at(t0, 90)), HoldAction::Arm);
        assert!(hold.is_armed());
        assert_eq!(
            hold.poll(at(t0, 200)),
            HoldAction::Nothing,
            "a re-arm while still armed opens a second stream on one device"
        );
    }

    #[test]
    fn a_shift_keystroke_is_too_short_to_arm() {
        // Typing a capital letter: ⇧ down, a letter, ⇧ up. The microphone must
        // not open for it, which is the entire reason arming waits 80ms.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), SHIFT_ONLY);
        assert_eq!(hold.poll(at(t0, 40)), HoldAction::Nothing);
        assert_eq!(hold.on_other_key(), HoldAction::Nothing);
        assert_eq!(hold.poll(at(t0, 500)), HoldAction::Nothing);
        assert_eq!(
            hold.on_modifiers(at(t0, 520), HoldModifiers::NONE),
            HoldAction::Nothing
        );
    }

    #[test]
    fn an_armed_chord_used_as_a_modifier_disarms_without_recording() {
        // ⌥ held long enough to arm, then ⌥⇧←. The stream has to close, and
        // the audio it collected is never transcribed.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), OPTION_ONLY);
        assert_eq!(hold.poll(at(t0, 100)), HoldAction::Arm);
        hold.on_modifiers(at(t0, 120), HoldModifiers::BOTH);
        assert_eq!(hold.on_other_key(), HoldAction::Disarm);
        assert_eq!(hold.poll(at(t0, 2_000)), HoldAction::Nothing);
    }

    #[test]
    fn a_chord_that_never_completes_disarms_on_release() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), OPTION_ONLY);
        assert_eq!(hold.poll(at(t0, 100)), HoldAction::Arm);
        assert_eq!(
            hold.on_modifiers(at(t0, 300), HoldModifiers::NONE),
            HoldAction::Disarm
        );
        assert!(!hold.is_armed());
    }

    #[test]
    fn an_armed_chord_released_before_the_threshold_disarms() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), OPTION_ONLY);
        hold.poll(at(t0, 100));
        hold.on_modifiers(at(t0, 120), HoldModifiers::BOTH);
        assert_eq!(
            hold.on_modifiers(at(t0, 200), OPTION_ONLY),
            HoldAction::Disarm
        );
    }

    #[test]
    fn a_recording_inherits_the_armed_stream_rather_than_re_arming() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), OPTION_ONLY);
        assert_eq!(hold.poll(at(t0, 90)), HoldAction::Arm);
        hold.on_modifiers(at(t0, 120), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 300)), HoldAction::Nothing, "chord clock");
        assert_eq!(hold.poll(at(t0, 480)), HoldAction::Start);
        assert!(!hold.is_armed(), "the stream belongs to the recording now");
        assert_eq!(
            hold.on_modifiers(at(t0, 900), HoldModifiers::NONE),
            HoldAction::Stop,
            "one stream, closed once"
        );
    }

    #[test]
    fn a_chord_pressed_in_one_event_still_arms_before_it_starts() {
        // Both flags arriving together leaves no Pending to arm, and there is
        // still 270ms of speech to save.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 90)), HoldAction::Arm);
        assert_eq!(hold.poll(at(t0, 400)), HoldAction::Start);
    }

    #[test]
    fn switching_the_mode_off_mid_arm_is_visible_to_the_driver() {
        // `reset` can only report one thing, and it reports the recording it
        // abandoned — so the driver has to ask about the arming separately.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), OPTION_ONLY);
        hold.poll(at(t0, 100));
        assert!(hold.is_armed());
        assert_eq!(hold.reset(), HoldAction::Nothing);
        assert!(!hold.is_armed());
    }

    // --- double-tap lock --------------------------------------------------

    #[test]
    fn two_quick_taps_lock_the_recording() {
        let (mut hold, t0) = machine();
        assert!(tap(&mut hold, t0, 0, 100)
            .iter()
            .all(|a| *a == HoldAction::Nothing));
        // The lock lands on the second press, not its release: the user is
        // already talking by then.
        assert_eq!(
            hold.on_modifiers(at(t0, 300), SHIFT_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 310), HoldModifiers::BOTH),
            HoldAction::StartLocked
        );
        // Letting go changes nothing — that is the whole feature.
        assert_eq!(
            hold.on_modifiers(at(t0, 380), SHIFT_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 390), HoldModifiers::NONE),
            HoldAction::Nothing
        );
        assert_eq!(hold.poll(at(t0, 5_000)), HoldAction::Nothing);
        assert!(hold.is_locked());
    }

    #[test]
    fn a_slow_second_tap_does_not_lock() {
        let (mut hold, t0) = machine();
        tap(&mut hold, t0, 0, 100);
        // 400ms after the first tap's release is the edge; this is past it.
        assert_eq!(
            hold.on_modifiers(at(t0, 700), SHIFT_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 710), HoldModifiers::BOTH),
            HoldAction::Nothing
        );
        assert!(!hold.is_locked());
        // And it behaves as an ordinary hold from there.
        assert_eq!(hold.poll(at(t0, 1_100)), HoldAction::Start);
    }

    #[test]
    fn a_completed_hold_does_not_feed_a_double_tap() {
        // Hold, talk, release — then a single tap. Two gestures, not two taps.
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(hold.poll(at(t0, 400)), HoldAction::Start);
        assert_eq!(
            hold.on_modifiers(at(t0, 1_000), HoldModifiers::NONE),
            HoldAction::Stop
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 1_100), SHIFT_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 1_110), HoldModifiers::BOTH),
            HoldAction::Nothing,
            "a hold that ran its course is not half of a double tap"
        );
    }

    #[test]
    fn a_tap_while_locked_stops_and_transcribes() {
        let (mut hold, t0) = machine();
        tap(&mut hold, t0, 0, 100);
        hold.on_modifiers(at(t0, 300), SHIFT_ONLY);
        assert_eq!(
            hold.on_modifiers(at(t0, 310), HoldModifiers::BOTH),
            HoldAction::StartLocked
        );
        hold.on_modifiers(at(t0, 400), HoldModifiers::NONE);

        hold.on_modifiers(at(t0, 9_000), SHIFT_ONLY);
        assert_eq!(
            hold.on_modifiers(at(t0, 9_010), HoldModifiers::BOTH),
            HoldAction::StopLocked
        );
        assert!(!hold.is_locked());
        // The keys coming up after the stopping tap must not start anything.
        assert_eq!(
            hold.on_modifiers(at(t0, 9_100), SHIFT_ONLY),
            HoldAction::Nothing
        );
        assert_eq!(
            hold.on_modifiers(at(t0, 9_110), HoldModifiers::NONE),
            HoldAction::Nothing
        );
        assert_eq!(hold.poll(at(t0, 9_500)), HoldAction::Nothing);
    }

    #[test]
    fn escape_cancels_a_lock_and_nothing_else() {
        let (mut hold, t0) = machine();
        assert_eq!(
            hold.cancel_lock(),
            HoldAction::Nothing,
            "no lock, no cancel"
        );

        tap(&mut hold, t0, 0, 100);
        hold.on_modifiers(at(t0, 300), SHIFT_ONLY);
        hold.on_modifiers(at(t0, 310), HoldModifiers::BOTH);
        hold.on_modifiers(at(t0, 400), HoldModifiers::NONE);

        assert_eq!(hold.cancel_lock(), HoldAction::CancelLocked);
        assert!(!hold.is_locked());
        assert_eq!(hold.cancel_lock(), HoldAction::Nothing, "once only");
    }

    #[test]
    fn a_forgotten_lock_stops_itself_at_the_hard_cap() {
        // An open microphone nobody remembers is the failure this guards.
        let mut hold =
            HoldToTalk::with_timings(Duration::from_millis(350), Duration::from_secs(60));
        let t0 = Instant::now();
        tap(&mut hold, t0, 0, 100);
        hold.on_modifiers(at(t0, 300), SHIFT_ONLY);
        assert_eq!(
            hold.on_modifiers(at(t0, 310), HoldModifiers::BOTH),
            HoldAction::StartLocked
        );

        assert_eq!(
            hold.time_until_deadline(at(t0, 1_310)),
            Some(Duration::from_secs(59))
        );
        assert_eq!(hold.poll(at(t0, 30_000)), HoldAction::Nothing);
        assert_eq!(
            hold.poll(at(t0, 60_310)),
            HoldAction::StopLocked,
            "transcribed, not discarded: the user did mean to dictate it"
        );
        assert_eq!(hold.poll(at(t0, 90_000)), HoldAction::Nothing);
    }

    #[test]
    fn typing_during_a_lock_does_not_end_it() {
        let (mut hold, t0) = machine();
        tap(&mut hold, t0, 0, 100);
        hold.on_modifiers(at(t0, 300), SHIFT_ONLY);
        hold.on_modifiers(at(t0, 310), HoldModifiers::BOTH);
        hold.on_modifiers(at(t0, 400), HoldModifiers::NONE);

        assert_eq!(hold.on_other_key(), HoldAction::Nothing);
        assert!(hold.is_locked());
    }

    #[test]
    fn reset_clears_a_lock_as_well_as_a_hold() {
        // Switching the mode off, or a run that ended by another route, must
        // not leave a lock with no recording behind it.
        let (mut hold, t0) = machine();
        tap(&mut hold, t0, 0, 100);
        hold.on_modifiers(at(t0, 300), SHIFT_ONLY);
        hold.on_modifiers(at(t0, 310), HoldModifiers::BOTH);

        assert_eq!(hold.reset(), HoldAction::StopLocked);
        assert!(!hold.is_locked());
        assert_eq!(hold.reset(), HoldAction::Nothing);
    }
}
