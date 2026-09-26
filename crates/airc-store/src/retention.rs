//! The drain (card 6781d7e9): rows the substrate writes that nothing reads.
//!
//! Every writer needs a drain. `events.sqlite` on BigMama reached 2.65 GB on
//! 2026-09-26 and the daemon's SQLx acquires stretched past 2 s; the core read the
//! slow daemon as a stale socket, refused to boot behind it, and the node was dark
//! from 05:27Z to 12:20Z (#4404 fixed the refusal; this fixes the weight). Measured
//! that day on this node, read-only:
//!
//! | class | rows | bytes | read by |
//! |---|---|---|---|
//! | `bus_events` with header `airc.heartbeat.kind` | 858,993 of 1,394,279 | — | nothing since #1458 (presence is the router snapshot) |
//! | `events` whose body carries `cursor_paging` | 51,863 of 678,889 | 1.234 GB | nothing since #1457 (paging frames route live) |
//!
//! Two classes, most of the file. This module deletes exactly those, reports the
//! file's footprint, and gives the bytes back — a `DELETE` alone frees nothing
//! from an `auto_vacuum=0` file (the pages go to the freelist and the file keeps
//! its size), so a pass that reports rows removed and a file unchanged would be a
//! success signal that cannot report failure. The daemon runs it on its tick
//! (`airc_daemon::store_retention`); `airc doctor` prints the footprint.
//!
//! Age-based retention of live classes is NOT here: a `bus_events` row of a durable
//! kind is what an offline peer backfills from, and an age cutoff would decide for
//! that peer how long it may be away. That is a policy decision with an owner,
//! not a drain.

use sea_orm::{ConnectionTrait, DbBackend, Statement};

use crate::error::StoreError;
use crate::sqlite::SqliteEventStore;

/// `bus_events` rows that were heartbeats: the header every heartbeat carries.
/// Quoted because the key has dots in it.
const DELETE_HEARTBEATS: &str =
    r#"DELETE FROM bus_events WHERE json_type(headers, '$."airc.heartbeat.kind"') IS NOT NULL"#;
/// `events` rows that were cursor-paging frames (backfill requests and replies):
/// their body is `{"kind":"json","value":{...,"cursor_paging":true,...}}`.
const DELETE_PAGING_FRAMES: &str =
    "DELETE FROM events WHERE json_type(body, '$.value.cursor_paging') IS NOT NULL";

/// The file as SQLite accounts for it: what it occupies and how much of that is
/// freelist (dead pages a `DELETE` left behind, reclaimable by a vacuum).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StoreFootprint {
    pub file_bytes: u64,
    pub free_bytes: u64,
}

impl StoreFootprint {
    /// Free space as a share of the file, in whole percent.
    pub fn free_percent(&self) -> u64 {
        self.free_bytes
            .saturating_mul(100)
            .checked_div(self.file_bytes)
            .unwrap_or(0)
    }
}

/// What one drain pass removed, and the footprint after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DrainReport {
    pub heartbeat_rows: u64,
    pub paging_rows: u64,
    pub after: StoreFootprint,
}

/// How the freelist is given back to the filesystem.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reclaim {
    /// A full `VACUUM`: rebuilds the file, holds the write lock for its whole
    /// duration and needs the file's size again in scratch space. The one way to
    /// shrink an `auto_vacuum=0` file, and it also switches the file to
    /// `auto_vacuum=INCREMENTAL`, after which [`Reclaim::Incremental`] works.
    Full,
    /// `PRAGMA incremental_vacuum`: returns freelist pages in short write
    /// transactions a live daemon can afford on every tick. A no-op on a file
    /// that has never had a full vacuum since the mode was set.
    Incremental,
}

impl SqliteEventStore {
    /// Delete the two dead classes and report the footprint after.
    pub async fn drain_dead_rows(&self) -> Result<DrainReport, StoreError> {
        let db = self.connection();
        let heartbeat_rows = db
            .execute(Statement::from_string(DbBackend::Sqlite, DELETE_HEARTBEATS))
            .await?
            .rows_affected();
        let paging_rows = db
            .execute(Statement::from_string(
                DbBackend::Sqlite,
                DELETE_PAGING_FRAMES,
            ))
            .await?
            .rows_affected();
        Ok(DrainReport {
            heartbeat_rows,
            paging_rows,
            after: self.footprint().await?,
        })
    }

    /// `page_size × page_count` and `page_size × freelist_count`, from SQLite
    /// itself — not the file's length, which the WAL and the OS decide.
    pub async fn footprint(&self) -> Result<StoreFootprint, StoreError> {
        let page_size = self.pragma_u64("page_size").await?;
        Ok(StoreFootprint {
            file_bytes: page_size.saturating_mul(self.pragma_u64("page_count").await?),
            free_bytes: page_size.saturating_mul(self.pragma_u64("freelist_count").await?),
        })
    }

    /// Give freelist pages back to the filesystem. See [`Reclaim`] for the cost of
    /// each mode; the caller decides when the daemon can afford which.
    pub async fn reclaim(&self, mode: Reclaim) -> Result<StoreFootprint, StoreError> {
        let db = self.connection();
        match mode {
            Reclaim::Full => {
                // Set the mode first: VACUUM is what makes an auto_vacuum change
                // take effect, so one full pass buys every later incremental one.
                db.execute_unprepared("PRAGMA auto_vacuum = INCREMENTAL")
                    .await?;
                db.execute_unprepared("VACUUM").await?;
            }
            Reclaim::Incremental => {
                db.execute_unprepared("PRAGMA incremental_vacuum").await?;
            }
        }
        self.footprint().await
    }

    async fn pragma_u64(&self, name: &str) -> Result<u64, StoreError> {
        let row = self
            .connection()
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                format!("PRAGMA {name}"),
            ))
            .await?
            .ok_or_else(|| StoreError::Migration(format!("PRAGMA {name} returned no row")))?;
        let value: i64 = row.try_get_by_index(0)?;
        u64::try_from(value)
            .map_err(|_| StoreError::Migration(format!("PRAGMA {name} was negative: {value}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus_sink::SqliteDurableSink;
    use crate::store::EventStore;
    use airc_bus::envelope::{DeliveryClass, Envelope, Kind};
    use airc_bus::{DurableSink, Seq};
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
        TranscriptKind,
    };

    fn bus_event(channel: RoomId, counter: u64, headers: &[(&str, &str)]) -> Envelope {
        let mut e = Envelope::new(
            channel,
            (PeerId::from_u128(0xa1), ClientId::from_u128(0xc1)),
            Kind::Event,
            DeliveryClass::Durable,
            bytes::Bytes::from_static(b"payload"),
        );
        e = e.with_event_id(EventId::from_u128(0x5000 + u128::from(counter)));
        for (k, v) in headers {
            e = e.with_header(*k, *v);
        }
        e.seq = Seq::new(1, counter);
        e.occurred_at_ms = 1_700_000_000_000 + counter;
        e
    }

    fn transcript_event(room: RoomId, lamport: u64, body: serde_json::Value) -> TranscriptEvent {
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
            body: Some(Body::Json(body)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    // regression for card 6781d7e9 (BigMama, 2026-09-26: 2.65 GB store, daemon
    // acquires > 2 s, node dark 7 h behind it).
    // what this catches: the drain removes exactly the two dead classes — a
    // heartbeat row and a paging frame go, a durable event and a message with a
    // body stay — and reports a footprint the file's length cannot lie about.
    #[tokio::test]
    async fn the_drain_removes_the_two_dead_classes_and_nothing_else() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        let path = dir.path().join("events.sqlite");
        // Both writers on the ONE file, as the daemon has them.
        let store = SqliteEventStore::open_path(&path)
            .await
            .expect("test: store");
        let sink = SqliteDurableSink::open_path(&path)
            .await
            .expect("test: sink");
        let ch = RoomId::from_u128(0xc0ffee);
        sink.append(&bus_event(
            ch,
            0,
            &[
                ("airc.heartbeat.kind", "alive"),
                ("airc.heartbeat.runtime", "agent"),
            ],
        ))
        .await
        .expect("test: heartbeat row");
        sink.append(&bus_event(ch, 1, &[("h", "durable")]))
            .await
            .expect("test: durable row");
        store
            .append(transcript_event(
                ch,
                1,
                serde_json::json!({"before": null, "channel": ch.to_string(), "cursor_paging": true, "limit": 200}),
            ))
            .await
            .expect("test: paging frame");
        store
            .append(transcript_event(
                ch,
                2,
                serde_json::json!({"text": "a real message"}),
            ))
            .await
            .expect("test: message");

        let report = store.drain_dead_rows().await.expect("test: drain");
        assert_eq!(
            (report.heartbeat_rows, report.paging_rows),
            (1, 1),
            "exactly the dead classes"
        );
        assert!(
            report.after.file_bytes > 0,
            "the footprint is SQLite's own accounting"
        );

        let bus_left = sink.page(ch, None, 100).await.expect("test: page");
        assert_eq!(bus_left.len(), 1, "the durable event stays");
        assert!(
            !bus_left[0].headers.contains_key("airc.heartbeat.kind"),
            "the heartbeat is gone"
        );
        let events_left = store
            .page_recent(Some(ch), 100)
            .await
            .expect("test: recent");
        assert_eq!(events_left.len(), 1, "the message stays");
        assert!(
            matches!(&events_left[0].body, Some(Body::Json(v)) if v["text"] == "a real message"),
            "the message stays intact"
        );

        // A second pass finds nothing: the drain is idempotent.
        let again = store.drain_dead_rows().await.expect("test: drain again");
        assert_eq!((again.heartbeat_rows, again.paging_rows), (0, 0));
    }

    // what this catches: the part a DELETE cannot do. The freelist a drain leaves
    // behind is only returned by a vacuum; the full pass shrinks the file and
    // enables incremental reclaim for every pass after it.
    #[tokio::test]
    async fn a_full_reclaim_returns_the_freelist_and_enables_incremental_passes() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        let path = dir.path().join("events.sqlite");
        let store = SqliteEventStore::open_path(&path)
            .await
            .expect("test: store");
        let sink = SqliteDurableSink::open_path(&path)
            .await
            .expect("test: sink");
        let ch = RoomId::from_u128(0xbeef);
        // Enough heartbeat rows to occupy pages the drain will free.
        for counter in 0..2_000u64 {
            sink.append(&bus_event(ch, counter, &[("airc.heartbeat.kind", "alive")]))
                .await
                .expect("test: heartbeat row");
        }
        let before = store.footprint().await.expect("test: footprint");
        let drained = store.drain_dead_rows().await.expect("test: drain");
        assert_eq!(drained.heartbeat_rows, 2_000);
        assert!(
            drained.after.free_bytes > 0,
            "a DELETE leaves the pages on the freelist: {:?}",
            drained.after
        );
        assert_eq!(
            drained.after.file_bytes, before.file_bytes,
            "…and the file does not shrink by itself"
        );
        let after = store.reclaim(Reclaim::Full).await.expect("test: vacuum");
        assert_eq!(after.free_bytes, 0, "a full vacuum returns every free page");
        assert!(
            after.file_bytes < before.file_bytes,
            "the file shrank: {before:?} -> {after:?}"
        );
        // Incremental reclaim is now live: rows deleted from here on come back
        // without a full vacuum.
        for counter in 2_000..2_500u64 {
            sink.append(&bus_event(ch, counter, &[("airc.heartbeat.kind", "alive")]))
                .await
                .expect("test: heartbeat row");
        }
        store.drain_dead_rows().await.expect("test: drain");
        let incremental = store
            .reclaim(Reclaim::Incremental)
            .await
            .expect("test: incremental");
        assert_eq!(
            incremental.free_bytes, 0,
            "incremental vacuum returned the freelist"
        );
    }
}
