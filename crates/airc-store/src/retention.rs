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
const HEARTBEAT_ROW: &str = r#"json_type(headers, '$."airc.heartbeat.kind"') IS NOT NULL"#;
/// `events` rows of any NON-DURABLE class — decided by the header, never the body
/// (the header rule: classify at ingress from headers, never decode payloads). After
/// #1457 no non-durable frame is transcript history, so every such row is dead:
/// cursor-paging requests and replies (stamped `request_response` by the backfill
/// exchange, up to 8 MB each) and any other class that predates the gate. A missing
/// header means durable, exactly as `delivery_class_from_header` reads it.
const NON_DURABLE_ROW: &str =
    r#"coalesce(json_extract(headers, '$."airc.delivery_class"'), 'durable') <> 'durable'"#;

/// Rows of rowid space one DELETE statement covers. The pools are
/// `max_connections(1)` and SQLite has one writer: a single statement over a
/// multi-GB backlog held both for its whole run — at boot, while peers backfill —
/// which is the >2 s acquire stall that darkened a node for 7 h on 2026-09-26. A
/// rowid WINDOW bounds each statement's work by the b-tree range it walks, not by
/// how many rows match, so the first pass over a 1.4M-row table is ~140 short
/// transactions with the connection and the write lock released between them.
const DRAIN_WINDOW_ROWS: i64 = 10_000;

/// Freelist pages one `incremental_vacuum` step returns (8 MB at 4 KiB pages):
/// the same bound on the reclaim that the window is on the delete.
const RECLAIM_STEP_PAGES: u64 = 2_048;

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
    /// `events` rows of a non-durable class (backfill frames above all).
    pub non_durable_rows: u64,
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
    /// `PRAGMA incremental_vacuum(N)` in bounded steps: returns freelist pages in
    /// short write transactions a live daemon can afford on every tick. A no-op on
    /// a file that has never had a full vacuum since the mode was set.
    Incremental,
}

impl SqliteEventStore {
    /// Delete the two dead classes and report the footprint after.
    pub async fn drain_dead_rows(&self) -> Result<DrainReport, StoreError> {
        self.drain_dead_rows_windowed(DRAIN_WINDOW_ROWS).await
    }

    pub(crate) async fn drain_dead_rows_windowed(
        &self,
        window: i64,
    ) -> Result<DrainReport, StoreError> {
        let heartbeat_rows = self
            .delete_in_windows("bus_events", HEARTBEAT_ROW, window)
            .await?;
        let non_durable_rows = self
            .delete_in_windows("events", NON_DURABLE_ROW, window)
            .await?;
        Ok(DrainReport {
            heartbeat_rows,
            non_durable_rows,
            after: self.footprint().await?,
        })
    }

    /// `DELETE … WHERE <predicate>` one rowid window at a time, yielding between
    /// windows so queued appends and reads take the connection and the write lock
    /// ([`DRAIN_WINDOW_ROWS`]). Bounded by the table's max rowid at the start: rows
    /// written during the pass are the next pass's.
    async fn delete_in_windows(
        &self,
        table: &str,
        predicate: &str,
        window: i64,
    ) -> Result<u64, StoreError> {
        let db = self.connection();
        let max_rowid = self
            .scalar_i64(&format!("SELECT coalesce(max(rowid), 0) FROM {table}"))
            .await?;
        let sql = format!("DELETE FROM {table} WHERE rowid > ? AND rowid <= ? AND {predicate}");
        let mut low = 0i64;
        let mut removed = 0u64;
        while low < max_rowid {
            let high = low.saturating_add(window.max(1));
            removed += db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    &sql,
                    [low.into(), high.into()],
                ))
                .await?
                .rows_affected();
            low = high;
            tokio::task::yield_now().await;
        }
        Ok(removed)
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
                // Bounded steps, yielding between them. A step that frees nothing
                // ends the loop: an `auto_vacuum=0` file ignores the pragma, and
                // waiting on its freelist to shrink would never end.
                let mut free = self.pragma_u64("freelist_count").await?;
                while free > 0 {
                    db.execute_unprepared(&format!(
                        "PRAGMA incremental_vacuum({RECLAIM_STEP_PAGES})"
                    ))
                    .await?;
                    let now = self.pragma_u64("freelist_count").await?;
                    if now >= free {
                        break;
                    }
                    free = now;
                    tokio::task::yield_now().await;
                }
            }
        }
        self.footprint().await
    }

    async fn scalar_i64(&self, sql: &str) -> Result<i64, StoreError> {
        let row = self
            .connection()
            .query_one(Statement::from_string(DbBackend::Sqlite, sql.to_owned()))
            .await?
            .ok_or_else(|| StoreError::Migration(format!("`{sql}` returned no row")))?;
        Ok(row.try_get_by_index(0)?)
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
        transcript_event_with(room, lamport, body, &[])
    }

    fn transcript_event_with(
        room: RoomId,
        lamport: u64,
        body: serde_json::Value,
        headers: &[(&str, &str)],
    ) -> TranscriptEvent {
        let mut h = Headers::new();
        for (k, v) in headers {
            h.insert((*k).to_owned(), (*v).to_owned());
        }
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: room,
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            lamport,
            target: MentionTarget::All,
            headers: h,
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
    // body stay — and reports a footprint the file's length cannot lie about. The
    // paging frame is picked by its HEADER class (`request_response`, as the
    // backfill exchange stamps it), and a message stamped `durable` explicitly
    // stays beside a header-less one: absent means durable.
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
            .append(transcript_event_with(
                ch,
                1,
                serde_json::json!({"before": null, "channel": ch.to_string(), "cursor_paging": true, "limit": 200}),
                &[("airc.delivery_class", "request_response")],
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
        store
            .append(transcript_event_with(
                ch,
                3,
                serde_json::json!({"text": "an explicitly durable message"}),
                &[("airc.delivery_class", "durable")],
            ))
            .await
            .expect("test: explicit durable message");
        // A non-durable row that is NOT a paging frame: only the header class can
        // tell it is dead (a body test for `cursor_paging` would keep it forever).
        store
            .append(transcript_event_with(
                ch,
                4,
                serde_json::json!({"typing": true}),
                &[("airc.delivery_class", "ephemeral_latest")],
            ))
            .await
            .expect("test: pre-gate ephemeral row");

        let report = store.drain_dead_rows().await.expect("test: drain");
        assert_eq!(
            (report.heartbeat_rows, report.non_durable_rows),
            (1, 2),
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
        assert_eq!(events_left.len(), 2, "both durable messages stay");
        assert!(
            events_left
                .iter()
                .any(|e| matches!(&e.body, Some(Body::Json(v)) if v["text"] == "a real message")),
            "the header-less message stays intact"
        );

        // A second pass finds nothing: the drain is idempotent.
        let again = store.drain_dead_rows().await.expect("test: drain again");
        assert_eq!((again.heartbeat_rows, again.non_durable_rows), (0, 0));
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
        // A window far below the row count: the drain must cover every window, not
        // just the first one (the batched pass that keeps the connection free).
        let drained = store
            .drain_dead_rows_windowed(64)
            .await
            .expect("test: drain");
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
