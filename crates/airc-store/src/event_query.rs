//! The `events` selects a bounded history scan runs: one builder, so the store
//! and the index-evidence tests read the same statements.
//! An addressee scan is one select per branch
//! ([`crate::event_table::addressee_branches`]); the caller merges them
//! ([`crate::event_table::merge_branches`]).

use airc_core::{HistoryMatch, RoomId, TranscriptCursor};
use sea_orm::{
    sea_query::{Expr, SimpleExpr},
    ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Select,
};

use crate::entities::event;

/// Events strictly after `c` in transcript order (lamport, then event id as the deterministic
/// tiebreak).
pub(crate) fn after(c: &TranscriptCursor) -> SimpleExpr {
    let lamport = c.lamport as i64;
    Expr::col(event::Column::Lamport)
        .gt(lamport)
        .or(Expr::col(event::Column::Lamport)
            .eq(lamport)
            .and(Expr::col(event::Column::EventId).gt(c.event_id.as_uuid())))
}

/// Events strictly before `c` in transcript order.
pub(crate) fn before(c: &TranscriptCursor) -> SimpleExpr {
    let lamport = c.lamport as i64;
    Expr::col(event::Column::Lamport)
        .lt(lamport)
        .or(Expr::col(event::Column::Lamport)
            .eq(lamport)
            .and(Expr::col(event::Column::EventId).lt(c.event_id.as_uuid())))
}

/// Events of `channels` strictly after `cursor` and at or before `through`: the one range every
/// history read, count and derivation scopes to.
pub(crate) fn in_range(
    cursor: &TranscriptCursor,
    through: &TranscriptCursor,
    channels: &[RoomId],
) -> Condition {
    Condition::all()
        .add(event::Column::RoomId.is_in(channels.iter().map(|room| room.as_uuid())))
        .add(after(cursor))
        .add(after(through).not())
}

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
    let order = if tail {
        sea_orm::Order::Desc
    } else {
        sea_orm::Order::Asc
    };
    let query = event::Entity::find()
        .filter(in_range(cursor, through, channels))
        .filter(crate::header_query::predicate::<event::Entity>(
            rule.headers,
        ))
        .order_by(event::Column::Lamport, order.clone())
        .order_by(event::Column::EventId, order)
        .limit(limit as u64);
    crate::event_table::addressee_branches(query, rule.addressed_to)
}
