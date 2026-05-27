//! Injectable wall clock (§5, §9).
//!
//! `occurred_at_ms` is owner-stamped through a [`Clock`] so tests are
//! bit-deterministic. Production uses [`SystemClock`]; tests use
//! [`ManualClock`] and advance it by hand.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Source of wall-clock milliseconds. `Send + Sync` so it lives behind an
/// `Arc` shared across the router's tasks.
pub trait Clock: Send + Sync {
    /// Current time as epoch milliseconds.
    fn now_ms(&self) -> u64;
}

/// Real wall clock.
#[derive(Debug, Default, Clone)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }
}

/// Deterministic clock for tests. Starts at a fixed instant; `set`/`advance`
/// move it explicitly. Cheap to clone — clones share the same time cell.
#[derive(Debug, Clone)]
pub struct ManualClock {
    now: Arc<AtomicU64>,
}

impl ManualClock {
    /// Construct a clock reading `start_ms`.
    pub fn new(start_ms: u64) -> Self {
        Self {
            now: Arc::new(AtomicU64::new(start_ms)),
        }
    }

    /// Advance the clock by `delta_ms`.
    pub fn advance(&self, delta_ms: u64) {
        self.now.fetch_add(delta_ms, Ordering::SeqCst);
    }

    /// Set the clock to an absolute time.
    pub fn set(&self, now_ms: u64) {
        self.now.store(now_ms, Ordering::SeqCst);
    }
}

impl Default for ManualClock {
    fn default() -> Self {
        Self::new(1_700_000_000_000)
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_clock_is_deterministic() {
        let c = ManualClock::new(1000);
        assert_eq!(c.now_ms(), 1000);
        c.advance(50);
        assert_eq!(c.now_ms(), 1050);
        c.set(42);
        assert_eq!(c.now_ms(), 42);
    }

    #[test]
    fn manual_clock_clone_shares_time() {
        let c = ManualClock::new(0);
        let d = c.clone();
        c.advance(10);
        assert_eq!(d.now_ms(), 10, "clones share the underlying time cell");
    }
}
