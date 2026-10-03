//! Opt-in isolated burst measurement. No live daemon, network, or account data.

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind};
use airc_bus::{
    BusError, DurableSink, EventRouter, InMemoryDurableSink, InMemoryEpochStore, ManualClock,
    RouterConfig, SeqSource,
};
use airc_core::{ClientId, EventId, PeerId, RoomId};
use async_trait::async_trait;
use bytes::Bytes;
use std::sync::Arc;
use std::time::{Duration, Instant};
struct DelayedSink {
    inner: Arc<InMemoryDurableSink>,
}

impl DelayedSink {
    fn new(inner: Arc<InMemoryDurableSink>) -> Self {
        Self { inner }
    }
}

#[async_trait]
impl DurableSink for DelayedSink {
    async fn append(&self, e: &Envelope) -> Result<(), BusError> {
        self.inner.append(e).await
    }

    async fn append_batch(&self, events: &[&Envelope]) -> Result<(), BusError> {
        tokio::time::sleep(Duration::from_millis(10)).await;
        self.inner.append_batch(events).await
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

    async fn page_tail_of_kinds(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: &[Kind],
        limit: usize,
    ) -> Result<Vec<Envelope>, BusError> {
        self.inner
            .page_tail_of_kinds(channel, before, kinds, limit)
            .await
    }

    async fn contains(&self, event_id: airc_core::EventId) -> Result<bool, BusError> {
        self.inner.contains(event_id).await
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "manual bounded burst measurement; --ignored --nocapture"]
async fn burst_slow_storage() {
    for total in [256usize, 2048, 8192] {
        let durable = Arc::new(InMemoryDurableSink::new());
        let sink = Arc::new(DelayedSink::new(durable.clone()));
        let router = EventRouter::new(
            RouterConfig::default(),
            Arc::new(ManualClock::new(1)),
            Arc::new(SeqSource::start(&InMemoryEpochStore::new())),
            sink,
        );
        let channel = RoomId::new();
        let start = Instant::now();
        let mut latency = Vec::with_capacity(total);
        let mut accepted = 0usize;
        let mut saturated = 0usize;
        for i in 0..total {
            let e = Envelope::new(
                channel,
                (PeerId::from_u128(1), ClientId::from_u128(1)),
                Kind::Message,
                DeliveryClass::Durable,
                Bytes::from_static(b"burst payload"),
            )
            .with_event_id(EventId::from_u128(i as u128 + 1));
            let at = Instant::now();
            match router.publish(e).await {
                Ok(_) => accepted += 1,
                Err(BusError::WriteBehindSaturated) => saturated += 1,
                Err(error) => panic!("unexpected publish error: {error}"),
            }
            latency.push(at.elapsed().as_nanos());
        }
        let publish_wall = start.elapsed();
        let pinned_after_burst = router.pinned_in_ring(channel);
        tokio::time::timeout(Duration::from_secs(30), async {
            while durable.len(channel) < accepted || router.pinned_in_ring(channel) != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("accepted events did not persist before drain deadline");
        latency.sort_unstable();
        println!("burst total={total} accepted={accepted} saturated={saturated} publish_ms={} attempts_per_sec={} publish_ns_p50={} p95={} p99={} drain_total_ms={} persisted={} pinned_after_burst={pinned_after_burst} pinned_after_drain={}", publish_wall.as_millis(), total as f64 / publish_wall.as_secs_f64(), latency[total/2], latency[total*95/100], latency[total*99/100], start.elapsed().as_millis(), durable.len(channel), router.pinned_in_ring(channel));
        assert_eq!(accepted + saturated, total);
        assert_eq!(durable.len(channel), accepted);
        assert_eq!(router.shed_count(), saturated as u64);
        assert_eq!(
            router.pinned_in_ring(channel),
            0,
            "rejected events must not remain pinned"
        );
    }
}
