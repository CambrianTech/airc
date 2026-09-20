//! Shared test support for the daemon's publish path.
//!
//! §3.8 back-pressure: the write-behind queue is bounded, and on a slow runner
//! a tight publish loop outpaces the SQLite drain. The daemon sheds with a LOUD
//! typed error (the designed signal, never a silent drop), so a test producer
//! must back off and retry the same event the way a real producer would.
//!
//! This lives here because the identical helper was inlined in TWO test files
//! (`inbox_paging.rs` and `room_tip_probe.rs`, the latter driving 5,000
//! publishes through its copy). Fixing the budget in one and not the other
//! would have fixed half the flake — one logical decision, one place.
#![allow(dead_code)] // each test binary uses a subset; that is not dead code.

use std::fmt::Display;
use std::future::Future;
use std::time::{Duration, Instant};

/// How long a publish RUN may stay saturated before the queue is called wedged.
///
/// A WALL-CLOCK budget, not an attempt count. The previous form was
/// `for _ in 0..500` with a flat 10 ms sleep — five seconds of unbroken
/// saturation — and a loaded Windows CI runner exceeds it: PR #1379 failed with
///
/// ```text
/// thread 'deep_room_most_recent_n_costs_by_n_not_room_depth'
///   panicked at crates\airc-daemon\tests\inbox_paging.rs:155:5:
///   publish kept saturating after 500 backoff retries
/// ```
///
/// on a diff that touched only `airc-cli`, and the identical commit passed on
/// re-run. That quoted text matters: it is the *retry-exhaustion* panic, which
/// rules out the other way this helper can fail on a slow runner — a single
/// publish exceeding `airc-ipc`'s 5 s `DEFAULT_RPC_TIMEOUT`, which surfaces as
/// "daemon RPC timed out", does NOT contain "saturated", and would trip the
/// assert below immediately. A wall-clock budget is no defence against that
/// path, so it was worth establishing which one we actually saw.
///
/// Sixty seconds is far past any healthy drain and still fails loudly on a
/// genuinely wedged queue — which is the behaviour this helper exists to
/// preserve.
pub const SATURATION_BUDGET: Duration = Duration::from_secs(60);

/// Retry backoff: starts at the old flat rate so a healthy drain is never
/// slowed, then eases off so a struggling one is not hammered while we wait it
/// out. Capped so a long wait still retries promptly once the drain recovers.
pub const BACKOFF_MIN: Duration = Duration::from_millis(10);
pub const BACKOFF_MAX: Duration = Duration::from_millis(250);

/// Retry `op` while it fails with the daemon's saturation shed, until `deadline`.
///
/// Generic over the operation and its error so the POLICY is testable without a
/// daemon, a socket, or a filesystem — see `tests/backoff_policy.rs`. Extracting
/// it is what turned this from "reasoned" into "proven": against the live daemon
/// the error arm is unreachable on a fast machine (verified — a 1 ns budget
/// still passes locally, and still passes under 36 busy-loops on 12 cores,
/// because saturation is I/O-bound rather than CPU-bound).
///
/// `deadline` is an absolute instant, not a per-call budget, so a caller
/// publishing N events bounds the WHOLE run rather than each event separately.
/// A per-call budget would let an oscillating queue — saturating for a while,
/// then accepting one event, repeating — run for N × budget, which is not what
/// "the queue is wedged" means.
///
/// Panics on any error that is not the saturation shed, on the FIRST occurrence:
/// a real failure must never be retried into a timeout.
pub async fn retry_while_saturated<F, Fut, T, E>(deadline: Instant, mut op: F) -> T
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    E: Display,
{
    let started = Instant::now();
    let mut backoff = BACKOFF_MIN;
    let mut attempts = 0u32;
    loop {
        match op().await {
            Ok(value) => return value,
            Err(error) => {
                let message = error.to_string();
                assert!(
                    message.contains("saturated"),
                    "unexpected publish error: {message}"
                );
                attempts += 1;
                if Instant::now() >= deadline {
                    // Report the DURATION alongside the attempt count: "500
                    // retries" alone named an implementation detail, and whether
                    // that was 5 seconds or 5 minutes is what decides
                    // wedged-queue versus merely-slow-runner.
                    panic!(
                        "publish stayed saturated for {:?} ({attempts} attempts) — \
                         the write-behind queue never drained",
                        started.elapsed()
                    );
                }
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(BACKOFF_MAX);
            }
        }
    }
}

/// The deadline a publish RUN should be bounded by, computed once by the caller.
pub fn saturation_deadline() -> Instant {
    Instant::now() + SATURATION_BUDGET
}
