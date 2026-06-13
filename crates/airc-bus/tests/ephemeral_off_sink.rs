//! Acceptance test 4 — ephemeral off the sink (§3.4, §11.1 ephemeral-off-ORM).
//!
//! 1000 `EphemeralLatest` updates with the same coalesce_key. Assert
//! `DurableSink::append` was called 0 times and a subscriber sees only the
//! latest value (TTL respected).

mod common;

use std::time::Duration;

use futures::StreamExt;

use airc_bus::{Filter, RouterConfig};
use airc_core::RoomId;

use common::{ephemeral, Owner};

#[tokio::test]
async fn ephemeral_firehose_never_touches_the_sink_and_coalesces() {
    let ch = RoomId::from_u128(0xef);
    let owner = Owner::new(RouterConfig {
        ephemeral_ttl_ms: 10_000,
        ..Default::default()
    });
    let r = &owner.router;

    // 1000 presence/typing updates, all coalescing on one key.
    for i in 1..=1000u128 {
        r.publish(ephemeral(ch, i, "typing:alice", &i.to_le_bytes()))
            .await
            .unwrap();
    }

    // Give the (idle) write-behind task a chance to run — it must have nothing
    // to do, because ephemerals are never enqueued to it.
    tokio::time::sleep(Duration::from_millis(30)).await;

    // (1) The sink was NEVER appended to. This is the efficiency keystone:
    // if ephemerals were (wrongly) enqueued to write-behind, this would be
    // 1000, not 0.
    assert_eq!(
        owner.sink.append_count(),
        0,
        "EphemeralLatest must never reach the durable tier (§3.4)"
    );
    assert_eq!(
        owner.sink.len(ch),
        0,
        "no durable rows for ephemeral traffic"
    );

    // (2) The coalesced cache holds exactly the latest value.
    let latest = r
        .ephemeral_latest(ch, "typing:alice")
        .expect("a live coalesced value");
    assert_eq!(
        latest.payload.as_ref(),
        &1000u128.to_le_bytes(),
        "latest-wins: only the final update survives coalescing"
    );

    // (3) A subscriber that attaches now and then sees one more update gets
    // the latest live value (not 1000 separate frames). Attach, publish one
    // more, observe exactly that one.
    let stream = r.subscribe(Filter::channel(ch), None);
    futures::pin_mut!(stream);
    r.publish(ephemeral(ch, 1001, "typing:alice", &1001u128.to_le_bytes()))
        .await
        .unwrap();
    let next = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("ephemeral live push timed out")
        .expect("stream ended");
    assert_eq!(
        next.payload.as_ref(),
        &1001u128.to_le_bytes(),
        "subscriber sees the latest ephemeral live"
    );

    // (4) TTL respected: advance the clock past the TTL; the cached value
    // expires.
    owner.clock.advance(10_000);
    assert!(
        r.ephemeral_latest(ch, "typing:alice").is_none(),
        "ephemeral entry expires after its TTL (§3.4)"
    );
}
