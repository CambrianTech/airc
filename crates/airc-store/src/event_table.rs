//! The two event tables (`events`, `bus_events`) as one abstraction: the
//! store's per-table logic (header predicates and rows, the addressee branch
//! queries, the derivation) is written once over [`EventTable`], and each impl
//! below holds only what differs between the tables: its columns, its header
//! entity, and how its content column yields addressing.

use airc_core::Addressing;
use sea_orm::EntityTrait;

use crate::entities::{bus_event, bus_event_header, event, event_header};
use crate::error::StoreError;

/// One event table and its derived header entity.
pub(crate) trait EventTable: EntityTrait {
    /// The table's header rows (`crate::header_query`).
    type Headers: EntityTrait;
    const EVENT_ID: Self::Column;
    const CLASS: Self::Column;
    const CLASS_FAMILY: Self::Column;
    const CLIENT_ID: Self::Column;
    const PEER_ID: Self::Column;
    const KIND: Self::Column;
    fn kinds(kinds: &std::collections::BTreeSet<airc_core::TranscriptKind>) -> Vec<&'static str>;
    const TARGET: Self::Column;
    const HEADERS: Self::Column;
    const ADDRESSED_PEER: Self::Column;
    const DERIVED_VERSION: Self::Column;
    const HEADER_EVENT_ID: <Self::Headers as EntityTrait>::Column;
    const HEADER_KEY: <Self::Headers as EntityTrait>::Column;
    const HEADER_VALUE: <Self::Headers as EntityTrait>::Column;
    /// Decode only the stored typed target.
    fn addressing(target: serde_json::Value) -> Result<Addressing, StoreError>;
}

fn decode(what: &str, error: impl std::fmt::Display) -> StoreError {
    StoreError::Migration(format!("decoding {what}: {error}"))
}

impl EventTable for event::Entity {
    fn kinds(kinds: &std::collections::BTreeSet<airc_core::TranscriptKind>) -> Vec<&'static str> {
        kinds.iter().map(|kind| kind.as_wire_str()).collect()
    }
    type Headers = event_header::Entity;
    const CLASS: event::Column = event::Column::Class;
    const CLASS_FAMILY: event::Column = event::Column::ClassFamily;
    const EVENT_ID: event::Column = event::Column::EventId;
    const CLIENT_ID: event::Column = event::Column::ClientId;
    const PEER_ID: event::Column = event::Column::PeerId;
    const KIND: event::Column = event::Column::Kind;
    const TARGET: event::Column = event::Column::Target;
    const HEADERS: event::Column = event::Column::Headers;
    const ADDRESSED_PEER: event::Column = event::Column::AddressedPeer;
    const DERIVED_VERSION: event::Column = event::Column::DerivedVersion;
    const HEADER_EVENT_ID: event_header::Column = event_header::Column::EventId;
    const HEADER_KEY: event_header::Column = event_header::Column::Key;
    const HEADER_VALUE: event_header::Column = event_header::Column::Value;

    fn addressing(target: serde_json::Value) -> Result<Addressing, StoreError> {
        let target: airc_core::MentionTarget =
            serde_json::from_value(target).map_err(|e| decode("events.target", e))?;
        Ok(Addressing::of_target(&target))
    }
}

impl EventTable for bus_event::Entity {
    fn kinds(kinds: &std::collections::BTreeSet<airc_core::TranscriptKind>) -> Vec<&'static str> {
        airc_bus::Kind::ALL
            .iter()
            .copied()
            .filter(|kind| kinds.contains(&kind.transcript_kind()))
            .map(crate::bus_sink::kind_to_text)
            .collect()
    }
    type Headers = bus_event_header::Entity;
    const CLASS: bus_event::Column = bus_event::Column::Class;
    const CLASS_FAMILY: bus_event::Column = bus_event::Column::ClassFamily;
    const EVENT_ID: bus_event::Column = bus_event::Column::EventId;
    const CLIENT_ID: bus_event::Column = bus_event::Column::ClientId;
    const PEER_ID: bus_event::Column = bus_event::Column::PeerId;
    const KIND: bus_event::Column = bus_event::Column::Kind;
    const TARGET: bus_event::Column = bus_event::Column::Target;
    const HEADERS: bus_event::Column = bus_event::Column::Headers;
    const ADDRESSED_PEER: bus_event::Column = bus_event::Column::AddressedPeer;
    const DERIVED_VERSION: bus_event::Column = bus_event::Column::DerivedVersion;
    const HEADER_EVENT_ID: bus_event_header::Column = bus_event_header::Column::EventId;
    const HEADER_KEY: bus_event_header::Column = bus_event_header::Column::Key;
    const HEADER_VALUE: bus_event_header::Column = bus_event_header::Column::Value;

    fn addressing(target: serde_json::Value) -> Result<Addressing, StoreError> {
        let target: airc_bus::Target =
            serde_json::from_value(target).map_err(|e| decode("bus_events.target", e))?;
        Ok(airc_bus::addressing_of(&target))
    }
}
