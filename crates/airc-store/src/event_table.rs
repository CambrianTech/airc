//! The two event tables (`events`, `bus_events`) as one abstraction: the
//! store's per-table logic (header predicates and rows, the addressee branch
//! queries, the derivation) is written once over [`EventTable`], and each impl
//! below holds only what differs between the tables: its columns, its header
//! entity, and how its content column yields addressing.

use airc_core::{AddresseeFilter, Addressing};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Select, TryGetable};

use crate::entities::{bus_event, bus_event_header, event, event_header};
use crate::error::StoreError;

/// One event table and its derived header entity.
pub(crate) trait EventTable: EntityTrait {
    /// The table's header rows (`crate::header_query`).
    type Headers: EntityTrait;
    const EVENT_ID: Self::Column;
    const CLIENT_ID: Self::Column;
    const TARGET: Self::Column;
    const HEADERS: Self::Column;
    /// What addressing is read from besides the target: the body or payload.
    const CONTENT: Self::Column;
    const ADDRESSED_PEER: Self::Column;
    const LEADING_MENTION: Self::Column;
    const DERIVED_VERSION: Self::Column;
    const HEADER_EVENT_ID: <Self::Headers as EntityTrait>::Column;
    const HEADER_KEY: <Self::Headers as EntityTrait>::Column;
    const HEADER_VALUE: <Self::Headers as EntityTrait>::Column;
    /// The stored type of [`Self::CONTENT`].
    type Content: TryGetable + Send;
    /// The addressing of a stored row, from its target JSON and content.
    fn addressing(
        target: serde_json::Value,
        content: Self::Content,
    ) -> Result<Addressing, StoreError>;
}

fn decode(what: &str, error: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("decoding {what}: {error}"))
}

impl EventTable for event::Entity {
    type Headers = event_header::Entity;
    const EVENT_ID: event::Column = event::Column::EventId;
    const CLIENT_ID: event::Column = event::Column::ClientId;
    const TARGET: event::Column = event::Column::Target;
    const HEADERS: event::Column = event::Column::Headers;
    const CONTENT: event::Column = event::Column::Body;
    const ADDRESSED_PEER: event::Column = event::Column::AddressedPeer;
    const LEADING_MENTION: event::Column = event::Column::LeadingMention;
    const DERIVED_VERSION: event::Column = event::Column::DerivedVersion;
    const HEADER_EVENT_ID: event_header::Column = event_header::Column::EventId;
    const HEADER_KEY: event_header::Column = event_header::Column::Key;
    const HEADER_VALUE: event_header::Column = event_header::Column::Value;
    type Content = Option<serde_json::Value>;

    fn addressing(
        target: serde_json::Value,
        body: Option<serde_json::Value>,
    ) -> Result<Addressing, StoreError> {
        let target: airc_core::MentionTarget =
            serde_json::from_value(target).map_err(|e| decode("events.target", e))?;
        let body: Option<airc_core::Body> = body
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| decode("events.body", e))?;
        Ok(Addressing::of_target(&target, body.as_ref()))
    }
}

impl EventTable for bus_event::Entity {
    type Headers = bus_event_header::Entity;
    const EVENT_ID: bus_event::Column = bus_event::Column::EventId;
    const CLIENT_ID: bus_event::Column = bus_event::Column::ClientId;
    const TARGET: bus_event::Column = bus_event::Column::Target;
    const HEADERS: bus_event::Column = bus_event::Column::Headers;
    const CONTENT: bus_event::Column = bus_event::Column::Payload;
    const ADDRESSED_PEER: bus_event::Column = bus_event::Column::AddressedPeer;
    const LEADING_MENTION: bus_event::Column = bus_event::Column::LeadingMention;
    const DERIVED_VERSION: bus_event::Column = bus_event::Column::DerivedVersion;
    const HEADER_EVENT_ID: bus_event_header::Column = bus_event_header::Column::EventId;
    const HEADER_KEY: bus_event_header::Column = bus_event_header::Column::Key;
    const HEADER_VALUE: bus_event_header::Column = bus_event_header::Column::Value;
    type Content = Vec<u8>;

    fn addressing(target: serde_json::Value, payload: Vec<u8>) -> Result<Addressing, StoreError> {
        let target: airc_bus::Target =
            serde_json::from_value(target).map_err(|e| decode("bus_events.target", e))?;
        Ok(airc_bus::addressing_of(&target, &payload))
    }
}

/// An addressee scan of `query`: one select per branch (`addressed_peer = me`,
/// `leading_mention = true`), each served whole by its composite index
/// `(room_id, <branch column>, <cursor order>)`. SQLite cannot serve an OR
/// across two columns from one index and walks the room's order index instead
/// (measured on the M5: `SEARCH events USING INDEX
/// idx_events_room_lamport_event_id (room_id=?)`, and the same on
/// `bus_events`). Without a filter, the query itself. [`merge_branches`] joins
/// what the branches return.
pub(crate) fn addressee_branches<T: EventTable>(
    query: Select<T>,
    addressed_to: Option<AddresseeFilter>,
) -> Vec<Select<T>> {
    let Some(addressee) = addressed_to else {
        return vec![query];
    };
    vec![
        query
            .clone()
            .filter(T::ADDRESSED_PEER.eq(addressee.peer.as_uuid())),
        query.filter(T::LEADING_MENTION.eq(true)),
    ]
}

/// The branches' rows as one page: in cursor order by `key` (which ends with
/// the event id, so equal keys are the same row), deduplicated, the first
/// `limit` (the last `limit` for `tail`).
pub(crate) fn merge_branches<M, K: Ord>(
    branches: Vec<Vec<M>>,
    key: impl Fn(&M) -> K,
    limit: usize,
    tail: bool,
) -> Vec<M> {
    let mut rows: Vec<M> = branches.into_iter().flatten().collect();
    rows.sort_by_key(&key);
    rows.dedup_by(|a, b| key(a) == key(b));
    if tail {
        rows.drain(..rows.len().saturating_sub(limit));
    } else {
        rows.truncate(limit);
    }
    rows
}
