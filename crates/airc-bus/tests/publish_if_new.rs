//! Card 4132f48c — `EventRouter::publish_if_new` idempotency pins.
//!
//! Inbound transport frames carry sender-minted event ids and can
//! legitimately reach the router more than once (same frame on two
//! LAN links, a wire echo of a locally published event, a re-inject
//! after a daemon restart). The plain `publish` assumes a fresh id;
//! `publish_if_new` is the ingest entry point that makes the second
//! arrival a no-op at EVERY tier — live fan-out, hot ring, and sink.

mod common;

use std::time::Duration;

use futures::StreamExt;

use airc_bus::{Filter, PublishIfNew, RouterConfig};
use airc_core::RoomId;

use common::{durable, Owner};

#[tokio::test]
async fn second_arrival_of_same_event_id_is_duplicate_at_every_tier() {
    let ch = RoomId::from_u128(0x4132);
    let owner = Owner::new(RouterConfig::default());
    let r = &owner.router;

    let stream = r.subscribe(Filter::channel(ch), None);
    futures::pin_mut!(stream);

    let first = r
        .publish_if_new(durable(ch, 0xA, "from the wire"))
        .await
        .unwrap();
    assert!(
        matches!(first, PublishIfNew::Published(_)),
        "fresh id must publish; got {first:?}"
    );
    let second = r
        .publish_if_new(durable(ch, 0xA, "from the wire"))
        .await
        .unwrap();
    assert_eq!(
        second,
        PublishIfNew::Duplicate,
        "the same event_id arriving again (second LAN link) must not re-publish"
    );

    // Live tier: exactly one delivery reaches the subscriber. Publish a
    // sentinel after the duplicate; if the duplicate had fanned out, it
    // would arrive before the sentinel.
    r.publish(durable(ch, 0xB, "sentinel")).await.unwrap();
    let first_seen = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("first event")
        .expect("stream open");
    assert_eq!(first_seen.payload.as_ref(), b"from the wire");
    let next_seen = tokio::time::timeout(Duration::from_secs(5), stream.next())
        .await
        .expect("second event")
        .expect("stream open");
    assert_eq!(
        next_seen.payload.as_ref(),
        b"sentinel",
        "the duplicate must NOT appear between the original and the sentinel"
    );

    // Durable tier: one row.
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(owner.sink.len(ch), 2, "original + sentinel, no dup row");
}

#[tokio::test]
async fn wire_echo_of_a_locally_published_event_is_duplicate() {
    // The local-sender + LAN-receiver shape: a scope publishes through
    // the daemon (plain `publish`), then the same event id comes back
    // over a transport link (`publish_if_new`).
    let ch = RoomId::from_u128(0x4133);
    let owner = Owner::new(RouterConfig::default());
    let r = &owner.router;

    r.publish(durable(ch, 0xC, "local original")).await.unwrap();
    let echo = r
        .publish_if_new(durable(ch, 0xC, "local original"))
        .await
        .unwrap();
    assert_eq!(
        echo,
        PublishIfNew::Duplicate,
        "plain publishes must be visible to the idempotency window"
    );

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(owner.sink.len(ch), 1);
}

#[tokio::test]
async fn already_durable_event_is_duplicate_even_for_a_fresh_router() {
    // The post-restart shape: the in-memory recent-ids window is gone,
    // but the event is in the durable tier — `DurableSink::contains`
    // is the leg that catches it.
    let ch = RoomId::from_u128(0x4134);
    let owner = Owner::new(RouterConfig::default());
    owner
        .router
        .publish(durable(ch, 0xD, "before restart"))
        .await
        .unwrap();
    // Let write-behind persist before "restarting".
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(owner.sink.len(ch), 1);

    let restarted = Owner::with_parts(
        RouterConfig::default(),
        owner.epoch_store.clone(),
        owner.sink.clone(),
        0,
    );
    let outcome = restarted
        .router
        .publish_if_new(durable(ch, 0xD, "before restart"))
        .await
        .unwrap();
    assert_eq!(
        outcome,
        PublishIfNew::Duplicate,
        "a durably persisted id must never re-enter the ring/live tiers"
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(restarted.sink.len(ch), 1, "still exactly one row");
}

// --- failure does not poison the idempotency window ------------------------

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use airc_bus::envelope::{Cursor, Envelope};
use airc_bus::{
    BusError, Clock, DurableSink, EventRouter, InMemoryDurableSink, InMemoryEpochStore, SeqSource,
    SystemClock,
};
use airc_core::EventId;
use async_trait::async_trait;

/// A sink whose `contains` fails exactly once — the transient-store
/// shape a retried inbound frame hits.
struct FlakyContainsSink {
    inner: Arc<InMemoryDurableSink>,
    fail_next_contains: AtomicBool,
}

#[async_trait]
impl DurableSink for FlakyContainsSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        self.inner.append(e).await
    }

    async fn page(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.inner.page(channel, from_cursor, limit).await
    }

    async fn head_cursor(&self, channel: RoomId) -> Result<Option<Cursor>, BusError> {
        self.inner.head_cursor(channel).await
    }

    async fn page_tail(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.inner.page_tail(channel, before, limit).await
    }

    async fn contains(&self, event_id: EventId) -> Result<bool, BusError> {
        if self.fail_next_contains.swap(false, Ordering::SeqCst) {
            return Err(BusError::Sink("transient store outage".into()));
        }
        self.inner.contains(event_id).await
    }
}

#[tokio::test]
async fn failed_publish_does_not_poison_the_window_for_a_retry() {
    let ch = RoomId::from_u128(0x4135);
    let sink = Arc::new(FlakyContainsSink {
        inner: Arc::new(InMemoryDurableSink::new()),
        fail_next_contains: AtomicBool::new(true),
    });
    let epoch_store = InMemoryEpochStore::new();
    let seq = Arc::new(SeqSource::start(&epoch_store));
    let router = EventRouter::new(
        RouterConfig::default(),
        Arc::new(SystemClock) as Arc<dyn Clock>,
        seq,
        sink.clone(),
    );

    // First attempt fails on the durable probe.
    let first = router.publish_if_new(durable(ch, 0xE, "retry me")).await;
    assert!(first.is_err(), "transient sink outage must surface");

    // The retry must PUBLISH — a poisoned recent-ids entry would
    // report Duplicate for an event the router never took (and the
    // receiver would ack delivered falsely).
    let second = router
        .publish_if_new(durable(ch, 0xE, "retry me"))
        .await
        .unwrap();
    assert!(
        matches!(second, PublishIfNew::Published(_)),
        "retry after failure must publish, got {second:?}"
    );
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(sink.inner.len(ch), 1, "the retried event is durable once");
}
