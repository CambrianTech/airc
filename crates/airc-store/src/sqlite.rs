//! SeaORM-backed SQLite implementation of [`EventStore`].
//!
//! `open(db_url)` connects and applies pending migrations. Subsequent
//! `append` / `page_recent` / `resume_from` / `latest_cursor` calls
//! hit the same `sea_orm::DatabaseConnection`, which the SeaORM
//! connection pool serialises internally.
//!
//! Performance notes (SQLite specifics, deliberate):
//!   - Connection pool size is left at sea_orm's default (one writer
//!     for SQLite). Concurrent appenders queue at the driver layer
//!     rather than the application layer.
//!   - JSON values are stored as TEXT; SQLite is opinion-free about
//!     it. Postgres-backed deployments would promote to JSONB by
//!     changing only the `[features]` of sea-orm.

use std::path::Path;

use async_trait::async_trait;
use sea_orm::{
    sea_query::{Expr, OnConflict},
    ActiveValue, ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect,
};
use sea_orm_migration::MigratorTrait;
use serde_json::Value as JsonValue;

use airc_core::{
    transcript::{MentionTarget, TranscriptKind},
    Body, ClientId, EventId, Headers, PeerId, RoomId, TranscriptCursor, TranscriptEvent,
};

use crate::entities::event;
use crate::error::StoreError;
use crate::migration::Migrator;
use crate::store::EventStore;

pub struct SqliteEventStore {
    db: DatabaseConnection,
}

impl SqliteEventStore {
    /// Open an existing SQLite database (or create one) at `db_url`
    /// and run any pending migrations.
    ///
    /// For an in-memory store use `"sqlite::memory:"`; for a file
    /// store use `"sqlite://<path>?mode=rwc"` so SQLite creates the
    /// file if missing.
    pub async fn open(db_url: &str) -> Result<Self, StoreError> {
        let mut opts = ConnectOptions::new(db_url.to_owned());
        // Keep timeouts predictable for tests — long enough to absorb
        // a slow CI box, short enough to fail fast on a bad URL.
        opts.connect_timeout(std::time::Duration::from_secs(5))
            .acquire_timeout(std::time::Duration::from_secs(5))
            .max_connections(1);
        let db = Database::connect(opts).await?;
        Migrator::up(&db, None)
            .await
            .map_err(|err| StoreError::Migration(err.to_string()))?;
        Ok(Self { db })
    }

    /// Open a file-backed SQLite store from a filesystem path.
    ///
    /// This keeps platform path rules out of consumers. Windows
    /// paths must be converted to URI-style forward slashes before
    /// handing them to SQLx/SeaORM; callers should not build SQLite
    /// URLs with `Path::display()`.
    pub async fn open_path(path: &Path) -> Result<Self, StoreError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        Self::open(&sqlite_file_url(path)).await
    }

    /// Open an ephemeral in-memory store. Convenience for tests.
    pub async fn in_memory() -> Result<Self, StoreError> {
        Self::open("sqlite::memory:").await
    }
}

fn sqlite_file_url(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    if has_windows_drive_prefix(&raw) {
        format!("sqlite:///{raw}?mode=rwc")
    } else {
        format!("sqlite://{raw}?mode=rwc")
    }
}

fn has_windows_drive_prefix(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 2 && bytes[1] == b':' && bytes[0].is_ascii_alphabetic()
}

#[async_trait]
impl EventStore for SqliteEventStore {
    async fn append(&self, ev: TranscriptEvent) -> Result<(), StoreError> {
        let active = to_active_model(&ev)?;
        // Insert with explicit "do nothing on conflict" so a replay
        // surfaces as DuplicateEventId rather than a generic DbErr.
        let res = event::Entity::insert(active)
            .on_conflict(
                OnConflict::column(event::Column::EventId)
                    .do_nothing()
                    .to_owned(),
            )
            .exec(&self.db)
            .await;
        match res {
            Ok(_) => {
                // Distinguish a true insert from a no-op DO-NOTHING:
                // re-query by event_id and compare.
                let existing = event::Entity::find_by_id(ev.event_id.as_uuid())
                    .one(&self.db)
                    .await?;
                match existing {
                    Some(row) if row.lamport == ev.lamport as i64 => Ok(()),
                    Some(_) => Err(StoreError::DuplicateEventId(ev.event_id.as_uuid())),
                    None => Err(StoreError::Database(sea_orm::DbErr::Custom(
                        "post-insert lookup returned no row".to_string(),
                    ))),
                }
            }
            Err(sea_orm::DbErr::RecordNotInserted) => {
                Err(StoreError::DuplicateEventId(ev.event_id.as_uuid()))
            }
            Err(err) => Err(StoreError::Database(err)),
        }
    }

    async fn page_recent(
        &self,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
        // "Newest N" = order DESC by (lamport, event_id), take N,
        // then reverse so the caller iterates oldest → newest.
        let mut query = event::Entity::find()
            .order_by_desc(event::Column::Lamport)
            .order_by_desc(event::Column::EventId)
            .limit(limit as u64);
        if let Some(room) = channel {
            query = query.filter(event::Column::RoomId.eq(room.as_uuid()));
        }
        let mut rows = query.all(&self.db).await?;
        rows.reverse();
        rows.into_iter().map(from_model).collect()
    }

    async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        channel: Option<RoomId>,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, StoreError> {
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
        let mut query = event::Entity::find()
            .filter(strictly_after)
            .order_by_asc(event::Column::Lamport)
            .order_by_asc(event::Column::EventId)
            .limit(limit as u64);
        if let Some(room) = channel {
            query = query.filter(event::Column::RoomId.eq(room.as_uuid()));
        }
        let rows = query.all(&self.db).await?;
        rows.into_iter().map(from_model).collect()
    }

    async fn latest_cursor(
        &self,
        channel: Option<RoomId>,
    ) -> Result<Option<TranscriptCursor>, StoreError> {
        let mut query = event::Entity::find()
            .order_by_desc(event::Column::Lamport)
            .order_by_desc(event::Column::EventId)
            .limit(1);
        if let Some(room) = channel {
            query = query.filter(event::Column::RoomId.eq(room.as_uuid()));
        }
        let row = query.one(&self.db).await?;
        Ok(row.map(|m| TranscriptCursor {
            lamport: m.lamport as u64,
            event_id: EventId::from_uuid(m.event_id),
        }))
    }
}

fn to_active_model(ev: &TranscriptEvent) -> Result<event::ActiveModel, StoreError> {
    let kind = match ev.kind {
        TranscriptKind::Message => "message",
        TranscriptKind::Attachment => "attachment",
        TranscriptKind::Receipt => "receipt",
        TranscriptKind::Presence => "presence",
        TranscriptKind::SessionControl => "session_control",
        TranscriptKind::System => "system",
    };
    Ok(event::ActiveModel {
        event_id: ActiveValue::Set(ev.event_id.as_uuid()),
        room_id: ActiveValue::Set(ev.room_id.as_uuid()),
        peer_id: ActiveValue::Set(ev.peer_id.as_uuid()),
        client_id: ActiveValue::Set(ev.client_id.as_uuid()),
        kind: ActiveValue::Set(kind.to_owned()),
        occurred_at_ms: ActiveValue::Set(ev.occurred_at_ms as i64),
        lamport: ActiveValue::Set(ev.lamport as i64),
        target: ActiveValue::Set(serde_json::to_value(&ev.target)?),
        headers: ActiveValue::Set(serde_json::to_value(&ev.headers)?),
        body: ActiveValue::Set(match &ev.body {
            Some(b) => Some(serde_json::to_value(b)?),
            None => None,
        }),
        attachment: ActiveValue::Set(match &ev.attachment {
            Some(a) => Some(serde_json::to_value(a)?),
            None => None,
        }),
        receipt: ActiveValue::Set(match &ev.receipt {
            Some(r) => Some(serde_json::to_value(r)?),
            None => None,
        }),
        metadata: ActiveValue::Set(ev.metadata.clone()),
    })
}

fn from_model(m: event::Model) -> Result<TranscriptEvent, StoreError> {
    let kind = match m.kind.as_str() {
        "message" => TranscriptKind::Message,
        "attachment" => TranscriptKind::Attachment,
        "receipt" => TranscriptKind::Receipt,
        "presence" => TranscriptKind::Presence,
        "session_control" => TranscriptKind::SessionControl,
        "system" => TranscriptKind::System,
        other => return Err(StoreError::UnknownTranscriptKind(other.to_string())),
    };
    let target: MentionTarget = serde_json::from_value(m.target)?;
    let headers: Headers = serde_json::from_value(m.headers)?;
    let body: Option<Body> = match m.body {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    };
    let attachment = match m.attachment {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    };
    let receipt = match m.receipt {
        Some(v) => Some(serde_json::from_value(v)?),
        None => None,
    };
    Ok(TranscriptEvent {
        event_id: EventId::from_uuid(m.event_id),
        room_id: RoomId::from_uuid(m.room_id),
        peer_id: PeerId::from_uuid(m.peer_id),
        client_id: ClientId::from_uuid(m.client_id),
        kind,
        occurred_at_ms: m.occurred_at_ms as u64,
        lamport: m.lamport as u64,
        target,
        headers,
        body,
        attachment,
        receipt,
        metadata: m.metadata,
    })
}

/// Silence the unused-import warning if `JsonValue` is needed later
/// without forcing an import-shuffle.
#[allow(dead_code)]
fn _json_value_keepalive(_: JsonValue) {}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::transcript::MentionTarget;
    use serde_json::json;
    use uuid::Uuid;

    fn make_event(lamport: u64, room: RoomId, body: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: room,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    fn model_with_kind(kind: &str) -> event::Model {
        event::Model {
            event_id: Uuid::new_v4(),
            room_id: Uuid::new_v4(),
            peer_id: Uuid::new_v4(),
            client_id: Uuid::new_v4(),
            kind: kind.to_string(),
            occurred_at_ms: 1_700_000_000_000,
            lamport: 1,
            target: json!(MentionTarget::All),
            headers: json!(Headers::new()),
            body: None,
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn unknown_transcript_kind_fails_closed() {
        let result = from_model(model_with_kind("future_kind"));

        assert!(
            matches!(result, Err(StoreError::UnknownTranscriptKind(ref kind)) if kind == "future_kind"),
            "expected UnknownTranscriptKind, got {result:?}"
        );
    }

    #[tokio::test]
    async fn append_then_page_recent_round_trips() {
        // The minimum-viable proof: write one, read one back, body
        // intact, identity preserved.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let ev = make_event(1, room, "hello");
        store.append(ev.clone()).await.unwrap();

        let page = store.page_recent(Some(room), 10).await.unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0], ev);
    }

    #[tokio::test]
    async fn page_recent_returns_oldest_to_newest_with_limit() {
        // Substrate contract: page is ordered oldest → newest within
        // the returned slice, AND a limit smaller than the total set
        // keeps the newest end of the log.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        for i in 1..=5u64 {
            store
                .append(make_event(i, room, &format!("msg{i}")))
                .await
                .unwrap();
        }
        let page = store.page_recent(Some(room), 3).await.unwrap();
        assert_eq!(page.len(), 3);
        let lamports: Vec<u64> = page.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![3, 4, 5], "newest 3, oldest-first");
    }

    #[tokio::test]
    async fn resume_from_returns_strictly_after_cursor() {
        // The cursor semantics that grievance §7 requires: events AT
        // OR BEFORE the cursor are excluded; everything strictly
        // after is returned in transcript order.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let mut events = Vec::new();
        for i in 1..=5u64 {
            let ev = make_event(i, room, &format!("msg{i}"));
            events.push(ev.clone());
            store.append(ev).await.unwrap();
        }
        let cursor = events[2].cursor(); // lamport=3
        let after = store.resume_from(&cursor, Some(room), 10).await.unwrap();
        let lamports: Vec<u64> = after.iter().map(|e| e.lamport).collect();
        assert_eq!(lamports, vec![4, 5]);
    }

    #[tokio::test]
    async fn channel_filter_isolates_rooms() {
        // page_recent / resume_from with channel = Some(room) must
        // not leak events from other rooms — even when the global
        // log interleaves them. Grievance §7: "no cross-room leakage
        // when wires are shared".
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room_a = RoomId::from_u128(0xaaaa);
        let room_b = RoomId::from_u128(0xbbbb);
        store.append(make_event(1, room_a, "a-1")).await.unwrap();
        store.append(make_event(2, room_b, "b-1")).await.unwrap();
        store.append(make_event(3, room_a, "a-2")).await.unwrap();
        store.append(make_event(4, room_b, "b-2")).await.unwrap();

        let room_a_page = store.page_recent(Some(room_a), 10).await.unwrap();
        let room_a_bodies: Vec<&str> = room_a_page
            .iter()
            .filter_map(|e| e.body.as_ref().and_then(Body::as_text))
            .collect();
        assert_eq!(room_a_bodies, vec!["a-1", "a-2"]);

        let room_b_page = store.page_recent(Some(room_b), 10).await.unwrap();
        let room_b_bodies: Vec<&str> = room_b_page
            .iter()
            .filter_map(|e| e.body.as_ref().and_then(Body::as_text))
            .collect();
        assert_eq!(room_b_bodies, vec!["b-1", "b-2"]);
    }

    #[tokio::test]
    async fn latest_cursor_reflects_newest_event_per_channel() {
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room_a = RoomId::from_u128(0xaaaa);
        let room_b = RoomId::from_u128(0xbbbb);
        assert!(store.latest_cursor(Some(room_a)).await.unwrap().is_none());

        let a1 = make_event(1, room_a, "a-1");
        let b1 = make_event(2, room_b, "b-1");
        let a2 = make_event(3, room_a, "a-2");
        store.append(a1.clone()).await.unwrap();
        store.append(b1.clone()).await.unwrap();
        store.append(a2.clone()).await.unwrap();

        let latest_a = store.latest_cursor(Some(room_a)).await.unwrap().unwrap();
        assert_eq!(latest_a, a2.cursor());
        let latest_b = store.latest_cursor(Some(room_b)).await.unwrap().unwrap();
        assert_eq!(latest_b, b1.cursor());
        let latest_global = store.latest_cursor(None).await.unwrap().unwrap();
        assert_eq!(latest_global, a2.cursor());
    }

    #[tokio::test]
    async fn duplicate_event_id_surfaces_as_typed_error() {
        // Replay-safety: appending the same event_id twice must
        // surface DuplicateEventId, not a generic DB error.
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let ev = make_event(1, room, "once");
        store.append(ev.clone()).await.unwrap();

        let second = store.append(ev.clone()).await;
        assert!(
            matches!(second, Err(StoreError::DuplicateEventId(id)) if id == ev.event_id.as_uuid()),
            "expected DuplicateEventId({}), got {second:?}",
            ev.event_id.as_uuid()
        );
    }

    #[tokio::test]
    async fn cursor_tiebreaks_on_event_id_at_same_lamport() {
        // Two events with the same lamport must page in event_id
        // order deterministically. Grievance §7 explicitly named
        // this: "cursor = (lamport, event_id) … stronger event-store
        // cursor".
        let store = SqliteEventStore::in_memory().await.unwrap();
        let room = RoomId::from_u128(0xc0ffee);
        let mut ev_a = make_event(7, room, "same-lamport-a");
        let mut ev_b = make_event(7, room, "same-lamport-b");
        // Force a deterministic event_id order so the test isn't
        // flaky against UUIDv4 chance.
        ev_a.event_id = EventId::from_u128(0x1);
        ev_b.event_id = EventId::from_u128(0x2);
        store.append(ev_a.clone()).await.unwrap();
        store.append(ev_b.clone()).await.unwrap();

        let page = store.page_recent(Some(room), 10).await.unwrap();
        assert_eq!(page[0].event_id, ev_a.event_id);
        assert_eq!(page[1].event_id, ev_b.event_id);

        // resume_from(ev_a) should return ev_b — strictly after at
        // the same lamport.
        let after = store
            .resume_from(&ev_a.cursor(), Some(room), 10)
            .await
            .unwrap();
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].event_id, ev_b.event_id);
    }

    #[test]
    fn sqlite_file_url_uses_uri_slashes_for_windows_paths() {
        let path = Path::new(r"C:\Users\agent\.airc\events.sqlite");

        assert_eq!(
            sqlite_file_url(path),
            "sqlite:///C:/Users/agent/.airc/events.sqlite?mode=rwc"
        );
    }

    #[test]
    fn sqlite_file_url_preserves_unix_absolute_paths() {
        let path = Path::new("/tmp/airc/events.sqlite");

        assert_eq!(
            sqlite_file_url(path),
            "sqlite:///tmp/airc/events.sqlite?mode=rwc"
        );
    }
}
