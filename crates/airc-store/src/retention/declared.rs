//! The `events` rows each kind's declared retention says are dead
//! ([`airc_core::TranscriptKind::retention`]): ONE predicate built from the declarations, so the
//! drain has no per-kind code and a new kind is drained by being defined.
//!
//! The predicate reads only the row's own columns (`kind`, `room_id`, `peer_id`, `lamport`),
//! never its header rows, so it holds for every row, including those an older writer stored
//! before the header rows existed. Each window is one set-based statement through the ORM: no
//! row is selected into the process.

use airc_core::{Retention, RetentionKey, TranscriptKind};
use sea_orm::sea_query::{Alias, Expr, Query, SelectStatement, SimpleExpr};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};

use super::{Checkpointed, Pace, Window, WindowStats};
use crate::entities::{event, event_header};
use crate::error::StoreError;
use crate::sqlite::SqliteEventStore;

/// Rows of `events` some declaration says are dead, as one predicate; `None` when every kind is
/// history.
pub(super) fn dead_rows() -> Option<SimpleExpr> {
    TranscriptKind::ALL_VARIANTS
        .iter()
        .filter_map(|kind| dead_of(*kind))
        .reduce(SimpleExpr::or)
}

/// A read on derived data never derives a row the next pass deletes (`crate::derive::Dropped`).
impl crate::derive::Dropped for event::Entity {
    fn dropped() -> Option<SimpleExpr> {
        dead_rows()
    }
}

/// `bus_events` has no declared retention: its one drain class (heartbeats) is a header
/// predicate, which an owed row (no header rows yet) cannot answer, so a read derives it.
impl crate::derive::Dropped for crate::entities::bus_event::Entity {
    fn dropped() -> Option<SimpleExpr> {
        None
    }
}

/// The rows of `kind` its declaration says are dead.
fn dead_of(kind: TranscriptKind) -> Option<SimpleExpr> {
    let of_kind = Expr::col((event::Entity, event::Column::Kind)).eq(kind.as_wire_str());
    match kind.retention() {
        Retention::History => None,
        Retention::DeadOnArrival => Some(of_kind),
        Retention::NewestPer(key) => Some(of_kind.and(Expr::exists(newer_of(kind, key)))),
    }
}

/// A newer row of `kind` with the same `key` as the outer `events` row. The room's order index
/// `(room_id, lamport, event_id)` serves it as a seek to the row's lamport and a short walk
/// forward to the next row of the same kind and key.
fn newer_of(kind: TranscriptKind, key: RetentionKey) -> SelectStatement {
    let newer = Alias::new("newer");
    let mut select = Query::select();
    select
        .expr(Expr::val(1))
        .from_as(event::Entity, newer.clone())
        .and_where(
            Expr::col((newer.clone(), event::Column::RoomId))
                .equals((event::Entity, event::Column::RoomId)),
        )
        .and_where(
            Expr::col((newer.clone(), event::Column::Lamport))
                .gt(Expr::col((event::Entity, event::Column::Lamport))),
        )
        .and_where(Expr::col((newer.clone(), event::Column::Kind)).eq(kind.as_wire_str()));
    match key {
        RetentionKey::Room => {}
        RetentionKey::RoomAndPeer => {
            select.and_where(
                Expr::col((newer, event::Column::PeerId))
                    .equals((event::Entity, event::Column::PeerId)),
            );
        }
    }
    select.to_owned()
}

impl SqliteEventStore {
    /// Delete the `events` rows matching `dead`, with their header rows, one rowid window at a
    /// time ([`Window`], sized from what the last one cost): each window is one short transaction under the write admission, and the pass
    /// yields between windows so queued appends and reads take the connection and the write
    /// lock. Bounded by the table's max rowid at the start: rows written during the pass are
    /// the next pass's.
    pub(super) async fn delete_declared_in_windows(
        &self,
        dead: &SimpleExpr,
        mut window: Window,
        pace: &mut Pace,
    ) -> Result<(u64, WindowStats), StoreError> {
        let db = self.connection();
        let rowid = || Expr::col((event::Entity, Alias::new("rowid")));
        let max_rowid: Option<Option<i64>> = event::Entity::find()
            .select_only()
            .column_as(rowid().max(), "max_rowid")
            .into_tuple()
            .one(db)
            .await?;
        let Some(max_rowid) = max_rowid.flatten() else {
            return Ok((0, WindowStats::default()));
        };
        let mut low = 0i64;
        let mut removed = 0u64;
        let mut stats = WindowStats {
            bar: pace.bar(),
            ..WindowStats::default()
        };
        while low < max_rowid {
            let high = low.saturating_add(window.rows());
            let in_window = rowid().gt(low).and(rowid().lte(high)).and(dead.clone());
            let write = self.admit_write().await?;
            // Timed from the grant: the window adapts to its own cost, never to its queueing.
            let started = std::time::Instant::now();
            let txn = db.begin().await?;
            // The header rows first, while the window still names the events they belong to.
            event_header::Entity::delete_many()
                .filter(
                    event_header::Column::EventId.in_subquery(
                        Query::select()
                            .column((event::Entity, event::Column::EventId))
                            .from(event::Entity)
                            .and_where(in_window.clone())
                            .to_owned(),
                    ),
                )
                .exec(&txn)
                .await?;
            let headers_done = started.elapsed();
            removed += event::Entity::delete_many()
                .filter(in_window)
                .exec(&txn)
                .await?
                .rows_affected;
            let events_done = started.elapsed();
            txn.commit().await?;
            drop(write);
            let held = started.elapsed();
            stats.add(headers_done, events_done - headers_done, held - events_done);
            // Sized by the deletes' work, never the commit's fixed sync (see [`super::Window`]).
            window.after(events_done);
            match self.checkpoint_after_window(held).await? {
                Checkpointed::Copied(took) => stats.checkpoint += took,
                Checkpointed::Pinned(_) => {
                    stats.stopped_pinned = true;
                    break;
                }
            }
            let (rested, slow) = self.rest_at_pace(pace).await?;
            stats.rested += rested;
            if let Some(layers) = slow {
                stats.slow(layers);
            }
            low = high;
            tokio::task::yield_now().await;
        }
        Ok((removed, stats))
    }
}
