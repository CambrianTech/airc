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

use super::Window;
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
    ) -> Result<u64, StoreError> {
        let db = self.connection();
        let rowid = || Expr::col((event::Entity, Alias::new("rowid")));
        let max_rowid: Option<Option<i64>> = event::Entity::find()
            .select_only()
            .column_as(rowid().max(), "max_rowid")
            .into_tuple()
            .one(db)
            .await?;
        let Some(max_rowid) = max_rowid.flatten() else {
            return Ok(0);
        };
        let mut low = 0i64;
        let mut removed = 0u64;
        while low < max_rowid {
            let high = low.saturating_add(window.rows());
            let started = std::time::Instant::now();
            let in_window = rowid().gt(low).and(rowid().lte(high)).and(dead.clone());
            let write = self.admit_write().await?;
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
            removed += event::Entity::delete_many()
                .filter(in_window)
                .exec(&txn)
                .await?
                .rows_affected;
            txn.commit().await?;
            drop(write);
            let took = started.elapsed();
            window.after(took);
            low = high;
            window.rest(took).await;
        }
        Ok(removed)
    }
}
