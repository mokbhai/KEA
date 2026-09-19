//! Read-only calendar access, and the rule for matching a recording to an event.
//!
//! Sibling to `permissions/`, `screen/`, `textio/`, `audio/` and `hotkeys/`: a
//! trait, a cfg-selected constructor, a macOS implementation and a non-macOS
//! stub.
//!
//! # Privacy
//!
//! Calendar entries routinely contain things the user has not chosen to share
//! — "1:1 — performance review", "Interview: <candidate>", "Oncology
//! follow-up". The rule this module exists to enforce:
//!
//! > Calendar data is read locally, used locally, and never placed in any text
//! > sent to an engine. Only the matched event's title is persisted, only on
//! > the meeting it matched, and only until that meeting is deleted.
//!
//! Three things make that structural rather than a convention somebody has to
//! remember:
//!
//! * [`CalendarEvent`] has no field for a body, a location, an attendee list,
//!   a URL or a calendar name. There is nothing to leak because nothing else
//!   is read. [`CalendarEvent::has_attendees`] is derived from a *count*, not
//!   from names.
//! * [`CalendarIo::events_between`] takes a window. Callers read
//!   `[start − 10 min, start + 10 min]`, never a day and never the calendar.
//! * The title is applied at the *title* step of the meeting stop, after notes
//!   synthesis has already run against `"Untitled Meeting"`. The notes prompt
//!   embeds the meeting title verbatim, so applying a calendar title any
//!   earlier would transmit it to whatever hosted provider is bound.
//!
//! # Why the matcher lives here and not in `kea-core`
//!
//! The plan put [`pick_event`] in `crates/core/src/meetings/calendar.rs` over a
//! [`CalendarEvent`] defined in this crate. That cannot compile: `kea-core` does
//! not depend on `kea-platform`, and the edge that would make it work points
//! the wrong way — `kea-platform` is the OS leaf, and `kea-core` owns sqlx, the
//! keychain and the engine traits. The function is pure and OS-free either way,
//! it is fully tested below with hand-written event lists, and `kea-features`
//! (which depends on both) is the caller in both designs.

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(not(target_os = "macos"))]
pub mod stub;

/// Everything KEA reads about a calendar event, and deliberately no more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub title: String,
    /// RFC 3339 / ISO 8601, as the OS reported it.
    pub start_rfc3339: String,
    pub end_rfc3339: String,
    pub is_all_day: bool,
    /// Whether anybody else is on it — a count turned into a boolean, never
    /// the names behind it.
    pub has_attendees: bool,
    /// Whether this user declined, or the organizer cancelled.
    pub declined: bool,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum CalendarError {
    #[error("calendar access is not granted")]
    NotAuthorized,
    #[error("calendar access is not available on this platform")]
    Unavailable,
    #[error("{0}")]
    Other(String),
}

/// Read-only event access over a bounded window.
pub trait CalendarIo: Send + Sync {
    /// Events intersecting `[start, end]`, both RFC 3339.
    ///
    /// Synchronous and expected to be called off the hot path: EventKit's first
    /// access on a machine with a large store can take a moment, and the one
    /// caller runs it at meeting stop behind a timeout.
    fn events_between(&self, start: &str, end: &str) -> Result<Vec<CalendarEvent>, CalendarError>;
}

/// Construct the active platform [`CalendarIo`] implementation for this OS.
pub fn new_calendar_io() -> Box<dyn CalendarIo> {
    #[cfg(target_os = "macos")]
    {
        Box::new(macos::MacCalendar::new())
    }
    #[cfg(not(target_os = "macos"))]
    {
        Box::new(stub::StubCalendar::new())
    }
}

/// How far either side of the recording start an event may begin and still be
/// considered. Recording almost always starts a beat after the call does.
pub const CANDIDATE_WINDOW_SECS: i64 = 10 * 60;

/// How far the best candidate's start may be from the recording start before
/// the match is rejected outright.
///
/// A wrong title is worse than a generated one: it is confidently wrong, it
/// will be searched for, and nobody re-reads a title to catch it.
pub const MAX_MATCH_DRIFT_SECS: i64 = 15 * 60;

/// Seconds since the Unix epoch for an RFC 3339 timestamp, or `None`.
///
/// Hand-rolled rather than pulling in a date crate for one parse. It accepts
/// exactly the two shapes that reach it — `2026-09-19T10:00:00Z` from EventKit
/// and `2026-09-19 10:00:00` from SQLite's `datetime('now')` — and treats the
/// second as UTC, which is what SQLite wrote.
///
/// Returning `Option` rather than guessing is what makes an unparsable stamp
/// fall through to the LLM title instead of matching the wrong event.
pub fn epoch_secs(stamp: &str) -> Option<i64> {
    let bytes = stamp.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let num = |from: usize, to: usize| stamp.get(from..to)?.parse::<i64>().ok();
    let (y, mo, d) = (num(0, 4)?, num(5, 7)?, num(8, 10)?);
    let (h, mi, sec) = (num(11, 13)?, num(14, 16)?, num(17, 19)?);
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    if h > 23 || mi > 59 || sec > 60 {
        return None;
    }

    // Howard Hinnant's days_from_civil: exact, integer-only, and valid across
    // the whole proleptic Gregorian calendar.
    let y = if mo <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;

    let mut secs = days * 86_400 + h * 3_600 + mi * 60 + sec;

    // A trailing offset, if there is one: `+05:30` / `-08:00`. `Z` and a bare
    // stamp are both UTC already.
    if let Some(rest) = stamp.get(19..) {
        let rest = rest.trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
        let sign = match rest.as_bytes().first() {
            Some(b'+') => 1,
            Some(b'-') => -1,
            _ => 0,
        };
        if sign != 0 && rest.len() >= 6 {
            let oh = rest.get(1..3)?.parse::<i64>().ok()?;
            let om = rest.get(4..6)?.parse::<i64>().ok()?;
            secs -= sign * (oh * 3_600 + om * 60);
        }
    }

    Some(secs)
}

/// The calendar event a recording most likely belongs to, or `None`.
///
/// Rules, in order:
///
/// 1. **All-day events are discarded entirely.** An all-day "Sam — PTO" or
///    "Sprint 42" overlaps every recording that day and would win on any
///    overlap-based score. They are not meetings and must never supply a
///    title. This is the rule an overlap-scoring implementation gets wrong.
/// 2. Declined and cancelled events are discarded.
/// 3. Candidates are events whose interval intersects
///    `[started_at − 10 min, started_at + 10 min]`.
/// 4. Score is `|event.start − meeting.start|`, ascending.
/// 5. Ties break on: has attendees before solo events (a real meeting over a
///    self-scheduled block), then shorter duration (a 30-minute standup over
///    an eight-hour "Focus time" block), then earliest start, then event id —
///    never on the calendar's own order, which is not stable.
/// 6. A best candidate more than 15 minutes from the recording start is
///    rejected.
///
/// Overlapping events are therefore resolved, not refused: two back-to-back
/// calls where one ran over is the normal case, and rule 4 picks the one that
/// actually started when recording did.
///
/// `ended_at` is accepted for the signature the plan specifies and for the
/// intersection test; the score deliberately ignores it, because a user who
/// stops recording early must still get the title of the call they were on.
pub fn pick_event<'a>(
    started_at: &str,
    ended_at: Option<&str>,
    candidates: &'a [CalendarEvent],
) -> Option<&'a CalendarEvent> {
    let start = epoch_secs(started_at)?;
    let end = ended_at.and_then(epoch_secs).unwrap_or(start).max(start);

    let window_from = start - CANDIDATE_WINDOW_SECS;
    let window_to = (start + CANDIDATE_WINDOW_SECS).max(end);

    let mut best: Option<(&CalendarEvent, i64, i64)> = None;

    for event in candidates {
        if event.is_all_day || event.declined {
            continue;
        }
        let Some(ev_start) = epoch_secs(&event.start_rfc3339) else {
            continue;
        };
        let ev_end = epoch_secs(&event.end_rfc3339)
            .unwrap_or(ev_start)
            .max(ev_start);
        if ev_end < window_from || ev_start > window_to {
            continue;
        }

        let drift = (ev_start - start).abs();
        if drift > MAX_MATCH_DRIFT_SECS {
            continue;
        }
        let duration = ev_end - ev_start;

        let better = match best {
            None => true,
            Some((current, best_drift, best_duration)) => (
                drift,
                !event.has_attendees,
                duration,
                ev_start,
                event.id.as_str(),
            )
                .lt(&(
                    best_drift,
                    !current.has_attendees,
                    best_duration,
                    epoch_secs(&current.start_rfc3339).unwrap_or(i64::MAX),
                    current.id.as_str(),
                )),
        };
        if better {
            best = Some((event, drift, duration));
        }
    }

    best.map(|(event, _, _)| event)
}

/// Seconds since the epoch as `YYYY-MM-DDTHH:MM:SSZ`.
///
/// The exact inverse of [`epoch_secs`], and hand-rolled for the same reason:
/// one formatting job does not justify a date crate, and the only consumer is
/// the window handed straight to [`CalendarIo::events_between`].
pub fn rfc3339_utc(epoch_secs: i64) -> String {
    let days = epoch_secs.div_euclid(86_400);
    let secs = epoch_secs.rem_euclid(86_400);

    // Howard Hinnant's civil_from_days.
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
        secs / 3_600,
        (secs % 3_600) / 60,
        secs % 60
    )
}

/// The title of the calendar event a recording happened during, if there is
/// one worth using.
///
/// The whole calendar rule in one place: compute the window, read only that
/// window, match with [`pick_event`], and hand back nothing but a title.
///
/// Every failure — unparsable start, permission denied, EventKit error, empty
/// calendar, no match above the threshold — is `None`, which is the caller's
/// signal to fall back to the generated title. Nothing here returns an error,
/// because there is no failure mode a caller could usefully act on: a meeting
/// must not be marked as failed because the calendar was unavailable.
///
/// The window is `[start − 10 min, start + 10 min]` and nothing wider. Reading
/// the day or the calendar would pull in events with no bearing on this
/// recording, which is both a worse match and more of someone's calendar than
/// this feature needs.
pub fn title_for_recording(
    io: &dyn CalendarIo,
    started_at: &str,
    ended_at: Option<&str>,
) -> Option<String> {
    let started = epoch_secs(started_at)?;
    let from = rfc3339_utc(started - CANDIDATE_WINDOW_SECS);
    let to = rfc3339_utc(started + CANDIDATE_WINDOW_SECS);

    let events = match io.events_between(&from, &to) {
        Ok(events) => events,
        Err(e) => {
            tracing::debug!(error = %e, "calendar: read failed, falling back to the generated title");
            return None;
        }
    };

    let matched = pick_event(started_at, ended_at, &events)?;
    // Only the title is ever taken, and only from the one matched event.
    // Note that the title itself is deliberately absent from this log line —
    // "Interview: <candidate>" does not belong in a log file either.
    tracing::debug!(event_id = %matched.id, "calendar: matched a recording to an event");
    let title = matched.title.trim();
    (!title.is_empty()).then(|| title.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A calendar that answers from a fixed list and records the window it was
    /// asked for, so the "never read more than the window" rule is testable.
    struct FakeCalendar {
        events: Vec<CalendarEvent>,
        asked: std::sync::Mutex<Vec<(String, String)>>,
    }

    impl FakeCalendar {
        fn new(events: Vec<CalendarEvent>) -> Self {
            Self {
                events,
                asked: std::sync::Mutex::new(Vec::new()),
            }
        }
    }

    impl CalendarIo for FakeCalendar {
        fn events_between(
            &self,
            start: &str,
            end: &str,
        ) -> Result<Vec<CalendarEvent>, CalendarError> {
            self.asked
                .lock()
                .unwrap()
                .push((start.to_string(), end.to_string()));
            Ok(self.events.clone())
        }
    }

    struct DeniedCalendar;

    impl CalendarIo for DeniedCalendar {
        fn events_between(
            &self,
            _start: &str,
            _end: &str,
        ) -> Result<Vec<CalendarEvent>, CalendarError> {
            Err(CalendarError::NotAuthorized)
        }
    }

    fn event(id: &str, title: &str, start: &str, end: &str) -> CalendarEvent {
        CalendarEvent {
            id: id.into(),
            title: title.into(),
            start_rfc3339: start.into(),
            end_rfc3339: end.into(),
            is_all_day: false,
            has_attendees: true,
            declined: false,
        }
    }

    #[test]
    fn epoch_secs_parses_both_shapes_that_reach_it() {
        assert_eq!(epoch_secs("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(epoch_secs("2026-09-19T10:00:00Z"), Some(1_789_812_000));
        // SQLite's `datetime('now')` spelling, which is UTC.
        assert_eq!(
            epoch_secs("2026-09-19 10:00:00"),
            epoch_secs("2026-09-19T10:00:00Z")
        );
        // An offset is applied, not ignored.
        assert_eq!(
            epoch_secs("2026-09-19T15:30:00+05:30"),
            epoch_secs("2026-09-19T10:00:00Z")
        );
        assert_eq!(
            epoch_secs("2026-09-19T02:00:00-08:00"),
            epoch_secs("2026-09-19T10:00:00Z")
        );
        assert_eq!(epoch_secs("2026-09-19T10:00:00.500Z"), Some(1_789_812_000));
    }

    /// An unparsable stamp is `None`, which sends the caller to the LLM title
    /// rather than to an arbitrary event.
    #[test]
    fn epoch_secs_refuses_rather_than_guesses() {
        assert_eq!(epoch_secs("soon"), None);
        assert_eq!(epoch_secs(""), None);
        assert_eq!(epoch_secs("2026-13-19T10:00:00Z"), None);
        assert_eq!(epoch_secs("2026-09-19T99:00:00Z"), None);
    }

    #[test]
    fn an_exact_start_matches() {
        let events = vec![event(
            "e1",
            "Q3 Roadmap Review",
            "2026-09-19T10:00:00Z",
            "2026-09-19T11:00:00Z",
        )];
        assert_eq!(
            pick_event("2026-09-19 10:00:00", None, &events).map(|e| e.title.as_str()),
            Some("Q3 Roadmap Review")
        );
    }

    #[test]
    fn recording_started_three_minutes_late_still_matches() {
        let events = vec![event(
            "e1",
            "Standup",
            "2026-09-19T10:00:00Z",
            "2026-09-19T10:15:00Z",
        )];
        assert!(pick_event("2026-09-19 10:03:00", None, &events).is_some());
    }

    /// Two back-to-back calls where the first ran over: the one that actually
    /// started when recording did wins.
    #[test]
    fn overlapping_events_are_resolved_by_closeness_of_start() {
        let events = vec![
            event(
                "e1",
                "Ran over",
                "2026-09-19T09:30:00Z",
                "2026-09-19T10:05:00Z",
            ),
            event(
                "e2",
                "Q3 Roadmap Review",
                "2026-09-19T10:00:00Z",
                "2026-09-19T11:00:00Z",
            ),
        ];
        assert_eq!(
            pick_event("2026-09-19 10:01:00", None, &events).map(|e| e.id.as_str()),
            Some("e2")
        );
    }

    /// The single most important rule: an all-day entry overlaps every
    /// recording that day and must never supply a title.
    #[test]
    fn an_all_day_event_never_beats_a_real_one() {
        let mut pto = event(
            "e1",
            "Sam — PTO",
            "2026-09-19T00:00:00Z",
            "2026-09-20T00:00:00Z",
        );
        pto.is_all_day = true;
        let events = vec![
            pto,
            event(
                "e2",
                "Q3 Roadmap Review",
                "2026-09-19T10:00:00Z",
                "2026-09-19T11:00:00Z",
            ),
        ];
        assert_eq!(
            pick_event("2026-09-19 10:00:00", None, &events).map(|e| e.id.as_str()),
            Some("e2")
        );
    }

    #[test]
    fn an_all_day_event_on_its_own_is_no_match_at_all() {
        let mut sprint = event(
            "e1",
            "Sprint 42",
            "2026-09-19T00:00:00Z",
            "2026-09-26T00:00:00Z",
        );
        sprint.is_all_day = true;
        assert!(pick_event("2026-09-19 10:00:00", None, &[sprint]).is_none());
    }

    #[test]
    fn a_declined_event_is_not_a_meeting_the_user_was_in() {
        let mut declined = event(
            "e1",
            "Optional sync",
            "2026-09-19T10:00:00Z",
            "2026-09-19T11:00:00Z",
        );
        declined.declined = true;
        assert!(pick_event("2026-09-19 10:00:00", None, &[declined]).is_none());
    }

    #[test]
    fn a_candidate_twenty_minutes_away_is_rejected() {
        let events = vec![event(
            "e1",
            "Later thing",
            "2026-09-19T10:20:00Z",
            "2026-09-19T11:00:00Z",
        )];
        assert!(pick_event("2026-09-19 10:00:00", None, &events).is_none());
    }

    #[test]
    fn an_empty_calendar_is_no_match() {
        assert!(pick_event("2026-09-19 10:00:00", None, &[]).is_none());
    }

    /// A real meeting beats a self-scheduled block at the same minute, and an
    /// eight-hour "Focus time" loses to a 30-minute standup.
    #[test]
    fn ties_break_on_attendees_then_on_duration() {
        let mut solo = event(
            "e1",
            "Focus time",
            "2026-09-19T10:00:00Z",
            "2026-09-19T18:00:00Z",
        );
        solo.has_attendees = false;
        let events = vec![
            solo,
            event(
                "e2",
                "Standup",
                "2026-09-19T10:00:00Z",
                "2026-09-19T10:30:00Z",
            ),
        ];
        assert_eq!(
            pick_event("2026-09-19 10:00:00", None, &events).map(|e| e.id.as_str()),
            Some("e2")
        );

        let both_solo = vec![
            {
                let mut e = event(
                    "e3",
                    "Long block",
                    "2026-09-19T10:00:00Z",
                    "2026-09-19T18:00:00Z",
                );
                e.has_attendees = false;
                e
            },
            {
                let mut e = event(
                    "e4",
                    "Short block",
                    "2026-09-19T10:00:00Z",
                    "2026-09-19T10:30:00Z",
                );
                e.has_attendees = false;
                e
            },
        ];
        assert_eq!(
            pick_event("2026-09-19 10:00:00", None, &both_solo).map(|e| e.id.as_str()),
            Some("e4")
        );
    }

    /// Calendar order is not stable, so two events that tie on every rule must
    /// still resolve the same way every time.
    #[test]
    fn identical_candidates_resolve_deterministically() {
        let a = event(
            "aaa",
            "Sync A",
            "2026-09-19T10:00:00Z",
            "2026-09-19T10:30:00Z",
        );
        let b = event(
            "bbb",
            "Sync B",
            "2026-09-19T10:00:00Z",
            "2026-09-19T10:30:00Z",
        );
        let forwards = vec![a.clone(), b.clone()];
        let backwards = vec![b, a];
        assert_eq!(
            pick_event("2026-09-19 10:00:00", None, &forwards).map(|e| e.id.as_str()),
            Some("aaa")
        );
        assert_eq!(
            pick_event("2026-09-19 10:00:00", None, &backwards).map(|e| e.id.as_str()),
            Some("aaa")
        );
    }

    #[test]
    fn an_unparsable_recording_start_matches_nothing() {
        let events = vec![event(
            "e1",
            "Sync",
            "2026-09-19T10:00:00Z",
            "2026-09-19T11:00:00Z",
        )];
        assert!(pick_event("soon", None, &events).is_none());
    }

    #[test]
    fn rfc3339_utc_round_trips_through_epoch_secs() {
        for stamp in [
            "1970-01-01T00:00:00Z",
            "2026-09-19T10:00:00Z",
            "2026-12-31T23:59:59Z",
            "2028-02-29T12:00:00Z",
        ] {
            assert_eq!(rfc3339_utc(epoch_secs(stamp).unwrap()), stamp);
        }
    }

    /// The privacy rule made testable: the read window is the ten minutes
    /// either side of the recording start, never the day and never the
    /// calendar.
    #[test]
    fn only_the_window_around_the_recording_is_ever_read() {
        let cal = FakeCalendar::new(vec![event(
            "e1",
            "Q3 Roadmap Review",
            "2026-09-19T10:00:00Z",
            "2026-09-19T11:00:00Z",
        )]);
        assert_eq!(
            title_for_recording(&cal, "2026-09-19 10:00:00", None),
            Some("Q3 Roadmap Review".into())
        );
        let asked = cal.asked.lock().unwrap().clone();
        assert_eq!(
            asked,
            vec![(
                "2026-09-19T09:50:00Z".to_string(),
                "2026-09-19T10:10:00Z".to_string()
            )]
        );
    }

    /// Denied permission degrades to "no title", never to an error the stop
    /// path could mistake for a failed meeting.
    #[test]
    fn a_denied_calendar_yields_no_title_rather_than_an_error() {
        assert_eq!(
            title_for_recording(&DeniedCalendar, "2026-09-19 10:00:00", None),
            None
        );
    }

    #[test]
    fn an_empty_calendar_and_an_unparsable_start_both_yield_no_title() {
        let empty = FakeCalendar::new(vec![]);
        assert_eq!(
            title_for_recording(&empty, "2026-09-19 10:00:00", None),
            None
        );
        let cal = FakeCalendar::new(vec![event(
            "e1",
            "Sync",
            "2026-09-19T10:00:00Z",
            "2026-09-19T11:00:00Z",
        )]);
        assert_eq!(title_for_recording(&cal, "soon", None), None);
        // A start it could not parse is not a window it should have asked for.
        assert!(cal.asked.lock().unwrap().is_empty());
    }

    /// A whitespace-only title is not a title, and must not replace a
    /// generated one with nothing.
    #[test]
    fn a_blank_event_title_is_not_used() {
        let cal = FakeCalendar::new(vec![event(
            "e1",
            "   ",
            "2026-09-19T10:00:00Z",
            "2026-09-19T11:00:00Z",
        )]);
        assert_eq!(title_for_recording(&cal, "2026-09-19 10:00:00", None), None);
    }
}
