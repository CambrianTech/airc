//! Counting matching history in the store, never by paging events out of it.
//!
//! [`CountHistory`] is the narrow capability every [`crate::EventStore`] backing
//! carries: how many rows in `(after, through]` of the given channels match a
//! [`HeaderFilter`], read newest first and stopping past `HISTORY_COUNT_CAP`. The header predicate is the store's own query layer
//! ([`crate::header_query`], typed subqueries on the header entity); no event
//! body is read or allocated.

use async_trait::async_trait;
use sea_orm::{EntityTrait, QueryFilter, QueryOrder, QuerySelect};

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
        let range = crate::event_query::in_range(after, through, channels);
        // A count derives nothing: rows in its range that still owe their header rows make it a
        // lower bound (`more`), never a wait on a derivation (crate::derive).
        let owed = crate::derive::reads_derived(headers, false, exclude.is_some())
            && crate::derive::owes::<event::Entity>(self.connection(), range.clone()).await?;
        let mut query = event::Entity::find()
            .filter(range)
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
        let mut count = HistoryCount::of_newest(times);
        count.more |= owed;
        Ok(count)
    }
}
