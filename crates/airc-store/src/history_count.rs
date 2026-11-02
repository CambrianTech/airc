//! Counting matching history in the store, never by paging events out of it.
//!
//! [`CountHistory`] is the narrow capability every [`crate::EventStore`] backing
//! carries: how many rows in `(after, through]` of the given channels match a
//! [`HeaderFilter`], read newest first and stopping past `HISTORY_COUNT_CAP`. The header predicate is the store's own query layer
//! ([`crate::header_query`], typed subqueries on the header entity); no event
//! body is read or allocated.

use async_trait::async_trait;
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};

use airc_core::{HeaderFilter, HistoryCount, RoomId, RuntimeSelf, TranscriptCursor};

use crate::entities::event;
use crate::error::StoreError;
use crate::sqlite::SqliteEventStore;

#[async_trait]
pub trait CountHistory: Send + Sync {
    /// Matching events strictly after `after` and at or before `through`,
    /// without `exclude`'s own sends.
    async fn count_history(
        &self,
        after: &TranscriptCursor,
        through: &TranscriptCursor,
        channels: &[RoomId],
        headers: &HeaderFilter,
        exclude: Option<&RuntimeSelf>,
    ) -> Result<HistoryCount, StoreError>;
}

/// `(after, through]` in transcript order: lamport, then event id.
fn transcript_range(after: &TranscriptCursor, through: &TranscriptCursor) -> SimpleExpr {
    let lamport = |c: &TranscriptCursor| c.lamport as i64;
    let after_range = Expr::col(event::Column::Lamport)
        .gt(lamport(after))
        .or(Expr::col(event::Column::Lamport)
            .eq(lamport(after))
            .and(Expr::col(event::Column::EventId).gt(after.event_id.as_uuid())));
    let through_range = Expr::col(event::Column::Lamport)
        .lt(lamport(through))
        .or(Expr::col(event::Column::Lamport)
            .eq(lamport(through))
            .and(Expr::col(event::Column::EventId).lte(through.event_id.as_uuid())));
    after_range.and(through_range)
}

fn to_u64(field: &'static str, value: i64) -> Result<u64, StoreError> {
    u64::try_from(value).map_err(|_| StoreError::InvalidStoredValue {
        field,
        value: i128::from(value),
    })
}

#[async_trait]
impl CountHistory for SqliteEventStore {
    async fn count_history(
        &self,
        after: &TranscriptCursor,
        through: &TranscriptCursor,
        channels: &[RoomId],
        headers: &HeaderFilter,
        exclude: Option<&RuntimeSelf>,
    ) -> Result<HistoryCount, StoreError> {
        let mut query = event::Entity::find()
            .filter(event::Column::RoomId.is_in(channels.iter().map(|room| room.as_uuid())))
            .filter(transcript_range(after, through))
            .filter(crate::header_query::predicate::<event::Entity>(headers));
        if let Some(rule) = exclude {
            query = query.filter(crate::header_query::not_runtime_self::<event::Entity>(rule));
        }
        // Newest first, at most one past the cap: the count never walks the whole range.
        let times: Vec<i64> = query
            .select_only()
            .column(event::Column::OccurredAtMs)
            .order_by_desc(event::Column::Lamport)
            .order_by_desc(event::Column::EventId)
            .limit(airc_core::HISTORY_COUNT_CAP + 1)
            .into_tuple()
            .all(self.connection())
            .await?;
        let times = times
            .into_iter()
            .map(|ms| to_u64("occurred_at_ms", ms))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(HistoryCount::of_newest(times))
    }
}
