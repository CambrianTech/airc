//! Counting matching history in the store, never by paging events out of it.
//!
//! [`CountHistory`] is the narrow capability every [`crate::EventStore`] backing
//! carries: `COUNT`, `MIN(occurred_at_ms)` and `MAX(occurred_at_ms)` over the
//! rows in `(after, through]` of the given channels whose headers match a
//! [`HeaderFilter`]. The header predicate is the store's own query layer
//! ([`crate::header_query`], typed subqueries on the header entity); no event
//! body is read or allocated.

use async_trait::async_trait;
use sea_orm::sea_query::{Expr, SimpleExpr};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect};

use airc_core::{HeaderFilter, HistoryCount, RoomId, RuntimeSelf, TranscriptCursor};

use crate::entities::{event, event_header};
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
            .filter(crate::header_query::predicate::<event_header::Entity>(
                headers,
            ));
        if let Some(rule) = exclude {
            query =
                query.filter(crate::header_query::not_runtime_self::<event_header::Entity>(rule));
        }
        let row: Option<(i64, Option<i64>, Option<i64>)> = query
            .select_only()
            .column_as(Expr::col(event::Column::EventId).count(), "count")
            .column_as(Expr::col(event::Column::OccurredAtMs).min(), "oldest")
            .column_as(Expr::col(event::Column::OccurredAtMs).max(), "newest")
            .into_tuple()
            .one(self.connection())
            .await?;
        let Some((count, oldest, newest)) = row else {
            return Ok(HistoryCount::default());
        };
        Ok(HistoryCount {
            count: to_u64("count", count)?,
            oldest_ms: oldest.map(|ms| to_u64("occurred_at_ms", ms)).transpose()?,
            newest_ms: newest.map(|ms| to_u64("occurred_at_ms", ms)).transpose()?,
        })
    }
}
