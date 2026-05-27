//! Acceptance test 3 — slow subscriber (§3.5, §5, §11.1).
//!
//! Two subscribers, one never drains. The fast one keeps receiving live with
//! no stall; the slow one is marked lagged and can resume from the sink via
//! its cursor. Fan-out is NEVER blocked by the slow consumer.

mod common;

use std::time::Duration;

use futures::StreamExt;

use airc_bus::{Filter, RouterConfig};
use airc_core::RoomId;

use common::{durable, Owner};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lagging_subscriber_never_stalls_fanout_and_resumes_from_sink() {
    let ch = RoomId::from_u128(0x10a6);

    // Bounded subscriber buffer: small enough that the never-drained slow
    // subscriber overflows it (and is marked lagged), large enough that the
    // continuously-drained fast subscriber's transient backlog stays under it.
    const BUFFER: usize = 16;
    const N: u128 = 200;
    let owner = Owner::new(RouterConfig {
        subscriber_buffer: BUFFER,
        ring_capacity: 4096,
        ..Default::default()
    });
    let r = &owner.router;

    // Slow subscriber: attach but NEVER drain it. Its mpsc receiver stays
    // alive (so the handle isn't dropped) and fills to BUFFER, then overflows.
    let (_slow_stream, slow_lag) = r.subscribe_with_lag(Filter::channel(ch), None);
    futures::pin_mut!(_slow_stream);

    // Fast subscriber: drained in lockstep with publishing below.
    let (fast_stream, _fast_lag) = r.subscribe_with_lag(Filter::channel(ch), None);
    futures::pin_mut!(fast_stream);

    // Publish N events, draining the fast subscriber after each so it keeps
    // pace and never overflows its own buffer. Each publish is timeout-guarded:
    // if fan-out blocked on the slow (undrained) subscriber, publish would
    // hang and fail here — the core "never stalls the shard" assertion.
    let mut seen = Vec::new();
    for i in 1..=N {
        tokio::time::timeout(
            Duration::from_secs(5),
            r.publish(durable(ch, i, &format!("m{i}"))),
        )
        .await
        .expect("publish STALLED — a slow subscriber must never block the shard")
        .unwrap();

        // Fast subscriber keeps receiving live with no stall.
        let env = tokio::time::timeout(Duration::from_secs(5), fast_stream.next())
            .await
            .expect("fast subscriber STALLED — fan-out was blocked by the slow one")
            .expect("fast stream ended early");
        seen.push(env.event_id.0.as_u128());
    }

    let expected: Vec<u128> = (1..=N).collect();
    assert_eq!(
        seen, expected,
        "fast subscriber got every event live, in order"
    );

    // The slow subscriber was marked lagged (its bounded channel overflowed
    // after BUFFER undrained events).
    assert!(
        slow_lag.is_lagged(),
        "the undrained subscriber must be marked lagged (§3.5)"
    );

    // The slow subscriber can resume from the sink via its cursor. It never
    // consumed anything, so resuming from None must yield all 200 from the
    // durable tier (ring may also hold recent, but resume merges + dedups).
    // Give write-behind a moment to flush everything to the sink.
    let mut waited = 0;
    while owner.sink.len(ch) < N as usize && waited < 5000 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += 5;
    }
    let resumed = r.resume_from_cursor(ch, None).await.unwrap();
    let resumed_markers: Vec<u128> = resumed.iter().map(|e| e.event_id.0.as_u128()).collect();
    assert_eq!(
        resumed_markers, expected,
        "lagged subscriber resumes the full stream from its cursor, no dup, in order"
    );
}
