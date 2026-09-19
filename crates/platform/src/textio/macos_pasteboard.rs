//! The general pasteboard's change counter.
//!
//! `arboard` is a fine cross-platform clipboard, but it can only answer "what
//! is on the clipboard now". Both halves of the rewrite/dictation flow need to
//! answer a different question — *did anything happen* — and text comparison
//! cannot:
//!
//! * After a synthetic ⌘C, reading back the same text the user had copied
//!   earlier is indistinguishable from a copy that never happened. That is how
//!   rewrite ends up rewriting the wrong text.
//! * After we write the transcript, a clipboard manager (Raycast, Maccy,
//!   Paste) may write its own entry on top before we restore. Restoring then
//!   clobbers *their* entry instead of putting the user's data back.
//!
//! `NSPasteboard.changeCount` is the OS's own answer: a monotonic counter
//! bumped once per declared owner, by anyone, including us. Comparing it
//! before and after is exact.
//!
//! The class is looked up by name rather than by linking `objc2-app-kit`,
//! which would add a very large dependency for one property. AppKit is already
//! loaded in every process that can have a pasteboard to begin with, and the
//! lookup degrades to `None` rather than failing if it somehow is not.

use objc2::msg_send;
use objc2::runtime::{AnyClass, AnyObject};

/// Reads `[[NSPasteboard generalPasteboard] changeCount]`.
///
/// `None` when AppKit is not loaded (headless test runs, chiefly), in which
/// case callers must treat "did it change" as unknown rather than as "no".
pub fn change_count() -> Option<i64> {
    let class = AnyClass::get(c"NSPasteboard")?;
    // SAFETY: `generalPasteboard` is a class method returning a shared,
    // autoreleased singleton that outlives this call, and `changeCount` is a
    // plain `NSInteger` property on it. Both are safe from any thread:
    // NSPasteboard is documented as thread-safe, which is what lets this run
    // on the blocking pool alongside the rest of the paste path.
    unsafe {
        let pasteboard: *mut AnyObject = msg_send![class, generalPasteboard];
        if pasteboard.is_null() {
            return None;
        }
        let count: i64 = msg_send![pasteboard, changeCount];
        Some(count)
    }
}

/// Whether the pasteboard was written between two [`change_count`] readings.
///
/// Unknown readings (`None`, i.e. no AppKit) answer `true`: the caller's job is
/// to flag a *missing* change as a problem, and reporting one on no evidence
/// would turn a headless run into a stream of false alarms.
pub fn changed_between(before: Option<i64>, after: Option<i64>) -> bool {
    match (before, after) {
        (Some(before), Some(after)) => after != before,
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bumped_counter_is_a_change() {
        assert!(changed_between(Some(41), Some(42)));
    }

    #[test]
    fn an_unmoved_counter_is_not_a_change() {
        assert!(!changed_between(Some(42), Some(42)));
    }

    /// The silent-failure guard, inverted: when we cannot tell, we must not
    /// claim the copy failed. A false "the copy did not land" would abort a
    /// rewrite that was about to work.
    #[test]
    fn an_unknown_reading_is_never_reported_as_no_change() {
        assert!(changed_between(None, Some(42)));
        assert!(changed_between(Some(42), None));
        assert!(changed_between(None, None));
    }
}
