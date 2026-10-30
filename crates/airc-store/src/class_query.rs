//! Typed routing predicates shared by transcript and bus history. Payloads are
//! never decoded to select a class. Unclassified historical rows remain audit
//! records and cannot be materialized as authenticated events.
use crate::{event_table::EventTable, StoreError};
use airc_core::{EventClass, EventSelection, HistoryMatch};
use sea_orm::{sea_query::SimpleExpr, ColumnTrait};

/// The common class/header predicate shared by live-filter replay and history.
pub(crate) fn selection<T: EventTable>(
    selection: EventSelection,
    headers: &airc_core::HeaderFilter,
) -> Result<SimpleExpr, StoreError> {
    let mut selected = T::CLASS.is_not_null().and(T::CLASS_FAMILY.is_not_null());
    if let Some(family) = selection.family() {
        selected = selected.and(T::CLASS_FAMILY.eq(family as i32));
    }
    if let EventSelection::Exact(class) = selection {
        selected = selected.and(T::CLASS.eq(serde_json::to_string(&class)?));
    }
    Ok(selected.and(crate::header_query::predicate::<T>(headers)))
}

pub(crate) fn matching<T: EventTable>(rule: HistoryMatch<'_>) -> Result<SimpleExpr, StoreError> {
    let mut selected = selection::<T>(rule.selection, rule.headers)?;
    if !rule.kinds.is_empty() {
        selected = selected.and(T::KIND.is_in(T::kinds(rule.kinds)));
    }
    if let Some((peer, client, filter)) = rule.self_echo {
        selected = match filter {
            airc_core::SelfFilter::IncludeAll => selected,
            airc_core::SelfFilter::ExcludeSameClient => {
                selected.and(T::CLIENT_ID.ne(client.as_uuid()))
            }
            airc_core::SelfFilter::ExcludeSamePeer => selected.and(T::PEER_ID.ne(peer.as_uuid())),
        };
    }
    if let Some(addressee) = rule.addressed_to {
        selected = selected.and(T::ADDRESSED_PEER.eq(addressee.peer.as_uuid()));
    }
    Ok(selected)
}

pub(crate) fn decode(
    id: uuid::Uuid,
    class: Option<String>,
    family: Option<i32>,
) -> Result<EventClass, StoreError> {
    let class: EventClass = serde_json::from_str(&class.ok_or(StoreError::UnclassifiedEvent(id))?)?;
    if family != Some(class.family() as i32) {
        return Err(StoreError::InvalidStoredEnumString {
            column: "class_family",
            value: format!("{family:?} disagrees with {class:?}"),
        });
    }
    Ok(class)
}
