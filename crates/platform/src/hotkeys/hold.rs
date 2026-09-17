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
pub const DEFAULT_MIN_HOLD: Duration = Duration::from_millis(350);

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
}

/// What the caller should do about the dictation run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HoldAction {
    Nothing,
    Start,
    Stop,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// The chord is not (fully) held.
    Idle,
    /// The chord is held and still eligible; `since` is when it completed.
    Armed { since: Instant },
    /// The chord is held but has been disqualified by another key. Stays here
    /// until the chord breaks, so the user can keep ⌥⇧ down and press arrow
    /// after arrow without the last one accidentally arming a recording.
    PassedThrough,
    /// A recording is running for this hold.
    Recording,
}

/// Decides when a held ⌥⇧ becomes a recording.
#[derive(Debug)]
pub struct HoldToTalk {
    state: State,
    min_hold: Duration,
}

impl HoldToTalk {
    pub fn new() -> Self {
        Self::with_min_hold(DEFAULT_MIN_HOLD)
    }

    pub fn with_min_hold(min_hold: Duration) -> Self {
        Self {
            state: State::Idle,
            min_hold,
        }
    }

    /// Feed a modifier-flags change.
    pub fn on_modifiers(&mut self, now: Instant, modifiers: HoldModifiers) -> HoldAction {
        if modifiers.is_chord() {
            // Arm only on the transition into the chord. Re-arming on every
            // flags event would restart the clock each time an unrelated
            // modifier (Cmd, Control, Fn) is pressed on top of a held ⌥⇧, and
            // the hold would never reach the threshold.
            if self.state == State::Idle {
                self.state = State::Armed { since: now };
            }
            return HoldAction::Nothing;
        }

        // Anything that is not the full chord ends the hold, which is what
        // makes releasing just one of the two keys enough to stop.
        let was_recording = self.state == State::Recording;
        self.state = State::Idle;
        if was_recording {
            HoldAction::Stop
        } else {
            HoldAction::Nothing
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
        if let State::Armed { .. } = self.state {
            self.state = State::PassedThrough;
        }
        // A keystroke during an established recording is not a reason to drop
        // it: the user may well be typing while dictating into another field.
        HoldAction::Nothing
    }

    /// How long until an armed hold would start, or `None` when nothing is
    /// waiting on the clock. Lets the driver sleep exactly as long as it needs
    /// to instead of polling.
    pub fn time_until_start(&self, now: Instant) -> Option<Duration> {
        match self.state {
            State::Armed { since } => Some(self.min_hold.saturating_sub(now.duration_since(since))),
            _ => None,
        }
    }

    /// Fire the pending start once the hold has lasted long enough. Idempotent:
    /// a second call in the same hold reports `Nothing`.
    pub fn poll(&mut self, now: Instant) -> HoldAction {
        match self.state {
            State::Armed { since } if now.duration_since(since) >= self.min_hold => {
                self.state = State::Recording;
                HoldAction::Start
            }
            _ => HoldAction::Nothing,
        }
    }

    /// Drops any hold in progress, reporting whether a recording was running.
    ///
    /// Used when the mode is switched off, and when the dictation run this hold
    /// started ended by some other route, so the next chord is judged fresh.
    pub fn reset(&mut self) -> HoldAction {
        let was_recording = self.state == State::Recording;
        self.state = State::Idle;
        if was_recording {
            HoldAction::Stop
        } else {
            HoldAction::Nothing
        }
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
        assert_eq!(hold.poll(at(t0, 200)), HoldAction::Nothing, "not yet");
        assert_eq!(hold.poll(at(t0, 380)), HoldAction::Start);
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
    fn time_until_start_lets_the_driver_sleep_exactly_once() {
        let (mut hold, t0) = machine();
        assert_eq!(hold.time_until_start(t0), None, "idle waits on events only");
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        assert_eq!(
            hold.time_until_start(at(t0, 100)),
            Some(Duration::from_millis(250))
        );
        // Past the deadline the remaining time saturates instead of underflowing.
        assert_eq!(hold.time_until_start(at(t0, 500)), Some(Duration::ZERO));
        hold.poll(at(t0, 500));
        assert_eq!(hold.time_until_start(at(t0, 600)), None, "recording now");
    }

    #[test]
    fn reset_reports_a_recording_it_had_to_abandon() {
        let (mut hold, t0) = machine();
        hold.on_modifiers(at(t0, 0), HoldModifiers::BOTH);
        hold.poll(at(t0, 400));
        assert_eq!(hold.reset(), HoldAction::Stop);
        assert_eq!(hold.reset(), HoldAction::Nothing, "already idle");
    }
}
