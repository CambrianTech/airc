//! The router's durable reads: cursors, tails, filtered history pages and counts.

use super::*;

impl EventRouter {
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

    /// **Card a1562dbc.** The channel's **durable transcript tip**: the
    /// cursor of the newest `Durable` envelope on `channel`, or `None`
    /// for a room with no durable history. Constant work regardless of
    /// room depth — this is the O(1) probe behind the `room_tip` IPC op
    /// (reconnect watermarking, idle detection, projection-cache
    /// validation), replacing the "replay the whole room, keep the last
    /// event" scan.
    ///
    /// Two constant-cost legs, NOT a perf fallback:
    /// - the hot ring's newest durable entry (bounded by ring capacity).
    ///   Eviction is oldest-first, so when the ring holds any durable,
    ///   its newest durable IS the channel's global durable tip — the
    ///   sink only lags the ring (write-behind), never leads it.
    /// - otherwise the sink's [`DurableSink::head_cursor`] (one indexed
    ///   row on SQLite). A sink error propagates — it is never papered
    ///   over with a scan.
    pub async fn durable_tip(&self, channel: airc_core::RoomId) -> crate::Result<Option<Cursor>> {
        let ring_tip = {
            let shard = self.shard_for(channel);
            let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            map.get(&channel.0.as_u128())
                .and_then(|state| state.ring.newest_durable_cursor())
        }; // shard lock released before the sink await
        if ring_tip.is_some() {
            return Ok(ring_tip);
        }
        self.inner.sink.head_cursor(channel).await
    }

    /// **Card 8428ae8c.** The newest `limit` **durable** envelopes on
    /// `channel`, in ascending total order — the "most recent N" leg of
    /// the inbox path. Work is bounded by `limit` and the ring capacity,
    /// NEVER by room depth: the previous shape
    /// (`resume_from_cursor(channel, None)` + truncate) materialized the
    /// entire room to answer N.
    ///
    /// Merge contract (mirrors [`Self::subscribe`]'s seam, reversed):
    /// - the hot ring is authoritative for the recent tail — it may hold
    ///   durables the sink lacks (write-behind lag), and eviction is
    ///   oldest-first with §3.8 pinning (a durable is never evicted
    ///   before it is persisted), so the ring's durables are exactly the
    ///   channel's newest durables and everything older is in the sink;
    /// - when the ring's durables don't cover `limit`, the remainder
    ///   comes from ONE [`DurableSink::page_tail`] call for the events
    ///   strictly before the ring's oldest retained cursor (`None` =
    ///   channel tail when the ring is empty — fresh-start shape). The
    ///   sink only ever holds durables, so no class filter is needed on
    ///   that leg. A sink error propagates — it is never papered over
    ///   with a full forward scan.
    pub async fn durable_tail(
        &self,
        channel: RoomId,
        limit: usize,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        self.durable_tail_filtered(channel, None, None, limit).await
    }

    /// A bounded durable page strictly before a position in THIS router's
    /// channel order. The cursor must come from an earlier page of this same
    /// owner and channel; another machine's ingest position is unrelated.
    pub async fn durable_tail_before(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        self.durable_tail_filtered(channel, before, None, limit)
            .await
    }

    /// **Continuum #297.** [`Self::durable_tail`] restricted to
    /// envelopes whose [`Kind`] is in `kinds` — "the newest N durable
    /// events OF THESE KINDS", with the kind predicate applied on both
    /// legs (ring scan + [`DurableSink::page_tail_of_kinds`]) BEFORE the
    /// limit. The inbox path that keeps a chunk-flooded room's page
    /// from evicting every `Message`: newest-N-raw-then-filter yields
    /// crumbs; this yields the newest N matching.
    ///
    /// `kinds` must be non-empty — filtering to nothing is never what a
    /// caller means, and the IPC boundary normalizes empty to the
    /// unfiltered [`Self::durable_tail`]. An empty slice here returns
    /// an empty page (the vacuous filter, honestly answered).
    pub async fn durable_tail_of_kinds(
        &self,
        channel: RoomId,
        kinds: &[Kind],
        limit: usize,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        self.durable_tail_filtered(channel, None, Some(kinds), limit)
            .await
    }

    /// [`Self::durable_tail_before`] restricted to `kinds`: the newest `limit`
    /// durable events of these kinds strictly before `before`. The backward
    /// page of `Inbox { before, kinds }`.
    pub async fn durable_tail_before_of_kinds(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: &[Kind],
        limit: usize,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        self.durable_tail_filtered(channel, before, Some(kinds), limit)
            .await
    }

    /// The one merge implementation behind [`Self::durable_tail`] /
    /// [`Self::durable_tail_of_kinds`] — `kinds: None` means no kind
    /// predicate. See `durable_tail` for the merge contract; the kind
    /// filter composes with it unchanged (ring durables of a kind are
    /// exactly the channel's newest durables of that kind, everything
    /// older is in the sink).
    pub(super) async fn durable_tail_filtered(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        kinds: Option<&[Kind]>,
        limit: usize,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        let mut filter = crate::Filter::channel(channel);
        if let Some(kinds) = kinds {
            filter = filter.with_kinds(kinds.to_vec());
        }
        self.filtered_tail_before(channel, before, limit, &filter)
            .await
    }

    /// Complete subscription predicate, applied in both ring and store before LIMIT.
    pub async fn filtered_tail_before(
        &self,
        channel: RoomId,
        before: Option<Cursor>,
        limit: usize,
        filter: &crate::Filter,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let (mut tail, ring_oldest) = {
            let shard = self.shard_for(channel);
            let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            match map.get(&channel.0.as_u128()) {
                Some(state) => (
                    state
                        .ring
                        .replay_after(None)
                        .into_iter()
                        .filter(|env| {
                            env.delivery.is_durable()
                                && filter.matches(env)
                                && before.is_none_or(|gate| env.cursor().is_before(&gate))
                        })
                        .collect::<Vec<_>>(),
                    state.ring.oldest_cursor(),
                ),
                None => (Vec::new(), None),
            }
        }; // shard lock released before the sink await
        if tail.len() >= limit {
            // The ring alone covers N — zero store reads.
            return Ok(tail.split_off(tail.len() - limit));
        }
        // Ring durables are the newest; top up with the sink tail older
        // than anything the ring still retains. Sink rows strictly
        // before `ring_oldest` cannot also be in the ring, so the two
        // legs concatenate with no dup and no gap.
        let remainder = limit - tail.len();
        // A page wholly behind the hot ring must retain the caller's older
        // bound. Otherwise the sink would repeat the same newest stored page.
        let sink_before = match (before, ring_oldest) {
            (Some(before), Some(oldest)) if before.is_before(&oldest) => Some(before),
            (_, Some(oldest)) => Some(oldest),
            (before, None) => before,
        };
        let older = self
            .inner
            .sink
            .filtered_tail_before(channel, sink_before, remainder, filter)
            .await?;
        let mut out: Vec<Arc<Envelope>> = older.into_iter().map(Arc::new).collect();
        out.append(&mut tail);
        Ok(out)
    }

    /// Deep + recent catch-up for a **lagged** subscriber (§3.5): everything
    /// strictly after its last cursor, recent-from-ring + deep-from-sink,
    /// merged in total order with no dup. A lagging `Durable` subscriber calls
    /// this to resume; fan-out to others was never blocked.
    ///
    /// `limit` bounds the answer AND the work: the deep leg is one indexed
    /// `LIMIT limit` query (never the channel's whole remainder), and the
    /// merged page is cut to `limit` in total order. Pagination is the
    /// caller's job — pass the last returned cursor back in. Measured
    /// 2026-09-06 (continuum M5, the wall projection): with `usize::MAX`
    /// here every 500-event inbox page materialized the entire remaining
    /// transcript (academy 244k rows, cambriantech 472k), so a from-zero
    /// walk was O(n²/page) and never returned inside the callers' 30 s
    /// deadline, while the board — resuming from a saved cursor — was instant.
    /// Snapshot-bounded durable page filtered by `rule` (headers, and addressee
    /// when set) before the limit. Hot pending writes remain visible.
    pub async fn matching_history(
        &self,
        channel: RoomId,
        after: Cursor,
        through: Cursor,
        rule: airc_core::HistoryMatch<'_>,
        limit: usize,
        tail: bool,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        if limit == 0 {
            return Ok(Vec::new());
        }
        let mut rows = {
            let shard = self.shard_for(channel);
            let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            map.get(&channel.0.as_u128())
                .map(|state| {
                    state
                        .ring
                        .replay_after(None)
                        .into_iter()
                        .filter(|e| {
                            e.delivery.is_durable()
                                && e.cursor().is_after(&after)
                                && !e.cursor().is_after(&through)
                                && e.matches_history(&rule)
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        rows.extend(
            self.inner
                .sink
                .page_matching(channel, after, through, rule, limit, tail)
                .await?
                .into_iter()
                .map(Arc::new),
        );
        rows.sort_by_key(|e| (e.seq, e.event_id.0));
        rows.dedup_by_key(|e| e.event_id);
        if tail {
            rows.reverse();
        }
        rows.truncate(limit);
        if tail {
            rows.reverse();
        }
        Ok(rows)
    }

    pub async fn resume_from_cursor(
        &self,
        channel: RoomId,
        from_cursor: Option<Cursor>,
        limit: usize,
    ) -> crate::Result<Vec<Arc<Envelope>>> {
        let limit = limit.max(1);
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
        let deep = self.inner.sink.page(channel, from_cursor, limit).await?;

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
        // Deep rows all precede ring rows in total order, so the first `limit`
        // of the merge are exactly the first `limit` after the cursor.
        out.truncate(limit);
        Ok(out)
    }

    /// Count what [`Self::matching_history`] would return over `(after, through]`,
    /// with the oldest and newest `occurred_at_ms`, without reading events out:
    /// the sink counts its durable rows in the store, and only ring events newer
    /// than the sink's head (written, not yet persisted) are counted in memory.
    /// Splitting at the sink head counts each event once.
    pub async fn count_matching(
        &self,
        channel: RoomId,
        after: Cursor,
        through: Cursor,
        rule: airc_core::HistoryMatch<'_>,
        exclude: Option<&airc_core::RuntimeSelf>,
    ) -> crate::Result<airc_core::HistoryCount> {
        let sink_head = self.inner.sink.head_cursor(channel).await?;
        let durable_through = match sink_head {
            Some(head) if through.is_after(&head) => head,
            Some(_) => through,
            None => after,
        };
        let durable = if durable_through.is_after(&after) {
            self.inner
                .sink
                .count_matching(channel, after, durable_through, rule.clone(), exclude)
                .await?
        } else {
            airc_core::HistoryCount::default()
        };
        let pending_after = if durable_through.is_after(&after) {
            durable_through
        } else {
            after
        };
        let pending = {
            let shard = self.shard_for(channel);
            let map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
            map.get(&channel.0.as_u128())
                .map(|state| {
                    state
                        .ring
                        .replay_after(None)
                        .into_iter()
                        .filter(|e| {
                            e.delivery.is_durable()
                                && e.cursor().is_after(&pending_after)
                                && !e.cursor().is_after(&through)
                                && e.matches_history(&rule)
                                && !exclude.is_some_and(|rule| rule.is_self(&e.headers, e.from.1))
                        })
                        .fold(airc_core::HistoryCount::default(), |count, e| {
                            count.with_event(e.occurred_at_ms)
                        })
                })
                .unwrap_or_default()
        };
        Ok(durable.merge(pending))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::DeliveryClass;

    // what this catches: the count splits at the sink head, so an event is counted
    // once whether it is still pending in the ring or already persisted, and header
    // routing applies on both legs.
    #[tokio::test]
    async fn count_is_exact_across_ring_and_sink_and_routes_by_class() {
        let sink = Arc::new(crate::InMemoryDurableSink::new());
        let router = EventRouter::new(
            RouterConfig::default(),
            Arc::new(crate::ManualClock::new(7)),
            Arc::new(SeqSource::start_at_counter(
                &crate::InMemoryEpochStore::new(),
                0,
            )),
            sink.clone(),
        );
        let room = RoomId::new();
        let from = (PeerId::new(), airc_core::ClientId::new());
        let mut last = None;
        for n in 0..6 {
            let mut env = Envelope::new(
                room,
                from,
                Kind::Message,
                airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
                DeliveryClass::Durable,
                bytes::Bytes::from_static(b"x"),
            );
            if n == 0 {
                env.kind = Kind::Event;
                env.class = airc_core::EventClass::Activity(airc_core::ActivityKind::Presence);
            }
            let id = env.event_id;
            let seq = router.publish(env).await.unwrap();
            last = Some(Cursor::new(seq, id));
        }
        let through = last.unwrap();
        let after = Cursor::new(crate::Seq::new(0, 0), airc_core::EventId::from_u128(0));
        let all_headers = airc_core::HeaderFilter::Any;
        let kinds = std::collections::BTreeSet::new();
        let routed = airc_core::HistoryMatch {
            sender: None,
            kinds: &kinds,
            self_echo: None,
            selection: airc_core::EventSelection::Correspondence,
            headers: &all_headers,
            addressed_to: None,
        };
        let expect = |count| airc_core::HistoryCount {
            count,
            oldest_ms: Some(7),
            newest_ms: Some(7),
        };
        let any = airc_core::HeaderFilter::Any;
        assert_eq!(
            router
                .count_matching(
                    room,
                    after,
                    through,
                    airc_core::HistoryMatch::headers(&any),
                    None
                )
                .await
                .unwrap(),
            expect(6)
        );
        assert_eq!(
            router
                .count_matching(room, after, through, routed.clone(), None)
                .await
                .unwrap(),
            expect(5)
        );
        for _ in 0..200 {
            if sink.head_cursor(room).await.unwrap() == Some(through) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert_eq!(
            sink.head_cursor(room).await.unwrap(),
            Some(through),
            "persisted"
        );
        // The runtime-self rule applies on both legs: every event here is this
        // client's own unstamped send, so excluding self leaves nothing.
        let me = airc_core::RuntimeSelf::new(from.1, None);
        let own = router
            .count_matching(
                room,
                after,
                through,
                airc_core::HistoryMatch::headers(&any),
                Some(&me),
            )
            .await
            .unwrap();
        assert_eq!(own.count, 0);
        assert_eq!(
            router
                .count_matching(
                    room,
                    after,
                    through,
                    airc_core::HistoryMatch::headers(&any),
                    None
                )
                .await
                .unwrap(),
            expect(6)
        );
        assert_eq!(
            router
                .count_matching(room, after, through, routed.clone(), None)
                .await
                .unwrap(),
            expect(5)
        );
    }
}
