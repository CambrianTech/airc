//! The shared header predicate algebra ([`HeaderFilter`]) evaluated by the
//! store as typed subqueries on an [`EventTable`]'s derived header entity
//! ([`crate::entities::event_header`], [`crate::entities::bus_event_header`]),
//! and the one writer of those header rows:
//! `Has` is a header row with the key, `Exact` one with the key and the value
//! (binary comparison), `Prefix` one with the key and a value in the code-point
//! range the prefix opens; `Not`/`All`/`AnyOf` compose. No JSON is parsed in SQL.

use airc_core::{HeaderFilter, Headers};
use sea_orm::sea_query::{Expr, OnConflict, Query, SelectStatement, SimpleExpr};
use sea_orm::{ActiveModelTrait, ConnectionTrait, DbErr, EntityTrait};
use uuid::Uuid;

use crate::event_table::EventTable;

/// Header rows written per INSERT: three bound values each, well under
/// SQLite's 32,766 host parameters per statement.
const HEADER_ROWS_PER_INSERT: usize = 5_000;

/// Write the header rows of `events` (id and headers) into `T`'s header
/// entity, in INSERTs of at most [`HEADER_ROWS_PER_INSERT`] rows; a row that is
/// already there (a replayed event) is skipped.
pub(crate) async fn write_header_rows<'a, T: EventTable>(
    db: &impl ConnectionTrait,
    events: impl IntoIterator<Item = (Uuid, &'a Headers)>,
) -> Result<(), DbErr>
where
    <T::Headers as EntityTrait>::ActiveModel: Send,
{
    let mut rows = Vec::new();
    for (id, headers) in events {
        for (key, value) in headers {
            let mut row = <<T::Headers as EntityTrait>::ActiveModel as ActiveModelTrait>::default();
            row.try_set(T::HEADER_EVENT_ID, id.into())?;
            row.try_set(T::HEADER_KEY, key.clone().into())?;
            row.try_set(T::HEADER_VALUE, value.clone().into())?;
            rows.push(row);
        }
    }
    for chunk in rows.chunks(HEADER_ROWS_PER_INSERT) {
        match T::Headers::insert_many(chunk.to_vec())
            .on_conflict(
                OnConflict::columns([T::HEADER_EVENT_ID, T::HEADER_KEY])
                    .do_nothing()
                    .to_owned(),
            )
            .exec(db)
            .await
        {
            Ok(_) | Err(DbErr::RecordNotInserted) => {}
            Err(err) => return Err(err),
        }
    }
    Ok(())
}

/// Rows of `T` that are NOT this runtime's own sends: [`airc_core::RuntimeSelf`]
/// translated to the header predicate and the table's `client_id` column.
pub(crate) fn not_runtime_self<T: EventTable>(rule: &airc_core::RuntimeSelf) -> SimpleExpr {
    let stamped = HeaderFilter::Has {
        key: airc_core::headers::HEADER_AIRC_CLIENT.to_string(),
    };
    let unstamped_self = predicate::<T>(&HeaderFilter::Not(Box::new(stamped)))
        .and(Expr::col((T::default(), T::CLIENT_ID)).eq(rule.client_id.as_uuid()));
    let is_self = match &rule.runtime_client {
        Some(tag) => predicate::<T>(&HeaderFilter::Exact {
            key: airc_core::headers::HEADER_AIRC_CLIENT.to_string(),
            value: tag.clone(),
        })
        .or(unstamped_self),
        None => unstamped_self,
    };
    is_self.not()
}

/// Rows of `T` whose headers match `filter`.
pub(crate) fn predicate<T: EventTable>(filter: &HeaderFilter) -> SimpleExpr {
    let value = || Expr::col((T::Headers::default(), T::HEADER_VALUE));
    match filter {
        HeaderFilter::Any => Expr::value(true),
        HeaderFilter::Has { key } => holding::<T>(rows::<T>(key, None)),
        HeaderFilter::Exact { key, value: wanted } => {
            holding::<T>(rows::<T>(key, Some(value().eq(wanted.as_str()))))
        }
        HeaderFilter::Prefix { key, value_prefix } => {
            // Every value that starts with the prefix, and only those, lies in
            // [prefix, successor(prefix)) in code-point (UTF-8 byte) order.
            let from = value().gte(value_prefix.as_str());
            let range = match successor(value_prefix) {
                Some(upper) => from.and(value().lt(upper)),
                None if value_prefix.is_empty() => Expr::value(true),
                None => from,
            };
            holding::<T>(rows::<T>(key, Some(range)))
        }
        HeaderFilter::Not(inner) => predicate::<T>(inner).not(),
        HeaderFilter::All(items) => items
            .iter()
            .fold(Expr::value(true), |acc, f| acc.and(predicate::<T>(f))),
        HeaderFilter::AnyOf(items) => items
            .iter()
            .fold(Expr::value(false), |acc, f| acc.or(predicate::<T>(f))),
    }
}

/// The ids of events holding header `key` (and matching `value`, if given).
fn rows<T: EventTable>(key: &str, value: Option<SimpleExpr>) -> SelectStatement {
    let mut select = Query::select();
    select
        .column((T::Headers::default(), T::HEADER_EVENT_ID))
        .from(T::Headers::default())
        .and_where(Expr::col((T::Headers::default(), T::HEADER_KEY)).eq(key));
    if let Some(value) = value {
        select.and_where(value);
    }
    select.to_owned()
}

fn holding<T: EventTable>(ids: SelectStatement) -> SimpleExpr {
    Expr::col((T::default(), T::EVENT_ID)).in_subquery(ids)
}

/// The least string greater than every string that starts with `prefix`:
/// drop trailing `char::MAX`, then step the last char to the next scalar
/// value. `None` when no such string exists (empty, or all `char::MAX`).
fn successor(prefix: &str) -> Option<String> {
    let mut chars: Vec<char> = prefix.chars().collect();
    while let Some(last) = chars.pop() {
        let next = match u32::from(last) + 1 {
            0xD800 => char::from_u32(0xE000),
            code => char::from_u32(code),
        };
        if let Some(next) = next {
            chars.push(next);
            return Some(chars.into_iter().collect());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::event;
    use crate::store::EventStore;
    use crate::SqliteEventStore;
    use airc_core::{Body, ClientId, EventId, MentionTarget, PeerId, RoomId, TranscriptEvent};
    use sea_orm::{QueryFilter, QuerySelect};

    // what this catches: the typed header subqueries agree with the Rust header
    // algebra for literal keys with SQL metacharacters, unicode values, exact
    // case, prefixes (including the code-point successor edge) and composition,
    // on an event with the header and one without any.
    #[tokio::test]
    async fn sql_matches_header_algebra_with_literal_keys_and_unicode() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let mut headers = Headers::new();
        headers.insert("a.b\"%_".into(), "🌍_%Case\u{10FFFF}".into());
        let room = RoomId::new();
        let make = |n: u128, headers: Headers| TranscriptEvent {
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            event_id: EventId::from_u128(n),
            room_id: room,
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: airc_core::TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: n as u64,
            target: MentionTarget::All,
            headers,
            body: Some(Body::text("x")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        store.append(make(1, headers.clone())).await.unwrap();
        store.append(make(2, Headers::new())).await.unwrap();
        let has = HeaderFilter::Has {
            key: "a.b\"%_".into(),
        };
        let missing = HeaderFilter::Has {
            key: "absent".into(),
        };
        let prefix = |value_prefix: &str| HeaderFilter::Prefix {
            key: "a.b\"%_".into(),
            value_prefix: value_prefix.into(),
        };
        let filters = vec![
            HeaderFilter::Any,
            has.clone(),
            missing.clone(),
            HeaderFilter::Not(Box::new(missing.clone())),
            HeaderFilter::Exact {
                key: "a.b\"%_".into(),
                value: "🌍_%Case\u{10FFFF}".into(),
            },
            HeaderFilter::Exact {
                key: "a.b\"%_".into(),
                value: "🌍_%case\u{10FFFF}".into(),
            },
            prefix("🌍_%"),
            prefix("🌍_%c"),
            prefix("🌍_%Case\u{10FFFF}"),
            prefix("🌍_%Cas"),
            prefix(""),
            HeaderFilter::Prefix {
                key: "absent".into(),
                value_prefix: "".into(),
            },
            HeaderFilter::All(vec![]),
            HeaderFilter::AnyOf(vec![]),
            HeaderFilter::All(vec![has.clone(), HeaderFilter::Not(Box::new(missing))]),
            HeaderFilter::AnyOf(vec![has.clone(), HeaderFilter::Not(Box::new(has))]),
        ];
        for filter in filters {
            let ids: Vec<uuid::Uuid> = event::Entity::find()
                .select_only()
                .column(event::Column::EventId)
                .filter(predicate::<event::Entity>(&filter))
                .into_tuple()
                .all(store.connection())
                .await
                .unwrap();
            for (n, held) in [(1, &headers), (2, &Headers::new())] {
                assert_eq!(
                    ids.contains(&EventId::from_u128(n).as_uuid()),
                    filter.matches(held),
                    "{filter:?} on event {n}"
                );
            }
        }
    }

    // what this catches: the prefix range's upper bound steps over the
    // surrogate gap and past trailing `char::MAX`.
    #[test]
    fn successor_bounds_every_prefix() {
        assert_eq!(successor("ab"), Some("ac".into()));
        assert_eq!(successor("a\u{D7FF}"), Some("a\u{E000}".into()));
        assert_eq!(successor("a\u{10FFFF}"), Some("b".into()));
        assert_eq!(successor("\u{10FFFF}"), None);
        assert_eq!(successor(""), None);
    }
}
