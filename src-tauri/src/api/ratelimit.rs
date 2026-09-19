//! A token bucket over the LLM-calling verbs.
//!
//! **Not a denial-of-service defence.** A local caller that wants to burn the
//! machine can just burn the CPU; refusing it HTTP requests achieves nothing.
//! The thing worth preventing is a runaway script — a `while true` in a Raycast
//! command, a retry loop with no backoff — spending the user's LLM API credits
//! a few hundred dollars at a time. So the limit is expressed as cost and sits
//! on the verbs that cost money, and every refusal is logged, because the log
//! line is how the user finds out which script did it.
//!
//! Per token, not per peer: over a unix socket every peer is the same user,
//! and there is exactly one token.

use std::time::{Duration, Instant};

/// How many LLM-backed API calls a minute, when the setting is absent.
///
/// Twenty is well above interactive use (a person cannot select, invoke and
/// read twenty rewrites in a minute) and well below what a loop does.
pub const DEFAULT_REWRITES_PER_MINUTE: u32 = 20;

/// Refills continuously rather than in steps, so a caller that waits three
/// seconds gets a token back instead of waiting out a whole window.
///
/// The clock is a parameter on every method, never read inside: that is what
/// makes the refill arithmetic testable without sleeping.
#[derive(Debug)]
pub struct TokenBucket {
    capacity: f64,
    per_second: f64,
    tokens: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn per_minute(limit: u32, now: Instant) -> Self {
        // A limit of zero means "no LLM calls over the API", not "divide by
        // zero": the bucket simply never has a token.
        let capacity = f64::from(limit);
        Self {
            capacity,
            per_second: capacity / 60.0,
            tokens: capacity,
            last: now,
        }
    }

    /// Take one token, or report that the caller is over the limit.
    pub fn try_take(&mut self, now: Instant) -> bool {
        self.refill(now);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Roughly how long until the next token, for the `Retry-After` header.
    pub fn retry_after(&self, now: Instant) -> Duration {
        if self.per_second <= 0.0 {
            // Never, but a client still needs a number it can sleep on.
            return Duration::from_secs(60);
        }
        let mut filled =
            self.tokens + now.saturating_duration_since(self.last).as_secs_f64() * self.per_second;
        filled = filled.min(self.capacity);
        if filled >= 1.0 {
            return Duration::ZERO;
        }
        Duration::from_secs_f64((1.0 - filled) / self.per_second)
    }

    fn refill(&mut self, now: Instant) {
        // `saturating_duration_since` rather than subtraction: a caller may
        // hand us an older instant than the last one (two requests timed on
        // different threads), and that must be a no-op, not a panic.
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        if elapsed > 0.0 {
            self.tokens = (self.tokens + elapsed * self.per_second).min(self.capacity);
        }
        if now > self.last {
            self.last = now;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_bucket_allows_exactly_its_capacity_then_refuses() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::per_minute(3, t0);
        assert!(bucket.try_take(t0));
        assert!(bucket.try_take(t0));
        assert!(bucket.try_take(t0));
        assert!(!bucket.try_take(t0));
    }

    #[test]
    fn it_refills_continuously() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::per_minute(60, t0);
        for _ in 0..60 {
            assert!(bucket.try_take(t0));
        }
        assert!(!bucket.try_take(t0));
        // One per second at 60/min.
        assert!(bucket.try_take(t0 + Duration::from_secs(1)));
        assert!(!bucket.try_take(t0 + Duration::from_secs(1)));
    }

    #[test]
    fn it_never_fills_past_capacity() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::per_minute(2, t0);
        // An hour of idleness does not buy an hour of burst.
        let later = t0 + Duration::from_secs(3600);
        assert!(bucket.try_take(later));
        assert!(bucket.try_take(later));
        assert!(!bucket.try_take(later));
    }

    #[test]
    fn a_zero_limit_refuses_everything_without_dividing_by_zero() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::per_minute(0, t0);
        assert!(!bucket.try_take(t0));
        assert!(!bucket.try_take(t0 + Duration::from_secs(600)));
        assert_eq!(bucket.retry_after(t0), Duration::from_secs(60));
    }

    #[test]
    fn a_clock_that_goes_backwards_is_a_no_op() {
        let t0 = Instant::now() + Duration::from_secs(10);
        let mut bucket = TokenBucket::per_minute(1, t0);
        assert!(bucket.try_take(t0));
        assert!(!bucket.try_take(t0 - Duration::from_secs(5)));
    }

    #[test]
    fn retry_after_is_zero_while_tokens_remain_and_positive_once_empty() {
        let t0 = Instant::now();
        let mut bucket = TokenBucket::per_minute(60, t0);
        assert_eq!(bucket.retry_after(t0), Duration::ZERO);
        for _ in 0..60 {
            bucket.try_take(t0);
        }
        let wait = bucket.retry_after(t0);
        assert!(
            wait > Duration::ZERO && wait <= Duration::from_secs(2),
            "{wait:?}"
        );
    }
}
