//! When to run an interim notes pass, and how much it is allowed to cost.
//!
//! Pure arithmetic over a config and two counters, so the decision that spends
//! the user's money is testable without a clock, a provider or a meeting.
//!
//! # How the bill is bounded
//!
//! Three independent limits, and the smallest one always wins:
//!
//! 1. **The fold.** Each pass sends the previous notes plus only the segments
//!    since the last pass, so one pass of a three-hour meeting costs the same
//!    as one pass of a ten-minute one. Cost is linear in duration, not
//!    quadratic (see `build_interim_notes_request`).
//! 2. **A floor between passes.** [`MIN_INTERIM_GAP_SECS`] is a hard 90-second
//!    minimum regardless of what the segment trigger says, so a burst of short
//!    segments cannot fire a pass per segment.
//! 3. **A ceiling per meeting.** [`InterimCadence::max_passes`] stops the
//!    schedule entirely once it is reached. A meeting left running overnight
//!    stops billing; it does not keep paying every five minutes until someone
//!    notices. The final full-transcript pass at stop still runs, so the notes
//!    the user keeps are complete either way.

use serde::{Deserialize, Serialize};

/// The hard floor between two passes, whatever the segment trigger says.
pub const MIN_INTERIM_GAP_SECS: u64 = 90;

/// Consecutive interim failures after which the schedule gives up for the rest
/// of the meeting.
///
/// A provider that has refused twice in a row is not going to start working
/// halfway through, and an interim pass is optional: continuing to retry it
/// spends money and log space on something the user will get at stop anyway.
pub const MAX_CONSECUTIVE_INTERIM_FAILURES: u32 = 2;

/// What the user configured, plus the ceiling they did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InterimCadence {
    /// Fire once this many new segments have accumulated.
    pub every_segments: u32,
    /// …or once this many minutes have elapsed, whichever comes first.
    pub every_minutes: u32,
    /// Stop scheduling after this many successful passes in one meeting.
    pub max_passes: u32,
}

impl Default for InterimCadence {
    fn default() -> Self {
        Self {
            every_segments: 8,
            every_minutes: 5,
            // Two hours of a five-minute cadence. Past that a meeting is
            // almost certainly one somebody forgot to stop.
            max_passes: 24,
        }
    }
}

/// Whether an interim pass is due.
///
/// `segments_since` and `secs_since` are counted from the last *successful*
/// pass, so a pass that failed does not reset the clock — the next attempt
/// happens on schedule rather than a whole cadence later.
pub fn should_run_interim(
    segments_since: u32,
    secs_since: u64,
    passes_so_far: u32,
    cfg: &InterimCadence,
) -> bool {
    if passes_so_far >= cfg.max_passes {
        return false;
    }
    // Nothing new to fold in: a pass here would re-ask the same question and
    // pay for the same answer.
    if segments_since == 0 {
        return false;
    }
    if secs_since < MIN_INTERIM_GAP_SECS {
        return false;
    }
    let by_segments = cfg.every_segments > 0 && segments_since >= cfg.every_segments;
    let by_time = cfg.every_minutes > 0 && secs_since >= cfg.every_minutes as u64 * 60;
    by_segments || by_time
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> InterimCadence {
        InterimCadence::default()
    }

    #[test]
    fn the_segment_trigger_fires_on_its_own() {
        // 8 segments, only 2 minutes in: under the time trigger, over the floor.
        assert!(should_run_interim(8, 120, 0, &cfg()));
    }

    #[test]
    fn the_time_trigger_fires_on_its_own() {
        // One segment in five minutes — a quiet meeting still gets notes.
        assert!(should_run_interim(1, 300, 0, &cfg()));
    }

    #[test]
    fn both_triggers_at_once_is_still_one_pass_worth_of_yes() {
        assert!(should_run_interim(20, 600, 0, &cfg()));
    }

    /// The floor is the answer to a burst of very short segments: eight of
    /// them inside a minute must not buy eight completions.
    #[test]
    fn the_ninety_second_floor_outranks_the_segment_trigger() {
        assert!(!should_run_interim(8, 89, 0, &cfg()));
        assert!(should_run_interim(8, 90, 0, &cfg()));
    }

    #[test]
    fn nothing_new_is_never_worth_a_pass() {
        assert!(!should_run_interim(0, 3_600, 0, &cfg()));
    }

    /// The ceiling is what stops a meeting nobody stopped from billing all
    /// night.
    #[test]
    fn the_per_meeting_ceiling_ends_the_schedule() {
        let cfg = cfg();
        assert!(should_run_interim(50, 3_600, cfg.max_passes - 1, &cfg));
        assert!(!should_run_interim(50, 3_600, cfg.max_passes, &cfg));
    }

    /// A zero in either setting turns that trigger off rather than making it
    /// fire on every tick, which is what `>= 0` would have done.
    #[test]
    fn a_zero_setting_disables_that_trigger_rather_than_always_firing() {
        let only_time = InterimCadence {
            every_segments: 0,
            ..cfg()
        };
        assert!(!should_run_interim(100, 120, 0, &only_time));
        assert!(should_run_interim(100, 300, 0, &only_time));

        let neither = InterimCadence {
            every_segments: 0,
            every_minutes: 0,
            ..cfg()
        };
        assert!(!should_run_interim(100, 36_000, 0, &neither));
    }
}
