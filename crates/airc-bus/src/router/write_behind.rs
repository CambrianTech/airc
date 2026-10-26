//! The router's write-behind drain: persist retained batches, then release ring pins.

use super::*;

impl EventRouter {
    /// The write-behind drain loop (§3.3 deliver-first / persist-async, §3.8
    /// ring-pinned-until-persisted). Confirm each retained batch with
    /// `sink.append_batch`, then re-lock its shards and `mark_persisted` so
    /// the ring may evict the durables. Failure never abandons pinned work.
    pub(super) async fn run_write_behind(
        inner: Arc<RouterInner>,
        mut rx: mpsc::Receiver<WriteBehindItem>,
    ) {
        // GROUP-COMMIT: when a burst is queued, drain everything immediately
        // available (up to MAX_BATCH) and persist it in ONE durable commit, so
        // a fsync-backed sink pays one fsync per batch instead of one per event.
        // Under light load this collapses to single-item appends (recv blocks,
        // try_recv finds nothing) — same latency as before; the win is purely
        // under the burst contention the daemon-path bench measures.
        const MAX_BATCH: usize = 64;
        while let Some(first) = rx.recv().await {
            let mut batch = Vec::with_capacity(MAX_BATCH);
            batch.push(first);
            while batch.len() < MAX_BATCH {
                match rx.try_recv() {
                    Ok(item) => batch.push(item),
                    Err(_) => break, // queue drained (empty or closed)
                }
            }

            // append is the only point we touch the durable tier; no shard
            // lock is held across it. Keep this batch until confirmed: the
            // sink contract is idempotent by event_id, including implementations
            // whose default append_batch partially succeeds before an error.
            // Do not drain newer work while retrying. The existing bounded
            // admission queue then refuses excess publishers BEFORE fan-out.
            let envs: Vec<&Envelope> = batch.iter().map(|i| i.env.as_ref()).collect();
            let mut failed_attempts = 0u64;
            let mut retry_delay = WRITE_RETRY_INITIAL;
            while let Err(error) = inner.sink.append_batch(&envs).await {
                failed_attempts = failed_attempts.saturating_add(1);
                // Log the first failure and exponentially spaced reminders,
                // not one diagnostic per attempt during a prolonged outage.
                if failed_attempts.is_power_of_two() {
                    inner.diagnostics.emit(
                        DiagnosticEvent::error(
                            DiagnosticComponent::Persistence,
                            DiagnosticCode::WriteBehindBatchFailed,
                            "durable batch retained for retry; publishers remain bounded",
                        )
                        .with_field("failed_attempts", failed_attempts)
                        .with_field("batch_len", batch.len())
                        .with_field("pinned_count", Self::pinned_count(&inner))
                        .with_field("retry_ms", retry_delay.as_millis())
                        .with_field("error", error),
                    );
                }
                tokio::time::sleep(retry_delay).await;
                retry_delay = retry_delay.saturating_mul(2).min(WRITE_RETRY_MAX);
            }

            // Confirmed persisted -> unpin each in its shard's ring.
            let n = inner.shards.len();
            for item in &batch {
                let idx = (item.env.channel.0.as_u128() % n as u128) as usize;
                let shard = &inner.shards[idx];
                let mut map = shard.channels.lock().unwrap_or_else(|p| p.into_inner());
                if let Some(state) = map.get_mut(&item.env.channel.0.as_u128()) {
                    state.ring.mark_persisted(item.env.event_id);
                }
            }
            if failed_attempts != 0 {
                inner.diagnostics.emit(
                    DiagnosticEvent::new(
                        DiagnosticSeverity::Info,
                        DiagnosticComponent::Persistence,
                        DiagnosticCode::WriteBehindBatchRecovered,
                        "retained durable batch committed and unpinned",
                    )
                    .with_field("failed_attempts", failed_attempts)
                    .with_field("batch_len", batch.len())
                    .with_field("pinned_count", Self::pinned_count(&inner)),
                );
            }
        }
    }

    // Diagnostic snapshot only: acquire one shard at a time, never across an
    // await. Keep counting off the healthy publish/persist hot paths.
    pub(super) fn pinned_count(inner: &RouterInner) -> usize {
        inner
            .shards
            .iter()
            .map(|shard| {
                shard
                    .channels
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .values()
                    .map(|state| state.ring.pinned_count())
                    .sum::<usize>()
            })
            .sum()
    }
}
