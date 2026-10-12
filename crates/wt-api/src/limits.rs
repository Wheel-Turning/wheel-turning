//! In-memory rate limits (Spec 06): asks per tester per hour, and the two public forms per
//! address per hour. A token bucket per key holds `per_hour` tokens and refills them evenly
//! over the hour, so `per_hour` requests pass at once and the next waits.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const HOUR: Duration = Duration::from_secs(3600);
/// Keys idle for this long are dropped, so the map does not grow without bound.
const FORGET_AFTER: Duration = Duration::from_secs(2 * 3600);

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// One limit: `per_hour` per key.
#[derive(Debug)]
pub struct Limiter {
    per_hour: u32,
    buckets: Mutex<HashMap<String, Bucket>>,
}

impl Limiter {
    /// A limit of `per_hour` per key. Zero allows nothing.
    pub fn new(per_hour: u32) -> Self {
        Self {
            per_hour,
            buckets: Mutex::new(HashMap::new()),
        }
    }

    /// Takes a token for `key`. `Err` carries how long until the next one.
    pub fn check(&self, key: &str) -> Result<(), Duration> {
        self.check_at(key, Instant::now())
    }

    fn check_at(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let capacity = f64::from(self.per_hour);
        if self.per_hour == 0 {
            return Err(HOUR);
        }
        let rate = capacity / HOUR.as_secs_f64();
        // A poisoned lock only means another request panicked mid-update; the counts are
        // still usable.
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if buckets.len() > 10_000 {
            buckets.retain(|_, b| now.saturating_duration_since(b.at) < FORGET_AFTER);
        }
        let b = buckets.entry(key.to_owned()).or_insert(Bucket {
            tokens: capacity,
            at: now,
        });
        let elapsed = now.saturating_duration_since(b.at).as_secs_f64();
        b.tokens = (b.tokens + elapsed * rate).min(capacity);
        b.at = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(Duration::from_secs_f64((1.0 - b.tokens) / rate))
        }
    }
}

/// Whole minutes until `wait` passes, at least one, for "try again in N minutes".
pub fn minutes(wait: Duration) -> u64 {
    wait.as_secs().div_ceil(60).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bucket_allows_its_hourly_count_then_waits() {
        let l = Limiter::new(3);
        let t = Instant::now();
        for _ in 0..3 {
            assert!(l.check_at("a", t).is_ok());
        }
        let wait = l.check_at("a", t).unwrap_err();
        assert_eq!(minutes(wait), 20, "one token comes back every 20 minutes");
        assert!(l.check_at("b", t).is_ok(), "keys are separate");
        assert!(l.check_at("a", t + Duration::from_secs(20 * 60)).is_ok());
        assert!(l.check_at("a", t + Duration::from_secs(20 * 60)).is_err());
    }

    #[test]
    fn zero_allows_nothing() {
        assert!(Limiter::new(0).check("a").is_err());
    }
}
