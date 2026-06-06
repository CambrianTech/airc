//! The event router — hot-path, in-memory, sharded (§3.1, §3.2, §3.5, §3.8).
//!
//! Owns:
//! - a **sharded** per-channel subscriber index (mutex-striped: unrelated
//!   rooms never serialize on one lock, §3.1). DashMap was the spec's
//!   alternative; striped mutexes are chosen here because they keep the hot
//!   path deterministic and make "no lock across `.await`" a syntactic
//!   property (every lock is a sync `MutexGuard` in a non-async block).
//! - a per-channel **hot ring** (bounded, pinned-until-persisted floor, §3.2).
//! - a per-channel coalesced **ephemeral cache** (latest-wins + TTL, §3.4).
//! - a bounded **write-behind** path to the [`DurableSink`] (§3.3, §3.8).
//!
//! ## The two load-bearing invariants
//!
//! **No-gap cursor (§3.5).** `subscribe` registers the live sender *and*
//! snapshots the ring under the **same** shard lock, so no event slips between
//! "what replay saw" and "what live delivers." Replay then covers
//! `(from_cursor, ring.oldest)` from the sink (deep) + the ring snapshot
//! (recent); live delivery dedups by a replay high-watermark so the seam admits
//! no miss and no dup. A `Durable` event evicted-pending from the ring is still
//! in the ring (pinned, §3.8) *or* already in the sink — never neither — so the
//! deep leg can always cover it.
//!
//! **Slow subscriber (§3.5).** Fan-out is `try_send` into each subscriber's
//! bounded channel. A full channel marks that subscriber **lagged** and drops
//! the live push — it NEVER blocks the shard or the other subscribers. A lagged
//! subscriber resumes from the sink via its cursor. No lock is held across the
//! fan-out (it's all synchronous `try_send`), and certainly none across an
//! `.await`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use futures::stream::Stream;
use tokio::sync::mpsc;

use airc_core::RoomId;

use crate::clock::Clock;
use crate::envelope::{Cursor, Envelope};
use crate::ephemeral::EphemeralCache;
use crate::filter::Filter;
use crate::ring::HotRing;
use crate::seq::SeqSource;
use crate::sink::DurableSink;

/// Tunables for an [`EventRouter`].
#[derive(Debug, Clone)]
pub struct RouterConfig {
    /// Number of shards (mutex stripes). Rooms hash onto a shard; unrelated
    /// rooms on different shards never serialize.
    pub shards: usize,
    /// Nominal per-channel hot-ring capacity. The §3.8 floor means a ring may
    /// temporarily exceed this while un-persisted `Durable` entries are pinned.
    pub ring_capacity: usize,
    /// Bound on each subscriber's live channel. Full = subscriber is lagged.
    pub subscriber_buffer: usize,
    /// Bound on the write-behind queue (§3.8 ≥ ring floor).
    pub write_behind_buffer: usize,
    /// Ephemeral coalesced-entry TTL in ms (§3.4).
    pub ephemeral_ttl_ms: u64,
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            shards: 16,
            ring_capacity: 256,
            subscriber_buffer: 1024,
            write_behind_buffer: 1024,
            ephemeral_ttl_ms: 30_000,
        }
    }
}

/// A registered subscriber's live delivery handle, held in the channel's
/// subscriber list.
struct SubscriberHandle {
    tx: mpsc::Sender<Arc<Envelope>>,
    filter: Filter,
    /// Set when a `try_send` failed because the bounded channel was full
    /// (§3.5). Shared with the subscriber's stream so it can observe "I
    /// lagged, resume from the sink."
    lagged: Arc<AtomicBool>,
}

/// Per-channel state: hot ring + ephemeral cache + subscriber list. Lives
/// inside a shard, behind that shard's mutex.
struct ChannelState {
    ring: HotRing,
    ephemeral: EphemeralCache,
    subscribers: Vec<SubscriberHandle>,
}

impl ChannelState {
    fn new(ring_capacity: usize, ephemeral_ttl_ms: u64) -> Self {
        Self {
            ring: HotRing::new(ring_capacity),
            ephemeral: EphemeralCache::new(ephemeral_ttl_ms),
            subscribers: Vec::new(),
        }
    }
}

/// One shard: a map of channel -> state, behind one mutex.
struct Shard {
    channels: Mutex<HashMap<u128, ChannelState>>,
}

/// A durable envelope queued for the write-behind task, paired with the shard
/// index so the task can re-lock and unpin the ring entry on confirmation.
struct WriteBehindItem {
    env: Arc<Envelope>,
}

/// The owner-core event router (§3).
///
/// Cheap to clone via the `Arc` fields; clones share the same router.
#[derive(Clone)]
pub struct EventRouter {
    inner: Arc<RouterInner>,
}

struct RouterInner {
    shards: Vec<Shard>,
    config: RouterConfig,
    clock: Arc<dyn Clock>,
    seq: Arc<SeqSource>,
    sink: Arc<dyn DurableSink>,
    write_behind_tx: mpsc::Sender<WriteBehindItem>,
    /// Count of events shed because the write-behind queue was saturated and
    /// the publisher was fire-and-forget (§3.8). Surfaced for diagnostics.
    shed_count: AtomicU64,
    /// Number of channel-state maps that have ever been created (across all
    /// shards) — the many-rooms test reads this as the allocation proxy.
    channels_created: AtomicU64,
}

impl EventRouter {
    /// Build a router and spawn its write-behind task.
    ///
    /// `seq` is built once per daemon start (it has already bumped the epoch).
    /// `sink` is the durable tier behind the trait. The write-behind task runs
    /// until the router (and all clones) drop.
    pub fn new(
        config: RouterConfig,
        clock: Arc<dyn Clock>,
        seq: Arc<SeqSource>,
        sink: Arc<dyn DurableSink>,
    ) -> Self {
        let shards = (0..config.shards.max(1))
            .map(|_| Shard {
                channels: Mutex::new(HashMap::new()),
            })
            .collect();

        let (write_behind_tx, write_behind_rx) =
            mpsc::channel::<WriteBehindItem>(config.write_behind_buffer.max(1));

        let inner = Arc::new(RouterInner {
            shards,
            config,
            clock,
            seq,
            sink,
            write_behind_tx,
            shed_count: AtomicU64::new(0),
            channels_created: AtomicU64::new(0),
        });

        // Write-behind task: drains durable envelopes, persists each, then
        // re-locks the owning shard and unpins the ring entry (§3.8). This is
        // the ONLY place that holds an `.await` near the durable tier, and it
        // never holds a shard lock across it.
        let task_inner = Arc::clone(&inner);
        tokio::spawn(async move {
            Self::run_write_behind(task_inner, write_behind_rx).await;
        });

        Self { inner }
    }

    fn shard_for(&self, channel: RoomId) -> &Shard {
        let n = self.inner.shards.len();
        let idx = (channel.0.as_u128() % n as u128) as usize;
        &self.inner.shards[idx]
    }

    /// Publish an envelope (§4 publish-hot). Returns the assigned [`Seq`].
    ///
    /// Steps, all synchronous up to the write-behind enqueue:
    /// 1. Stamp owner metadata: `seq` (generational) + `occurred_at_ms`.
    /// 2. Under the shard lock: push to the ring (deliver-first), coalesce if
    ///    `EphemeralLatest`, fan out to matching subscribers via `try_send`
    ///    (lagging ones marked, never blocking). Release the lock.
    /// 3. Off the lock: enqueue `Durable` envelopes to write-behind.
    ///
    /// [`Seq`]: crate::Seq
    pub async fn publish(&self, mut env: Envelope) -> crate::Result<crate::Seq> {
        let seq = self.inner.seq.next();
        env.seq = seq;
        env.occurred_at_ms = self.inner.clock.now_ms();

        // Wrap ONCE: from here on every delivery (ring, ephemeral, fan-out,
        // write-behind) is an `Arc::clone` — a refcount bump, never a deep copy
        // of the envelope or its `headers` BTreeMap. This is the hot-path
        // zero-copy keystone: per-subscriber CPU is one atomic increment.
        let env = Arc::new(env);

        // --- synchronous hot path: NO await while the shard lock is held ---
        {
            let shard = self.shard_for(env.channel);
            let mut map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            let key = env.channel.0.as_u128();
            let is_new = !map.contains_key(&key);
            let state = map.entry(key).or_insert_with(|| {
                ChannelState::new(
                    self.inner.config.ring_capacity,
                    self.inner.config.ephemeral_ttl_ms,
                )
            });
            if is_new {
                self.inner.channels_created.fetch_add(1, Ordering::SeqCst);
            }

            if env.delivery.is_ephemeral_latest() {
                // §3.4: coalesce latest-wins; do NOT ring it (it's a
                // projection, not a log) and do NOT persist it.
                state
                    .ephemeral
                    .coalesce(Arc::clone(&env), env.occurred_at_ms);
            } else {
                // Recent log: ring it (pins if Durable, §3.8).
                state.ring.push(Arc::clone(&env));
            }

            // Fan out live to matching subscribers. try_send only — a slow
            // subscriber is marked lagged, never blocks the shard (§3.5). Each
            // send is an `Arc::clone` (refcount bump), NOT a deep copy.
            //
            // Card 800ce5bd: per-publish observability. The fan-out is the
            // load-bearing path that carries chat from `airc msg` to attached
            // subscribers, and it has been opaque — when chat doesn't arrive,
            // we couldn't tell whether the publish reached this loop, how
            // many subscribers were considered, which were filtered out,
            // which were closed. INFO so `RUST_LOG=airc_bus=info` turns it
            // on for diagnosis without flooding the operator at default level.
            let subscribers_before = state.subscribers.len();
            let mut matched = 0usize;
            let mut sent_ok = 0usize;
            let mut sent_lagged = 0usize;
            let mut sent_closed = 0usize;
            state.subscribers.retain(|sub| {
                if !sub.filter.matches(&env) {
                    return true; // not for this subscriber, keep it
                }
                matched += 1;
                match sub.tx.try_send(Arc::clone(&env)) {
                    Ok(()) => {
                        sent_ok += 1;
                        true
                    }
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Lagged: drop the live push, flag it; the subscriber
                        // resumes from the sink via its cursor.
                        sub.lagged.store(true, Ordering::SeqCst);
                        sent_lagged += 1;
                        true
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => {
                        // Receiver gone -> drop the handle.
                        sent_closed += 1;
                        false
                    }
                }
            });
            tracing::info!(
                channel = %env.channel,
                epoch = env.seq.epoch,
                counter = env.seq.counter,
                kind = ?env.kind,
                delivery = ?env.delivery,
                subscribers_before,
                matched,
                sent_ok,
                sent_lagged,
                sent_closed,
                "airc-bus publish: fan-out summary"
            );
        } // shard lock released here, before any await

        // --- write-behind (durable only), off the hot lock ---
        if env.delivery.is_durable() {
            match self.inner.write_behind_tx.try_send(WriteBehindItem {
                env: Arc::clone(&env),
            }) {
                Ok(()) => {}
                Err(mpsc::error::TrySendError::Full(_)) => {
                    // §3.8: bounded write-behind full. Slice-1 fire-and-forget
                    // policy: shed + surface, never silently drop, never OOM.
                    // (The `await_durable`/blocking publisher variant is a
                    // later refinement; the default path sheds with a surfaced
                    // error so the contract is explicit.)
                    self.inner.shed_count.fetch_add(1, Ordering::SeqCst);
                    return Err(crate::BusError::WriteBehindSaturated);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    return Err(crate::BusError::Sink("write-behind task gone".into()));
                }
            }
        }

        Ok(seq)
    }

    /// The write-behind drain loop (§3.3 deliver-first / persist-async, §3.8
    /// ring-pinned-until-persisted). For each durable item: `sink.append` then
    /// re-lock the shard and `mark_persisted` so the ring may evict it.
    async fn run_write_behind(inner: Arc<RouterInner>, mut rx: mpsc::Receiver<WriteBehindItem>) {
        while let Some(item) = rx.recv().await {
            // append is the only point we touch the durable tier; no shard
            // lock is held across it.
            let appended = inner.sink.append(&item.env).await;
            if appended.is_err() {
                // Append failed: leave the ring entry pinned so the no-gap
                // precondition still holds (the event is still in RAM). A real
                // sink would retry; the in-memory test sink never fails.
                continue;
            }
            // Confirmed persisted -> unpin in the ring.
            let n = inner.shards.len();
            let idx = (item.env.channel.0.as_u128() % n as u128) as usize;
            let shard = &inner.shards[idx];
            let mut map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(state) = map.get_mut(&item.env.channel.0.as_u128()) {
                state.ring.mark_persisted(item.env.event_id);
            }
        }
    }

    /// **Card 7d5b6a65.** Return the cursor of the most-recent envelope
    /// in the channel's ring snapshot, or `None` if the channel has not
    /// yet received any envelope.
    ///
    /// Callers use this to implement "subscribe from the live edge":
    /// pass the returned cursor as `from_cursor` to [`Self::subscribe`]
    /// and the deep-replay + ring-snapshot legs return empty (their
    /// `is_after` predicate filters them out), so the subscriber gets
    /// only events published strictly after the call. This is the
    /// agent-Monitor live-tail shape — the `AttachRequest::from_now`
    /// flag.
    pub fn head_cursor(&self, channel: airc_core::RoomId) -> Option<Cursor> {
        let n = self.inner.shards.len();
        let idx = (channel.0.as_u128() % n as u128) as usize;
        let shard = &self.inner.shards[idx];
        let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
        map.get(&channel.0.as_u128())
            .and_then(|state| state.ring.newest_cursor())
    }

    /// **Card 7d5b6a65.** Async fallback for [`Self::head_cursor`] that
    /// queries the durable sink. Callers use this when the in-memory
    /// ring is empty (fresh daemon start with backlog in the sink):
    ///
    /// ```ignore
    /// let from = match router.head_cursor(channel) {
    ///     Some(c) => Some(c),
    ///     None => router.sink_head_cursor(channel).await,
    /// };
    /// ```
    ///
    /// Without this fallback, an `AttachRequest::from_now: true`
    /// against a freshly-started daemon would fall through to
    /// `from: None` and replay the whole sink — the exact bug card
    /// 7d5b6a65 closes.
    pub async fn sink_head_cursor(&self, channel: airc_core::RoomId) -> Option<Cursor> {
        self.inner.sink.head_cursor(channel).await.ok().flatten()
    }

    /// Subscribe (§4 subscribe, §3.5 cursor contract).
    ///
    /// Returns a stream that yields **every** envelope on the filter strictly
    /// after `from_cursor`, exactly once, with no gap at the replay→live seam:
    ///
    /// 1. Under the shard lock, register the live sender *and* snapshot the
    ///    ring's replay + oldest cursor atomically. (Registering first means
    ///    no live event published after this moment can be missed.)
    /// 2. Off the lock, page the sink for the deep leg `(from_cursor,
    ///    ring.oldest)` — covering anything older than the ring still retains,
    ///    including a `Durable` event that was evicted-pending and is now in
    ///    the sink (§3.8).
    /// 3. Yield deep replay, then the ring snapshot (historical), tracking the
    ///    highest cursor emitted.
    /// 4. Drain the live channel, skipping anything at-or-before the replay
    ///    high-watermark (dedup) — the seam admits no dup; step-1 ordering
    ///    admits no miss.
    pub fn subscribe(
        &self,
        filter: Filter,
        from_cursor: Option<Cursor>,
    ) -> impl Stream<Item = Arc<Envelope>> {
        self.subscribe_with_lag(filter, from_cursor).0
    }

    /// Like [`EventRouter::subscribe`] but also returns a [`LagFlag`] the
    /// caller can poll to learn whether the router dropped a live push to this
    /// subscriber (§3.5 slow-subscriber). When lagged, the caller resumes via
    /// [`EventRouter::resume_from_cursor`].
    pub fn subscribe_with_lag(
        &self,
        filter: Filter,
        from_cursor: Option<Cursor>,
    ) -> (impl Stream<Item = Arc<Envelope>>, LagFlag) {
        let inner = Arc::clone(&self.inner);
        let channel = filter.channel;

        // --- step 1: register live + snapshot ring under one lock ---
        let lagged = Arc::new(AtomicBool::new(false));
        let buffer_capacity = inner.config.subscriber_buffer.max(1);
        let (tx, mut rx) = mpsc::channel::<Arc<Envelope>>(buffer_capacity);
        let (ring_snapshot, ring_oldest, shard_idx, subscribers_after) = {
            let n = inner.shards.len();
            let idx = (channel.0.as_u128() % n as u128) as usize;
            let shard = &inner.shards[idx];
            let mut map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            let key = channel.0.as_u128();
            let is_new = !map.contains_key(&key);
            let state = map.entry(key).or_insert_with(|| {
                ChannelState::new(inner.config.ring_capacity, inner.config.ephemeral_ttl_ms)
            });
            if is_new {
                inner.channels_created.fetch_add(1, Ordering::SeqCst);
            }
            state.subscribers.push(SubscriberHandle {
                tx,
                filter: filter.clone(),
                lagged: Arc::clone(&lagged),
            });
            // Snapshot recent replay + the oldest cursor still in RAM, while
            // holding the same lock that gated live registration.
            (
                state.ring.replay_after(from_cursor),
                state.ring.oldest_cursor(),
                idx,
                state.subscribers.len(),
            )
        }; // lock released; now we may await

        // Card 800ce5bd: per-registration observability. Pairs with the
        // per-publish summary so an operator can correlate "I subscribed at
        // T=X on shard=Y for channel=Z" with "at T=X+ε a publish landed on
        // shard=Y for channel=Z and saw N subscribers." A subscribe that
        // doesn't appear in a subsequent publish's `subscribers_before` is
        // the smoking gun for "wrong shard / wrong channel / wrong state."
        tracing::info!(
            channel = %channel,
            shard_idx,
            subscribers_after,
            buffer_capacity,
            filter_summary = ?filter,
            "airc-bus subscribe_with_lag: registered"
        );

        let lag_flag = LagFlag(Arc::clone(&lagged));
        let stream = async_stream::stream! {
            // --- step 2: deep replay leg from the sink ---
            // The sink covers `(from_cursor, ring_oldest)`: events older than
            // the ring still retains. If the ring is non-empty we page the sink
            // up to (but not including) the ring's oldest; if the ring is empty
            // we page the whole tail after the cursor.
            let mut high: Option<Cursor> = from_cursor;
            let deep = inner
                .sink
                .page(channel, from_cursor, usize::MAX)
                .await
                .unwrap_or_default();
            for env in deep {
                // The sink (persistence) is a real copy boundary, so the deep
                // leg arrives as owned `Envelope`s; wrap each once in `Arc` so
                // the stream item type is uniform with the (already-`Arc`) ring
                // and live legs and downstream stays zero-copy.
                let env = Arc::new(env);
                // Only emit sink events strictly before the ring snapshot's
                // window — the ring snapshot is authoritative for the recent
                // tail (it may hold un-persisted Durable the sink lacks).
                let before_ring = match ring_oldest {
                    Some(o) => env.cursor().is_before(&o),
                    None => true,
                };
                let after_gate = match high {
                    Some(h) => env.cursor().is_after(&h),
                    None => true,
                };
                if before_ring && after_gate && filter.matches(&env) {
                    high = Some(env.cursor());
                    yield env;
                }
            }

            // --- step 3: recent replay leg from the ring snapshot ---
            for env in ring_snapshot {
                let after_gate = match high {
                    Some(h) => env.cursor().is_after(&h),
                    None => true,
                };
                if after_gate && filter.matches(&env) {
                    high = Some(env.cursor());
                    yield env;
                }
            }

            // --- step 4: live, deduped against the replay high-watermark ---
            while let Some(env) = rx.recv().await {
                let after_gate = match high {
                    Some(h) => env.cursor().is_after(&h),
                    None => true,
                };
                if after_gate {
                    high = Some(env.cursor());
                    yield env;
                }
                // events at-or-before `high` were already delivered by replay;
                // dropping them is the no-dup half of the seam.
            }
        };
        (stream, lag_flag)
    }

    /// Deep + recent catch-up for a **lagged** subscriber (§3.5): everything
    /// strictly after its last cursor, recent-from-ring + deep-from-sink,
    /// merged in total order with no dup. A lagging `Durable` subscriber calls
    /// this to resume; fan-out to others was never blocked.
    pub async fn resume_from_cursor(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        // Recent from ring (snapshot under lock; no await held).
        let (ring_events, ring_oldest) = {
            let shard = self.shard_for(channel);
            let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            match map.get(&channel.0.as_u128()) {
                Some(state) => (
                    state.ring.replay_after(from_cursor),
                    state.ring.oldest_cursor(),
                ),
                None => (Vec::new(), None),
            }
        };

        // Deep from sink for the window older than the ring.
        let deep = self
            .inner
            .sink
            .page(channel, from_cursor, usize::MAX)
            .await?;

        let mut out: Vec<Arc<Envelope>> = Vec::new();
        for env in deep {
            // Deep leg is owned `Envelope` from the sink copy boundary; wrap
            // once so the merged output is uniformly `Arc<Envelope>`.
            let env = Arc::new(env);
            let before_ring = match ring_oldest {
                Some(o) => env.cursor().is_before(&o),
                None => true,
            };
            let after_gate = match &from_cursor {
                Some(h) => env.cursor().is_after(h),
                None => true,
            };
            if before_ring && after_gate {
                out.push(env);
            }
        }
        out.extend(ring_events);
        // Total order, dedup by event_id (a Durable may appear in both legs if
        // it was persisted between the two snapshots).
        out.sort_by(|a, b| {
            a.seq
                .cmp(&b.seq)
                .then_with(|| a.event_id.0.cmp(&b.event_id.0))
        });
        out.dedup_by(|a, b| a.event_id == b.event_id);
        Ok(out)
    }

    /// The latest coalesced ephemeral value for `(channel, coalesce_key)`, if
    /// live (§3.4). Used to seed a freshly-attached subscriber with current
    /// presence/pose.
    pub fn ephemeral_latest(&self, channel: RoomId, key: &str) -> Option<Arc<Envelope>> {
        let now = self.inner.clock.now_ms();
        let shard = self.shard_for(channel);
        let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
        map.get(&channel.0.as_u128())
            .and_then(|s| s.ephemeral.get(key, now).map(Arc::clone))
    }

    /// Number of distinct channels that have ever had state allocated. The
    /// many-rooms test uses this as the allocation/wakeup proxy: idle channels
    /// that were merely *named* (never published/subscribed) cost nothing.
    pub fn channels_created(&self) -> u64 {
        self.inner.channels_created.load(Ordering::SeqCst)
    }

    /// Count of fire-and-forget durable events shed under write-behind
    /// saturation (§3.8). Always counted, never silent.
    pub fn shed_count(&self) -> u64 {
        self.inner.shed_count.load(Ordering::SeqCst)
    }

    /// Test/diagnostic: pinned (un-persisted `Durable`) entries in a channel's
    /// ring — the live un-persisted backlog (§3.8 floor).
    pub fn pinned_in_ring(&self, channel: RoomId) -> usize {
        let shard = self.shard_for(channel);
        let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
        map.get(&channel.0.as_u128())
            .map_or(0, |s| s.ring.pinned_count())
    }

    /// Test/diagnostic: snapshot the count of retained ring entries.
    pub fn ring_len(&self, channel: RoomId) -> usize {
        let shard = self.shard_for(channel);
        let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
        map.get(&channel.0.as_u128()).map_or(0, |s| s.ring.len())
    }
}

/// A handle a subscriber polls to learn it has lagged (§3.5). Returned by
/// [`EventRouter::subscribe_with_lag`] alongside the stream.
#[derive(Clone)]
pub struct LagFlag(Arc<AtomicBool>);

impl LagFlag {
    /// True once the router dropped a live push to this subscriber. The
    /// subscriber then resumes via [`EventRouter::resume_from_cursor`].
    pub fn is_lagged(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}
