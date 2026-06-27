//! Acceptance test 1 — no-gap cursor (§3.5, §11.1).
//!
//! A subscriber attaching mid-stream with a cursor receives EVERY event after
//! the cursor exactly once, including the replay→live seam, **including** the
//! case where a `Durable` event was evicted-pending from the ring (small ring
//! + delayed sink) — it must come from the sink, never be skipped.

mod common;

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;

use airc_bus::{Clock, Cursor, EventRouter, Filter, InMemoryDurableSink, RouterConfig, SeqSource};
use airc_core::RoomId;

use common::{durable, GatedSink};

/// Helper: drain `n` events from a stream with a timeout so a missed event
/// fails loud (a hang) rather than passing trivially. Items are `Arc<Envelope>`
/// — the zero-copy fan-out yields refcounted handles, not owned envelopes.
async fn take_n<S>(mut stream: S, n: usize) -> Vec<Arc<airc_bus::Envelope>>
where
    S: futures::Stream<Item = Arc<airc_bus::Envelope>> + Unpin,
{
    let mut out = Vec::with_capacity(n);
    for _ in 0..n {
        let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out waiting for an event — a gap/miss would hang here");
        out.push(next.expect("stream ended early — missing events"));
    }
    out
}

#[tokio::test]
async fn mid_stream_attach_sees_every_event_after_cursor_once() {
    let ch = RoomId::from_u128(0xabc);
    let owner = common::Owner::new(RouterConfig {
        ring_capacity: 1024,
        ..Default::default()
    });
    let r = &owner.router;

    // Publish 5 events; remember the cursor of the 2nd.
    let mut cursors = Vec::new();
    for i in 1..=5u128 {
        let seq = r.publish(durable(ch, i, &format!("m{i}"))).await.unwrap();
        cursors.push(Cursor::new(seq, airc_core::EventId::from_u128(i)));
    }
    let from = cursors[1]; // after the 2nd event

    // Attach mid-stream from `from`. Spawn live publishes that race the
    // attach/replay so the seam is genuinely exercised.
    let stream = r.subscribe(Filter::channel(ch), Some(from));
    futures::pin_mut!(stream);

    // Publish 3 more live, interleaved.
    for i in 6..=8u128 {
        r.publish(durable(ch, i, &format!("m{i}"))).await.unwrap();
    }

    // Expect events 3,4,5 (replay) + 6,7,8 (live) = 6 events, each once.
    let got = take_n(&mut stream, 6).await;
    let markers: Vec<u128> = got.iter().map(|e| e.event_id.0.as_u128()).collect();
    assert_eq!(
        markers,
        vec![3, 4, 5, 6, 7, 8],
        "every event strictly after the cursor, in order, exactly once"
    );

    // No-dup: assert no further event is immediately available (no replayed
    // event re-delivered as live).
    let extra = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
    assert!(extra.is_err(), "no duplicate at the seam");
}

#[tokio::test]
async fn evicted_pending_durable_is_served_from_sink_not_skipped() {
    // The §3.8 teeth: small ring + delayed sink. Events stay pinned (not
    // evictable) while the sink gate is shut; once opened, write-behind
    // persists + unpins, then capacity pressure evicts them from the ring.
    // A subscriber attaching from the very start must still get them — now
    // from the SINK, because the ring no longer holds them.
    let ch = RoomId::from_u128(0xeee);

    let backing = Arc::new(InMemoryDurableSink::new());
    let gated = Arc::new(GatedSink::new(backing.clone()));

    // Build a router with a TINY ring directly against the gated sink.
    let epoch_store = airc_bus::InMemoryEpochStore::new();
    let clock = airc_bus::ManualClock::new(1_700_000_000_000);
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let r = EventRouter::new(
        RouterConfig {
            ring_capacity: 2, // tiny: forces eviction once unpinned
            ..Default::default()
        },
        Arc::new(clock) as Arc<dyn Clock>,
        seq,
        gated.clone(),
    );

    // Publish 6 durable events while the sink gate is SHUT. They fan out and
    // ring, but write-behind blocks on append → all 6 stay pinned in the ring
    // (the §3.8 floor: ring grows past capacity 2 rather than drop unpersisted
    // durables).
    for i in 1..=6u128 {
        r.publish(durable(ch, i, &format!("m{i}"))).await.unwrap();
    }
    assert_eq!(
        r.pinned_in_ring(ch),
        6,
        "all unpersisted durables are pinned — ring exceeds capacity (§3.8 floor)"
    );
    assert_eq!(r.ring_len(ch), 6, "ring held all 6 above its nominal 2");

    // Open the gate: write-behind drains, persists all 6, unpins them, and the
    // ring evicts down toward capacity. Wait until the sink has all 6 and the
    // ring has shrunk (the eviction we need for the test to mean something).
    gated.open();
    let mut waited = 0;
    while (backing.len(ch) < 6 || r.ring_len(ch) > 2) && waited < 5000 {
        tokio::time::sleep(Duration::from_millis(5)).await;
        waited += 5;
    }
    assert_eq!(backing.len(ch), 6, "all durables persisted after gate open");
    assert!(
        r.ring_len(ch) <= 2,
        "ring evicted the now-persisted durables (so they are NOT in RAM)"
    );

    // Now attach from the beginning. The early events (1..=4) are gone from
    // the ring; they MUST be served from the sink. If the deep-replay leg were
    // removed (or the ring weren't authoritative-then-deep), they'd be skipped.
    let stream = r.subscribe(Filter::channel(ch), None);
    futures::pin_mut!(stream);
    let got = take_n(&mut stream, 6).await;
    let markers: Vec<u128> = got.iter().map(|e| e.event_id.0.as_u128()).collect();
    assert_eq!(
        markers,
        vec![1, 2, 3, 4, 5, 6],
        "evicted-pending durables come from the sink — none skipped (§3.8 no-gap)"
    );
}
