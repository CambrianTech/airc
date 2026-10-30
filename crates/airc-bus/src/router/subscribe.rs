//! Subscribing to the router: deep replay, ring snapshot, then live, with no gap or dup.

use super::*;

impl EventRouter {
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
    ) -> impl Stream<Item = crate::Result<Arc<Envelope>>> {
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
    ) -> (impl Stream<Item = crate::Result<Arc<Envelope>>>, LagFlag) {
        self.subscribe_mode(filter, from_cursor, false)
    }

    /// Register at the live edge without reading or decoding transcript history.
    pub fn subscribe_live_with_lag(
        &self,
        filter: Filter,
    ) -> (impl Stream<Item = crate::Result<Arc<Envelope>>>, LagFlag) {
        self.subscribe_mode(filter, None, true)
    }

    pub(super) fn subscribe_mode(
        &self,
        filter: Filter,
        from_cursor: Option<Cursor>,
        live_only: bool,
    ) -> (impl Stream<Item = crate::Result<Arc<Envelope>>>, LagFlag) {
        let inner = Arc::clone(&self.inner);
        let channel = filter.channel;
        let id = inner.next_subscription.fetch_add(1, Ordering::Relaxed);
        let registration = SubscriptionGuard {
            router: Arc::downgrade(&inner),
            channel,
            id,
            headers: filter.headers.clone(),
        };

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
            let state = map
                .entry(key)
                .or_insert_with(|| ChannelState::new(&inner.config));
            if is_new {
                inner.channels_created.fetch_add(1, Ordering::SeqCst);
            }
            state.subscribers.insert(
                id,
                &filter.headers,
                SubscriberHandle {
                    tx,
                    filter: filter.clone(),
                    lagged: Arc::clone(&lagged),
                },
            );
            // Snapshot recent replay + the oldest cursor still in RAM, while
            // holding the same lock that gated live registration.
            (
                if live_only {
                    Vec::new()
                } else {
                    state.ring.replay_after(from_cursor)
                },
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
        let stream = async_stream::try_stream! {
            // Captured even before first poll: dropping an unpolled stream
            // unregisters immediately, with no later publish needed for cleanup.
            let _registration = registration;
            // --- step 2: deep replay leg from the sink ---
            // The durable owner applies the same typed predicate before LIMIT.
            // A failed read terminates with an error; never advance past missing history.
            const DEEP_REPLAY_PAGE: usize = 1024;
            let mut high: Option<Cursor> = from_cursor;
            if !live_only {
                let mut page_from: Option<Cursor> = from_cursor;
                'deep: loop {
                    let deep = inner
                        .sink
                        .filtered_page_after(channel, page_from, DEEP_REPLAY_PAGE, &filter)
                        .await?;
                    let full_page = deep.len() >= DEEP_REPLAY_PAGE;
                    let last_cursor = deep.last().map(|e| e.cursor());
                    for env in deep {
                        let cursor = env.cursor();
                        // Hand off to step 3 once we reach the ring snapshot's
                        // window — it is authoritative from there on (it may hold
                        // un-persisted Durable the sink lacks). Rows arrive in total
                        // order, so this break ends the whole leg.
                        if matches!(ring_oldest, Some(o) if !cursor.is_before(&o)) {
                            break 'deep;
                        }
                        let after_gate = match high {
                            Some(h) => cursor.is_after(&h),
                            None => true,
                        };
                        // The sink (persistence) is a real copy boundary, so the deep
                        // leg arrives as owned `Envelope`s; wrap each once in `Arc`
                        // so the stream item type stays uniform with the
                        // (already-`Arc`) ring and live legs.
                        let env = Arc::new(env);
                        if after_gate {
                            high = Some(cursor);
                            yield env;
                        }
                    }
                    if !full_page {
                        break 'deep;
                    }
                    page_from = last_cursor;
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
}
