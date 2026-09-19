//! EventKit calendar reads.
//!
//! # What is read, and what is deliberately not
//!
//! Per event: identifier, title, start, end, all-day flag, `hasAttendees` and
//! the event's own cancellation status. Nothing else. Not the notes body, not
//! the location, not the attendee array, not the URL, not the calendar name.
//! See the privacy rule on the parent module — the shape of [`CalendarEvent`]
//! is the enforcement mechanism.
//!
//! `hasAttendees` is `EKCalendarItem.hasAttendees`, a boolean the framework
//! computes itself. It is used rather than `attendees.count` precisely so the
//! attendee array is never materialised.
//!
//! # `declined` is cancellation, not participation — and why
//!
//! The plan set a verification gate: is participation status reachable from
//! `EKEvent` without pulling attendee identities? It is not. `EKEvent.status`
//! is the *event's* status (none / confirmed / tentative / canceled); the
//! user's own RSVP lives on the `EKParticipant` in `event.attendees` whose
//! `isCurrentUser` is true, which means materialising the attendee array and
//! with it every attendee's name and address. The gate says to drop `declined`
//! rather than do that, so [`CalendarEvent::declined`] is set from
//! `status == EKEventStatusCanceled` alone. A meeting the user declined but
//! that still happened can therefore supply a title; that is a title being
//! slightly wrong, against reading a contact list to avoid it.
//!
//! # Subscribed and birthday calendars are excluded
//!
//! A subscribed holiday or sports calendar with *timed* entries would pollute
//! matching, and those entries are not meetings the user was in. Events are
//! filtered on `EKCalendar.type`, which the framework exposes directly, so no
//! allowlist setting is needed.
//!
//! # Manual verification (not exercised by `cargo test`)
//!
//! 1. Turn on "Name meetings from your calendar" and grant access when asked.
//! 2. Record a short meeting during a real calendar event.
//! 3. The saved meeting is named after the event and shows the "from Calendar"
//!    chip.
//! 4. With access denied, the same recording gets the generated title and
//!    nothing is logged as an error.

use std::ffi::{c_char, CStr};

use objc2::rc::{autoreleasepool, Retained};
use objc2::runtime::{AnyObject, Bool};
use objc2::{class, msg_send};

use super::{epoch_secs, CalendarError, CalendarEvent, CalendarIo};
use crate::permissions::macos::{event_auth_status, EK_AUTH_FULL_ACCESS};

// Force EventKit to be linked.
//
// Nothing else in the process pulls it in — a Tauri app loads AppKit and
// WebKit, not EventKit — and `class!(EKEventStore)` *panics* when the class is
// not registered rather than returning null. Without this line, asking for the
// Calendar permission status takes the app down; with it, the framework is
// loaded at launch and the class is always there to answer.
#[link(name = "EventKit", kind = "framework")]
extern "C" {}

/// `EKEventStatusCanceled`.
const EK_EVENT_STATUS_CANCELED: i64 = 3;

/// `EKCalendarType` values whose events are never meetings the user attended.
const EK_CALENDAR_TYPE_SUBSCRIPTION: i64 = 3;
const EK_CALENDAR_TYPE_BIRTHDAY: i64 = 4;

pub struct MacCalendar;

impl MacCalendar {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacCalendar {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarIo for MacCalendar {
    fn events_between(&self, start: &str, end: &str) -> Result<Vec<CalendarEvent>, CalendarError> {
        // Checked before touching the store so a denied grant is a clear error
        // rather than an empty list that looks like an empty calendar — and so
        // nothing here can trigger a prompt the user did not ask for.
        if event_auth_status() != EK_AUTH_FULL_ACCESS {
            return Err(CalendarError::NotAuthorized);
        }

        let from = epoch_secs(start)
            .ok_or_else(|| CalendarError::Other(format!("unparsable window start {start:?}")))?;
        let to = epoch_secs(end)
            .ok_or_else(|| CalendarError::Other(format!("unparsable window end {end:?}")))?;

        autoreleasepool(|_| unsafe { read_events(from as f64, to as f64) })
    }
}

/// # Safety
/// Sends Objective-C messages; every pointer is checked for null before use.
unsafe fn read_events(from_epoch: f64, to_epoch: f64) -> Result<Vec<CalendarEvent>, CalendarError> {
    let store: *mut AnyObject = msg_send![class!(EKEventStore), alloc];
    let store: *mut AnyObject = msg_send![store, init];
    let store = Retained::from_raw(store)
        .ok_or_else(|| CalendarError::Other("EKEventStore could not be created".into()))?;

    let start_date: *mut AnyObject =
        msg_send![class!(NSDate), dateWithTimeIntervalSince1970: from_epoch];
    let end_date: *mut AnyObject =
        msg_send![class!(NSDate), dateWithTimeIntervalSince1970: to_epoch];

    // `calendars: nil` is "every calendar the grant covers"; the per-calendar
    // filter below is what narrows it, because EventKit has no predicate for
    // calendar *type*.
    let nil_calendars: *mut AnyObject = std::ptr::null_mut();
    let predicate: *mut AnyObject = msg_send![
        &*store,
        predicateForEventsWithStartDate: start_date,
        endDate: end_date,
        calendars: nil_calendars
    ];
    if predicate.is_null() {
        return Err(CalendarError::Other("EventKit refused the window".into()));
    }

    let events: *mut AnyObject = msg_send![&*store, eventsMatchingPredicate: predicate];
    if events.is_null() {
        // Genuinely no events in the window: an empty list, not an error.
        return Ok(Vec::new());
    }

    let formatter: *mut AnyObject = msg_send![class!(NSISO8601DateFormatter), alloc];
    let formatter: *mut AnyObject = msg_send![formatter, init];
    let formatter = Retained::from_raw(formatter)
        .ok_or_else(|| CalendarError::Other("NSISO8601DateFormatter unavailable".into()))?;

    let count: usize = msg_send![events, count];
    let mut out = Vec::with_capacity(count);
    for index in 0..count {
        let event: *mut AnyObject = msg_send![events, objectAtIndex: index];
        if let Some(parsed) = read_event(event, &formatter) {
            out.push(parsed);
        }
    }
    Ok(out)
}

/// One `EKEvent`, or `None` when it is not a thing this feature can use.
///
/// # Safety
/// `event` must be an `EKEvent` or null.
unsafe fn read_event(
    event: *mut AnyObject,
    formatter: &Retained<AnyObject>,
) -> Option<CalendarEvent> {
    if event.is_null() {
        return None;
    }

    let calendar: *mut AnyObject = msg_send![event, calendar];
    if !calendar.is_null() {
        let kind: i64 = msg_send![calendar, type];
        if kind == EK_CALENDAR_TYPE_SUBSCRIPTION || kind == EK_CALENDAR_TYPE_BIRTHDAY {
            return None;
        }
    }

    let title = ns_string_to_rust(msg_send![event, title])?;
    // An untitled event has no title to lend; skipping it here is cheaper than
    // letting `pick_event` win with an empty string.
    if title.trim().is_empty() {
        return None;
    }

    let start: *mut AnyObject = msg_send![event, startDate];
    let end: *mut AnyObject = msg_send![event, endDate];
    let start_rfc3339 = iso8601(formatter, start)?;
    let end_rfc3339 = iso8601(formatter, end).unwrap_or_else(|| start_rfc3339.clone());

    let id = ns_string_to_rust(msg_send![event, eventIdentifier]).unwrap_or_else(|| {
        // An event with no identifier still matches; it just cannot be told
        // apart from another one, and the tie-break falls through to start.
        format!("{start_rfc3339}|{title}")
    });

    let is_all_day: Bool = msg_send![event, isAllDay];
    let has_attendees: Bool = msg_send![event, hasAttendees];
    let status: i64 = msg_send![event, status];

    Some(CalendarEvent {
        id,
        title,
        start_rfc3339,
        end_rfc3339,
        is_all_day: is_all_day.is_true(),
        has_attendees: has_attendees.is_true(),
        declined: status == EK_EVENT_STATUS_CANCELED,
    })
}

/// # Safety
/// `date` must be an `NSDate` or null.
unsafe fn iso8601(formatter: &Retained<AnyObject>, date: *mut AnyObject) -> Option<String> {
    if date.is_null() {
        return None;
    }
    ns_string_to_rust(msg_send![&**formatter, stringFromDate: date])
}

/// # Safety
/// `string` must be an `NSString` or null.
unsafe fn ns_string_to_rust(string: *mut AnyObject) -> Option<String> {
    if string.is_null() {
        return None;
    }
    let utf8: *const c_char = msg_send![string, UTF8String];
    if utf8.is_null() {
        return None;
    }
    CStr::from_ptr(utf8).to_str().ok().map(str::to_owned)
}
