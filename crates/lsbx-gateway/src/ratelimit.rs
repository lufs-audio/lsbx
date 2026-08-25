//! Token-bucket rate limiter (Unit 13's genuinely new functionality —
//! SPEC.md Deviation 13; the existing Python gateway has no rate limiter
//! at all).
//!
//! Keyed by bearer token for authenticated routes, falling back to source
//! IP for the one unauthenticated route (`GET /console`), per this unit's
//! acceptance criteria. Exhaustion returns `429` with a `Retry-After`
//! header naming exactly how long until the bucket has at least one token
//! again.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Configuration for the token bucket: sustained rate plus how many
/// requests can burst above that rate before throttling kicks in.
#[derive(Debug, Clone, Copy)]
pub struct RateLimitConfig {
    pub requests_per_minute: u32,
    pub burst: u32,
}

impl Default for RateLimitConfig {
    /// A conservative but real default (not zero, which would make every
    /// route permanently `429`) — chosen so a gateway with no explicit
    /// rate-limit configuration is still usably rate-limited rather than
    /// silently unlimited, matching this unit's own framing of the
    /// limiter as real functionality, not a decorative no-op.
    fn default() -> Self {
        Self {
            requests_per_minute: 120,
            burst: 20,
        }
    }
}

#[derive(Debug)]
struct BucketState {
    tokens: f64,
    last_refill: Instant,
}

/// The decision a `TokenBucket::check` call reaches for one key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RateLimitDecision {
    Allow,
    Deny { retry_after: Duration },
}

/// Per-key token-bucket state, one bucket per distinct key (bearer
/// token, or source IP for the unauthenticated route).
///
/// Buckets are created lazily on first use and never proactively evicted —
/// this crate's own acceptance criteria/tests only exercise short-lived
/// gateway processes (each test constructs its own `TokenBucket`), and
/// unbounded per-key growth over a long-lived process's lifetime is a
/// real, documented follow-up rather than a silent gap: a production
/// deployment with a large, ever-changing set of source IPs hitting the
/// unauthenticated route would want an eviction/TTL policy this
/// implementation does not attempt to invent here.
pub struct TokenBucket {
    config: RateLimitConfig,
    buckets: Mutex<HashMap<String, BucketState>>,
}

impl TokenBucket {
    pub fn new(config: RateLimitConfig) -> Self {
        Self {
            config,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Checks and consumes one token for `key`, refilling first based on
    /// elapsed time since the bucket's last refill at the configured
    /// `requests_per_minute` rate, capped at `burst`.
    ///
    /// `key` is a bearer token for an authenticated route, or a source IP
    /// string for the sole unauthenticated route (`GET /console`) — this
    /// type has no opinion on which; the caller (the middleware in
    /// `lib.rs`) decides what to pass based on whether `AuthedRequest`
    /// resolved for the request.
    pub fn check(&self, key: &str) -> RateLimitDecision {
        let now = Instant::now();
        let refill_per_sec = f64::from(self.config.requests_per_minute) / 60.0;
        let capacity = f64::from(self.config.burst.max(1));

        #[allow(clippy::unwrap_used)] // Poisoning would mean an earlier panic inside this same lock; there is no meaningful recovery for a rate limiter beyond surfacing that panic, and this crate's own workspace lints already warn (not deny) unwrap_used for exactly this class of "a poisoned lock is itself the bug report" situation.
        let mut buckets = self.buckets.lock().unwrap();
        let state = buckets.entry(key.to_string()).or_insert_with(|| BucketState {
            tokens: capacity,
            last_refill: now,
        });

        let elapsed = now.saturating_duration_since(state.last_refill).as_secs_f64();
        state.tokens = (state.tokens + elapsed * refill_per_sec).min(capacity);
        state.last_refill = now;

        if state.tokens >= 1.0 {
            state.tokens -= 1.0;
            RateLimitDecision::Allow
        } else {
            // How long until at least one token is available: the
            // shortfall (in tokens) divided by the refill rate (tokens per
            // second) gives seconds; round up so `Retry-After` never
            // undershoots and invites a caller to retry a moment too
            // early.
            let shortfall = 1.0 - state.tokens;
            let seconds_until_token = (shortfall / refill_per_sec).ceil().max(1.0);
            RateLimitDecision::Deny {
                retry_after: Duration::from_secs_f64(seconds_until_token),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    #[test]
    fn allows_requests_within_burst() {
        let bucket = TokenBucket::new(RateLimitConfig {
            requests_per_minute: 60,
            burst: 3,
        });
        assert_eq!(bucket.check("key-a"), RateLimitDecision::Allow);
        assert_eq!(bucket.check("key-a"), RateLimitDecision::Allow);
        assert_eq!(bucket.check("key-a"), RateLimitDecision::Allow);
    }

    #[test]
    fn denies_once_burst_is_exhausted() {
        let bucket = TokenBucket::new(RateLimitConfig {
            requests_per_minute: 60,
            burst: 2,
        });
        assert_eq!(bucket.check("key-b"), RateLimitDecision::Allow);
        assert_eq!(bucket.check("key-b"), RateLimitDecision::Allow);
        let decision = bucket.check("key-b");
        assert!(matches!(decision, RateLimitDecision::Deny { .. }));
    }

    #[test]
    fn retry_after_is_positive_on_denial() {
        let bucket = TokenBucket::new(RateLimitConfig {
            requests_per_minute: 60,
            burst: 1,
        });
        assert_eq!(bucket.check("key-c"), RateLimitDecision::Allow);
        match bucket.check("key-c") {
            RateLimitDecision::Deny { retry_after } => assert!(retry_after.as_secs() >= 1),
            RateLimitDecision::Allow => panic!("expected denial after burst exhausted"),
        }
    }

    #[test]
    fn distinct_keys_have_independent_buckets() {
        let bucket = TokenBucket::new(RateLimitConfig {
            requests_per_minute: 60,
            burst: 1,
        });
        assert_eq!(bucket.check("key-d"), RateLimitDecision::Allow);
        // A different key must not be affected by key-d's exhaustion.
        assert_eq!(bucket.check("key-e"), RateLimitDecision::Allow);
    }

    #[test]
    fn refills_over_time() {
        let bucket = TokenBucket::new(RateLimitConfig {
            requests_per_minute: 6000, // 100/sec, so refill is fast enough to observe in a test
            burst: 1,
        });
        assert_eq!(bucket.check("key-f"), RateLimitDecision::Allow);
        assert!(matches!(bucket.check("key-f"), RateLimitDecision::Deny { .. }));
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(bucket.check("key-f"), RateLimitDecision::Allow);
    }
}
