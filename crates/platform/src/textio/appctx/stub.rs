//! Non-macOS stub until Windows/Linux platform tasks land.
//!
//! Returns an empty context rather than an error: a missing app context is an
//! ordinary answer everywhere (Accessibility revoked, no GUI session), so the
//! callers already handle it, and "no profile applies" is the right behaviour
//! on a platform that cannot tell which app is frontmost.

use super::{AppContext, AppContextProbe, CaptureOpts};

pub struct StubAppContextProbe;

impl StubAppContextProbe {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StubAppContextProbe {
    fn default() -> Self {
        Self::new()
    }
}

impl AppContextProbe for StubAppContextProbe {
    fn capture(&self, _opts: CaptureOpts) -> AppContext {
        AppContext::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_captures_nothing_even_when_everything_is_opted_in() {
        let ctx = StubAppContextProbe::new().capture(CaptureOpts {
            window_title: true,
            url: true,
        });
        assert!(ctx.is_empty());
    }
}
