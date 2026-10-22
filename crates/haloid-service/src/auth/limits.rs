//! Per-key rate limiting: token buckets in memory.
//!
//! In front of every unauthenticated auth endpoint — login, registration,
//! and the ALTCHA challenge endpoint itself, which runs a key derivation per
//! request and so is a denial-of-service target the proof-of-work does not
//! cover. Single-instance by design, like the deployment; a second instance
//! would need these counters shared.

use haloid_core::DomainError;
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    updated: Instant,
}

pub struct RateLimiter {
    buckets: Mutex<HashMap<String, Bucket>>,
    /// Requests allowed in a burst.
    capacity: f64,
    /// Tokens restored per second.
    refill: f64,
    /// Above this many tracked keys, full buckets (idle callers) are
    /// forgotten, so a flood of distinct addresses cannot grow the map
    /// without bound.
    max_keys: usize,
}

impl RateLimiter {
    /// `burst` requests at once, then one every `every`.
    pub fn new(burst: u32, every: Duration) -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
            capacity: f64::from(burst.max(1)),
            refill: 1.0 / every.as_secs_f64().max(f64::EPSILON),
            max_keys: 100_000,
        }
    }

    /// Spend one request for `key` at `now`, or say how long until one is
    /// available.
    pub fn check(&self, key: &str, now: Instant) -> Result<(), DomainError> {
        self.check_with(key, now, self.capacity, self.refill)
    }

    /// [`RateLimiter::check`] with the rate given now rather than at
    /// construction, for limits an administrator can change at runtime.
    pub fn check_rate(
        &self,
        key: &str,
        now: Instant,
        burst: u32,
        every: Duration,
    ) -> Result<(), DomainError> {
        let capacity = f64::from(burst.max(1));
        let refill = 1.0 / every.as_secs_f64().max(f64::EPSILON);
        self.check_with(key, now, capacity, refill)
    }

    fn check_with(
        &self,
        key: &str,
        now: Instant,
        capacity: f64,
        refill: f64,
    ) -> Result<(), DomainError> {
        let mut buckets = self
            .buckets
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        if buckets.len() >= self.max_keys {
            buckets.retain(|_, b| {
                let elapsed = now.saturating_duration_since(b.updated).as_secs_f64();
                b.tokens + elapsed * refill < capacity
            });
        }

        let bucket = buckets.entry(key.to_string()).or_insert(Bucket {
            tokens: capacity,
            updated: now,
        });
        let elapsed = now.saturating_duration_since(bucket.updated).as_secs_f64();
        bucket.tokens = (bucket.tokens + elapsed * refill).min(capacity);
        bucket.updated = now;

        if bucket.tokens >= 1.0 {
            bucket.tokens -= 1.0;
            Ok(())
        } else {
            let wait = (1.0 - bucket.tokens) / refill;
            Err(DomainError::RateLimited {
                retry_after_secs: wait.ceil().max(1.0) as u64,
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_burst_is_allowed_then_refused_then_refilled() {
        let limiter = RateLimiter::new(3, Duration::from_secs(10));
        let t0 = Instant::now();
        for _ in 0..3 {
            limiter.check("1.2.3.4", t0).unwrap();
        }
        match limiter.check("1.2.3.4", t0) {
            Err(DomainError::RateLimited { retry_after_secs }) => assert_eq!(retry_after_secs, 10),
            other => panic!("expected RateLimited, got {other:?}"),
        }
        limiter
            .check("1.2.3.4", t0 + Duration::from_secs(10))
            .expect("one token back after the interval");
    }

    #[test]
    fn keys_are_independent() {
        let limiter = RateLimiter::new(1, Duration::from_secs(60));
        let now = Instant::now();
        limiter.check("a", now).unwrap();
        assert!(limiter.check("a", now).is_err());
        limiter
            .check("b", now)
            .expect("another caller is unaffected");
    }

    #[test]
    fn idle_keys_are_forgotten_when_the_map_is_full() {
        let mut limiter = RateLimiter::new(1, Duration::from_secs(1));
        limiter.max_keys = 10;
        let t0 = Instant::now();
        for i in 0..10 {
            limiter.check(&format!("k{i}"), t0).unwrap();
        }
        // An hour later every bucket is full again, so all are forgettable.
        limiter
            .check("new", t0 + Duration::from_secs(3600))
            .unwrap();
        assert!(limiter.buckets.lock().unwrap().len() <= 2);
    }
}
