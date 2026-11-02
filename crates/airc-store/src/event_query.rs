//! The `events` selects a bounded history scan runs: one builder, so the store
//! and the index-evidence tests read the same statements.
//! An addressee scan is one select per branch
//! ([`crate::event_table::addressee_branches`]); the caller merges them
//! ([`crate::event_table::merge_branches`]).

use airc_core::{HistoryMatch, RoomId, TranscriptCursor};
use sea_orm::{
    sea_query::Expr, ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Select,
};

use crate::entities::event;

/// Events of `channels` in `(cursor, through]` matching `rule`, the first
/// `limit` in transcript order (the last `limit`, newest first, for `tail`):
/// one select, or one per addressee branch.
pub(crate) fn matching(
    cursor: &TranscriptCursor,
    through: &TranscriptCursor,
    channels: &[RoomId],
    rule: HistoryMatch<'_>,
    limit: usize,
    tail: bool,
) -> Vec<Select<event::Entity>> {
    // Strictly after the cursor: lamport > c.lamport
    //   OR (lamport == c.lamport AND event_id > c.event_id).
    // Two-key tiebreak so events with identical lamports remain
    // ordered deterministically.
    let cursor_lamport = cursor.lamport as i64;
    let cursor_event_id = cursor.event_id.as_uuid();
    let strictly_after = Expr::col(event::Column::Lamport)
        .gt(cursor_lamport)
        .or(Expr::col(event::Column::Lamport)
            .eq(cursor_lamport)
            .and(Expr::col(event::Column::EventId).gt(cursor_event_id)));
    let order = if tail {
        sea_orm::Order::Desc
    } else {
        sea_orm::Order::Asc
    };
    let query = event::Entity::find()
        .filter(strictly_after)
        .order_by(event::Column::Lamport, order.clone())
        .order_by(event::Column::EventId, order)
        .limit(limit as u64)
        .filter(event::Column::RoomId.is_in(channels.iter().map(|room| room.as_uuid())))
        .filter(crate::header_query::predicate::<event::Entity>(
            rule.headers,
        ))
        .filter(
            Expr::col(event::Column::Lamport)
                .lt(through.lamport as i64)
                .or(Expr::col(event::Column::Lamport)
                    .eq(through.lamport as i64)
                    .and(Expr::col(event::Column::EventId).lte(through.event_id.as_uuid()))),
        );
    crate::event_table::addressee_branches(query, rule.addressed_to)
}
