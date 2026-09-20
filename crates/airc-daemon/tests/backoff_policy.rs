//! The publish retry POLICY, proven without a daemon.
//!
//! Why this file exists: the policy in `common::retry_while_saturated` guards
//! §3.8 back-pressure in two integration tests, and against the live daemon its
//! error arm is UNREACHABLE on a developer machine — a 1 ns budget still passes
//! locally, and still passes under 36 busy-loops on 12 cores, because saturation
//! is I/O-bound rather than CPU-bound. So the whole `Err` path (the assert, the
//! attempt counter, the backoff clamp, the panic) shipped untested until now:
//! the flake it guards only reproduces on a loaded Windows CI runner.
//!
//! Making the policy generic over the operation is what makes it provable here
//! in milliseconds, with no socket and no filesystem.

mod common;

use common::{retry_while_saturated, BACKOFF_MAX, BACKOFF_MIN};
use std::cell::Cell;
use std::time::{Duration, Instant};

/// The daemon's shed, as the client surfaces it. `ClientError::Daemon(String)`
/// flattens the bus error at the IPC boundary, so the real code has no variant
/// to match on and tests the substring — mirrored here rather than idealised, so
/// this exercises what production actually does.
fn saturated() -> String {
    "write-behind queue saturated".to_string()
}

// what this catches: the happy path regressing into a retry, and the clamp
// failing to cap. A caller that succeeds after N sheds must get its value, and
// the waits must follow 10/20/40/80/160/250/250… — never unbounded growth.
#[tokio::test]
async fn returns_the_value_after_transient_saturation_and_caps_the_backoff() {
    let attempts = Cell::new(0u32);
    let started = Instant::now();
    let deadline = started + Duration::from_secs(30);

    let value = retry_while_saturated(deadline, || {
        let n = attempts.get() + 1;
        attempts.set(n);
        async move {
            if n <= 8 {
                Err(saturated())
            } else {
                Ok(n)
            }
        }
    })
    .await;

    assert_eq!(value, 9, "returned on the first Ok");
    assert_eq!(attempts.get(), 9, "retried exactly the sheds, no more");

    // 8 sheds ⇒ waits of 10+20+40+80+160+250+250+250 = 1060 ms.
    let elapsed = started.elapsed();
    assert!(
        elapsed >= Duration::from_millis(1000),
        "backed off far too little: {elapsed:?}"
    );
    // Unclamped doubling would be 10+20+…+1280 = 2550 ms; the cap is what keeps
    // this under 2 s. Mutation check: removing `.min(BACKOFF_MAX)` fails here.
    assert!(
        elapsed < Duration::from_millis(2000),
        "clamp did not hold — backoff grew unbounded: {elapsed:?}"
    );
}

// what this catches: a wedged queue failing to fail, or failing without saying
// how long it waited. The panic message is the whole diagnostic — "500 retries"
// told a reader nothing about wedged-vs-slow.
#[tokio::test]
async fn a_permanently_saturated_queue_panics_naming_the_duration() {
    let deadline = Instant::now() + Duration::from_millis(50);
    let result = std::panic::AssertUnwindSafe(retry_while_saturated(deadline, || async {
        Err::<(), _>(saturated())
    }))
    .catch_unwind()
    .await;

    let panic = result.expect_err("a permanently saturated queue must panic");
    let message = panic
        .downcast_ref::<String>()
        .expect("panic payload is a String");
    assert!(
        message.contains("stayed saturated for"),
        "message must name the DURATION: {message}"
    );
    assert!(
        message.contains("attempts"),
        "message must name the attempt count too: {message}"
    );
}

// what this catches: the single most dangerous refactor of this helper —
// retrying a REAL error into a timeout. A non-saturation failure must abort on
// the FIRST occurrence, never be swallowed by the retry loop.
#[tokio::test]
async fn a_non_saturation_error_fails_immediately() {
    let attempts = Cell::new(0u32);
    let deadline = Instant::now() + Duration::from_secs(30);

    let result = std::panic::AssertUnwindSafe(retry_while_saturated(deadline, || {
        attempts.set(attempts.get() + 1);
        async { Err::<(), _>("daemon RPC timed out".to_string()) }
    }))
    .catch_unwind()
    .await;

    assert!(result.is_err(), "a real error must not be retried");
    assert_eq!(
        attempts.get(),
        1,
        "must abort on the FIRST non-saturation error, not retry it"
    );
}

// what this catches: the constants drifting into a shape that reintroduces the
// flake — a floor above the old flat rate would slow every healthy publish, and
// a cap below the floor would make the clamp meaningless.
#[test]
fn backoff_bounds_stay_coherent() {
    assert!(
        BACKOFF_MIN <= BACKOFF_MAX,
        "floor must not exceed the cap: {BACKOFF_MIN:?} > {BACKOFF_MAX:?}"
    );
    assert_eq!(
        BACKOFF_MIN,
        Duration::from_millis(10),
        "the floor is deliberately the OLD flat rate, so a healthy drain sees \
         no regression on the happy path"
    );
}

use futures::FutureExt as _;
