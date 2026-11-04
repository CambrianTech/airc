//! Whether a file answers filtered reads exactly ([`airc_core::Settlement`]), per file and shared
//! by every handle on it, and how a filtered read stays in milliseconds until it does.
//!
//! A filtered read is exact when two things are true of its file: every row retention keeps has
//! its derived data (`crate::derive`), and the indexes the filters search exist
//! (`crate::indexes`). A store that never ran migration 000021 has neither until its background
//! task derives (at open) and its first pass builds the indexes after the drain. On a copy of a
//! resident's 1.55 GB store (2026-11-04), an inbox read before then walked all 2.2M rows: 62.6 s,
//! and 13.2 s even after the derivation, for want of the indexes.
//!
//! So until the file is settled, a filtered read walks only its newest rows: one statement over
//! the rows above `max(rowid) - span`, the span sized by the drain's own [`Window`] (one owner of
//! the statement budget) from the reads before it, and the read says Unsettled. When both facts
//! hold, [`Settling::subscribe`] fires once, so a reader re-reads instead of polling.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use airc_core::Settlement;
use sea_orm::sea_query::{Alias, Expr};
use sea_orm::{DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Select};

use crate::error::StoreError;
use crate::event_table::EventTable;
use crate::retention::Window;

/// One file's settlement.
#[derive(Debug)]
pub(crate) struct Settling {
    /// Every row retention keeps has its derived data.
    derived: AtomicBool,
    /// Every index a filtered read searches exists.
    indexed: AtomicBool,
    settled: tokio::sync::watch::Sender<Settlement>,
    /// The newest rows one unsettled read walks, learned from the reads before it.
    walk: Mutex<Window>,
}

impl Default for Settling {
    fn default() -> Self {
        Self {
            derived: AtomicBool::new(false),
            indexed: AtomicBool::new(false),
            settled: tokio::sync::watch::channel(Settlement::Unsettled).0,
            walk: Mutex::new(Window::adaptive()),
        }
    }
}

impl Settling {
    pub(crate) fn settlement(&self) -> Settlement {
        if self.derived.load(Ordering::Acquire) && self.indexed.load(Ordering::Acquire) {
            Settlement::Settled
        } else {
            Settlement::Unsettled
        }
    }

    /// Every row retention keeps is derived.
    pub(crate) fn mark_derived(&self) {
        self.derived.store(true, Ordering::Release);
        self.publish();
    }

    /// Every index a filtered read searches exists.
    pub(crate) fn mark_indexed(&self) {
        self.indexed.store(true, Ordering::Release);
        self.publish();
    }

    /// Fires once the file is settled; it never goes back.
    pub(crate) fn subscribe(&self) -> tokio::sync::watch::Receiver<Settlement> {
        self.settled.subscribe()
    }

    fn publish(&self) {
        let now = self.settlement();
        self.settled.send_if_modified(|was| {
            let changed = *was != now;
            *was = now;
            changed
        });
    }

    /// Run a read of `T`. A read on derived data while the file is unsettled reaches only the
    /// newest span of rows ([`Reach::Newest`]), whose time sizes the next span; any other read
    /// reaches them all ([`Reach::Whole`]). Returns the read's result and its settlement.
    pub(crate) async fn read<T, R, E, Run, Ran>(
        &self,
        db: &DatabaseConnection,
        reads_derived: bool,
        lift: fn(StoreError) -> E,
        run: Run,
    ) -> Result<(R, Settlement), E>
    where
        T: EventTable,
        Run: FnOnce(Reach<T>) -> Ran,
        Ran: std::future::Future<Output = Result<R, E>>,
    {
        if !reads_derived || self.settlement() == Settlement::Settled {
            return Ok((run(Reach::Whole).await?, Settlement::Settled));
        }
        let rowid = || Expr::col((T::default(), Alias::new("rowid")));
        let newest: Option<Option<i64>> = T::find()
            .select_only()
            .column_as(rowid().max(), "max_rowid")
            .into_tuple()
            .one(db)
            .await
            .map_err(|e| lift(e.into()))?;
        let span = self.walk.lock().unwrap_or_else(|p| p.into_inner()).rows(); // a poisoned lock still holds a valid span
        let rows: Vec<i64> = match newest.flatten() {
            Some(newest) => (newest.saturating_sub(span).saturating_add(1)..=newest).collect(),
            None => Vec::new(),
        };
        let started = std::time::Instant::now();
        let result = run(Reach::Newest(Box::new(
            T::find().filter(rowid().is_in(rows)),
        )))
        .await?;
        self.walk
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .after(started.elapsed());
        Ok((result, Settlement::Unsettled))
    }
}

/// What a filtered read may touch.
pub(crate) enum Reach<T: EntityTrait> {
    /// Every row: the file is settled, or the read reads no derived data. The read runs its own
    /// indexed statements.
    Whole,
    /// The newest span of rows, selected by rowid alone, which SQLite looks up by its integer
    /// key. The read matches them in memory (`crate::matched`, `airc_bus::matched`), needing no
    /// derived data or index. Beside the read's own filters SQLite would walk the room index
    /// whole instead: a rowid range, an event-id list and a rowid list all did, the last by a
    /// multi-index OR over the cursor range (9.0 s on a 2.2M-row copy of a resident's store).
    Newest(Box<Select<T>>),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::event;
    use sea_orm::QueryTrait as _;

    // what this catches (card 70055c76): an unsettled read bounded beside its own filters, which
    // SQLite answered by walking the room index whole (9.0 s on a 2.2M-row copy of a resident's
    // store, for a rowid range, an event-id list and a rowid list alike). The span is selected by
    // rowid alone, and SQLite reaches it by the integer key, never by a scan or the room index.
    #[tokio::test]
    async fn an_unsettled_span_is_reached_by_rowid_never_by_a_scan() {
        use crate::store::EventStore;
        let store = crate::SqliteEventStore::in_memory().await.unwrap();
        for lamport in 1..=300u64 {
            store
                .append(airc_core::TranscriptEvent {
                    event_id: airc_core::EventId::new(),
                    room_id: airc_core::RoomId::from_u128(lamport as u128 % 42),
                    peer_id: airc_core::PeerId::from_u128(0xa1),
                    client_id: airc_core::ClientId::from_u128(0xc1),
                    kind: airc_core::TranscriptKind::Message,
                    occurred_at_ms: lamport,
                    lamport,
                    target: airc_core::MentionTarget::All,
                    headers: airc_core::Headers::new(),
                    body: Some(airc_core::Body::text("ambient")),
                    attachment: None,
                    receipt: None,
                    metadata: serde_json::Value::Null,
                })
                .await
                .unwrap();
        }
        let db = store.connection();
        let settling = Settling::default();
        let plan = settling
            .read::<event::Entity, _, StoreError, _, _>(
                db,
                true,
                std::convert::identity,
                |reach| async move {
                    let Reach::Newest(select) = reach else {
                        panic!("an unsettled read on derived data reaches the newest span")
                    };
                    crate::sqlite_only::query_plan(db, select.build(sea_orm::DbBackend::Sqlite))
                        .await
                },
            )
            .await
            .unwrap();
        let (plan, settlement) = plan;
        assert_eq!(settlement, Settlement::Unsettled);
        assert!(
            plan.iter()
                .all(|step| !step.starts_with("SCAN") && !step.contains("idx_events_room")),
            "by key only: {plan:?}"
        );
    }
}
