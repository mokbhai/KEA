//! Non-macOS calendar stub — there is no calendar to read.
//!
//! Returns an empty list rather than an error: the caller's contract is "no
//! match falls back to the generated title", and an empty calendar is exactly
//! that case. An error here would be indistinguishable from a real failure in
//! the logs for a platform where none is possible.

use super::{CalendarError, CalendarEvent, CalendarIo};

pub struct StubCalendar;

impl StubCalendar {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubCalendar {
    fn default() -> Self {
        Self::new()
    }
}

impl CalendarIo for StubCalendar {
    fn events_between(
        &self,
        _start: &str,
        _end: &str,
    ) -> Result<Vec<CalendarEvent>, CalendarError> {
        Ok(Vec::new())
    }
}
