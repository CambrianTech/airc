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
//! Two classes, most of the file then. On 2026-11-03 the IntelMac's stores held two
//! more, by kind: 2,205,371 of a resident store's rows were identity cards (a 60 s
//! republish into every room) and 1,626,658 of a project store's were cursor
//! advances (a lifecycle event per cursor save, read by nothing). Those are not
//! classes here: every kind declares its own retention at its definition
//! ([`airc_core::TranscriptKind::retention`]), and [`declared`] turns the
//! declarations into the drain's predicate, so there is one drain and no per-kind
//! code.
//!
//! The drain reports the file's footprint before and after and gives the bytes
//! back: a `DELETE` alone frees nothing from an `auto_vacuum=0` file (the pages go
//! to the freelist and the file keeps its size), so a pass that reports rows
//! removed and a file unchanged would be a success signal that cannot report
//! failure. Every store runs it in the background, the daemon's and every store a
//! process opens itself ([`run`]); `airc doctor` prints the footprint.
//!
//! History (every kind declared [`airc_core::Retention::History`]) is kept here
//! until the store's byte budget lands (card e95d41a4, slice 2): a `bus_events` row
//! of a durable kind is what an offline peer backfills from, so trimming history
//! decides how long a peer may be away, and the budget is that decision.

use airc_core::HeaderFilter;
use sea_orm::sea_query::{Alias, Expr, SimpleExpr};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, TransactionTrait};

use crate::entities::{bus_event, event};
use crate::error::StoreError;
use crate::event_table::EventTable;
use crate::header_query::predicate;
use crate::sqlite::SqliteEventStore;
use crate::sqlite_only::{pragma_u64, sqlite_only, Pragma, SqliteOnly};

mod declared;
pub mod run;

/// `bus_events` rows that were heartbeats: the header every heartbeat carries.
fn heartbeat_row() -> HeaderFilter {
    HeaderFilter::Has {
        key: "airc.heartbeat.kind".into(),
    }
}

/// `events` rows of any NON-DURABLE class — decided by the header, never the body
/// (the header rule: classify at ingress from headers, never decode payloads). After
/// #1457 no non-durable frame is transcript history, so every such row is dead:
/// cursor-paging requests and replies (stamped `request_response` by the backfill
/// exchange, up to 8 MB each) and any other class that predates the gate. A missing
/// header means durable, exactly as `delivery_class_from_header` reads it.
fn non_durable_row() -> HeaderFilter {
    let key = "airc.delivery_class".to_string();
    HeaderFilter::All(vec![
        HeaderFilter::Has { key: key.clone() },
        HeaderFilter::Not(Box::new(HeaderFilter::Exact {
            key,
            value: "durable".into(),
        })),
    ])
}

/// The rowid span one drain DELETE covers. A rowid window bounds each statement's work by the
/// b-tree range it walks, not by how many rows match, and the pass yields between statements so
/// queued appends and reads take the connection and the write lock.
///
/// The span is sized from what the last statement cost, never a fixed count. The store's pool is
/// one connection (`max_connections(1)`), so a read queued behind a drain statement waits its
/// whole run: on 2026-11-03 a fixed 10,000-row window over a 1.1 GB resident store copy ran
/// about 0.7 s a statement, and the newest-page read beside it took 1.2 s p50, 1.7 s p99. Each
/// statement is timed and the next span scaled toward [`STATEMENT_BUDGET`], growing at most
/// twofold a step so one cheap window never buys an expensive next one.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Window {
    rows: i64,
    /// `None` keeps the span fixed (the tests' small windows).
    budget: Option<std::time::Duration>,
}

/// What one drain statement may hold the connection for: a read queued behind it waits at most
/// this long. Joel, 2026-11-03: reads stay in milliseconds while a drain runs.
const STATEMENT_BUDGET: std::time::Duration = std::time::Duration::from_millis(5);

/// The first statement's span, before anything is measured: small enough that even a slow host
/// stays near the budget, and the second statement is already sized from the first.
const FIRST_WINDOW_ROWS: i64 = 256;

impl Window {
    pub(crate) fn adaptive() -> Self {
        Self {
            rows: FIRST_WINDOW_ROWS,
            budget: Some(STATEMENT_BUDGET),
        }
    }

    #[cfg(test)]
    pub(crate) fn fixed(rows: i64) -> Self {
        Self {
            rows: rows.max(1),
            budget: None,
        }
    }

    pub(crate) fn rows(&self) -> i64 {
        self.rows
    }

    /// Rest after a statement that held the write lock for `took`, as long again: the drain holds
    /// the lock at most half the time, so a writer waiting in SQLite's busy handler (which sleeps
    /// in growing steps, not until the lock frees) finds it free on its next retry. With only a
    /// yield the drain re-took the lock before the sleeper woke: on 2026-11-04, a 979 MB copy, a
    /// reader sharing a handle with a waiting appender went from p99 1.50 ms with no drain to 19.9
    /// ms during it, and the appender's own p99 from 31.7 ms to 40.6 ms. A fixed window (the tests)
    /// only yields.
    pub(crate) async fn rest(&self, took: std::time::Duration) {
        match self.budget {
            Some(_) => tokio::time::sleep(took).await,
            None => tokio::task::yield_now().await,
        }
    }

    /// Size the next span from the statement that covered `self.rows()` in `took`.
    pub(crate) fn after(&mut self, took: std::time::Duration) {
        let Some(budget) = self.budget else { return };
        let scaled = match took.as_micros() {
            0 => self.rows.saturating_mul(2),
            micros => (self.rows as u128 * budget.as_micros() / micros) as i64,
        };
        self.rows = scaled.clamp(1, self.rows.saturating_mul(2));
    }
}

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
    pub before: StoreFootprint,
    /// `events` rows their kind's declared retention says are dead: dead on arrival, or
    /// superseded by a newer row of the same kind and key ([`declared`]).
    pub declared_rows: u64,
    pub heartbeat_rows: u64,
    /// `events` rows of a non-durable class (backfill frames above all).
    pub non_durable_rows: u64,
    pub after: StoreFootprint,
}

/// What a WAL checkpoint did, as SQLite reports it: `busy` when a reader or writer kept it from
/// finishing, the frames in the log, and the frames copied back into the database.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WalCheckpoint {
    pub busy: bool,
    pub log_frames: u64,
    pub checkpointed_frames: u64,
    /// The WAL file was truncated to zero after a complete checkpoint.
    pub truncated: bool,
}

impl WalCheckpoint {
    /// An open read snapshot (or a writer) held part of the log: the frames past it stay, and the
    /// file cannot shrink. On the M5 on 2026-11-03 such a reader grew the WAL to 14 GB and slowed
    /// every reader with it, because nothing checkpointed outside a full vacuum.
    pub fn pinned(&self) -> bool {
        self.busy || self.checkpointed_frames < self.log_frames
    }
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
    /// The one drain: every row a declaration or a dead class marks, and the footprint before
    /// and after.
    pub async fn drain(&self) -> Result<DrainReport, StoreError> {
        self.drain_windowed(Window::adaptive()).await
    }

    pub(crate) async fn drain_windowed(&self, window: Window) -> Result<DrainReport, StoreError> {
        let before = self.footprint().await?;
        let declared_rows = match declared::dead_rows() {
            Some(dead) => self.delete_declared_in_windows(&dead, window).await?,
            None => 0,
        };
        let heartbeat_rows = self
            .delete_in_windows::<bus_event::Entity>(&heartbeat_row(), window)
            .await?;
        let non_durable_rows = self
            .delete_in_windows::<event::Entity>(&non_durable_row(), window)
            .await?;
        Ok(DrainReport {
            before,
            declared_rows,
            heartbeat_rows,
            non_durable_rows,
            after: self.footprint().await?,
        })
    }

    /// Delete the rows of `T` whose headers match `dead`, with their
    /// header rows, one rowid window at a time, yielding between windows so
    /// queued appends and reads take the connection and the write lock
    /// ([`Window`]). Bounded by the table's max rowid at the start:
    /// rows written during the pass are the next pass's.
    async fn delete_in_windows<T: EventTable>(
        &self,
        dead: &HeaderFilter,
        mut window: Window,
    ) -> Result<u64, StoreError> {
        let db = self.connection();
        let rowid = || Expr::col((T::default(), Alias::new("rowid")));
        let max_rowid: Option<Option<i64>> = T::find()
            .select_only()
            .column_as(rowid().max(), "max_rowid")
            .into_tuple()
            .one(db)
            .await?;
        let Some(max_rowid) = max_rowid.flatten() else {
            return Ok(0);
        };
        let owner_id = || Expr::col((T::default(), T::EVENT_ID));
        let mut low = 0i64;
        let mut removed = 0u64;
        while low < max_rowid {
            let high = low.saturating_add(window.rows());
            let started = std::time::Instant::now();
            let in_window: SimpleExpr = rowid()
                .gt(low)
                .and(rowid().lte(high))
                .and(predicate::<T>(dead));
            // The ids first: the header rows the predicate reads go with them.
            let ids: Vec<uuid::Uuid> = T::find()
                .select_only()
                .column_as(owner_id(), "event_id")
                .filter(in_window)
                .into_tuple()
                .all(db)
                .await?;
            if !ids.is_empty() {
                let _write = self.admit_write().await?;
                let txn = db.begin().await?;
                T::Headers::delete_many()
                    .filter(T::HEADER_EVENT_ID.is_in(ids.clone()))
                    .exec(&txn)
                    .await?;
                removed += T::delete_many()
                    .filter(owner_id().is_in(ids))
                    .exec(&txn)
                    .await?
                    .rows_affected;
                txn.commit().await?;
            }
            let took = started.elapsed();
            window.after(took);
            low = high;
            window.rest(took).await;
        }
        Ok(removed)
    }

    /// `page_size × page_count` and `page_size × freelist_count`, from SQLite
    /// itself — not the file's length, which the WAL and the OS decide.
    pub async fn footprint(&self) -> Result<StoreFootprint, StoreError> {
        let page_size = pragma_u64(self.connection(), Pragma::PageSize).await?;
        Ok(StoreFootprint {
            file_bytes: page_size
                .saturating_mul(pragma_u64(self.connection(), Pragma::PageCount).await?),
            free_bytes: page_size
                .saturating_mul(pragma_u64(self.connection(), Pragma::FreelistCount).await?),
        })
    }

    /// The one rebuild a legacy `auto_vacuum=0` file needs: a full VACUUM that also makes it
    /// incremental, then a TRUNCATE checkpoint. It holds every write for the whole rebuild (6.8 s
    /// on a 2.9 GB file), so it runs only in a lifecycle stop window where nothing writes: the
    /// daemon before it serves its file, and `airc store compact` for a store a process opens
    /// itself, while that process is down. An incremental file is returned as it is.
    pub async fn compact(&self) -> Result<StoreFootprint, StoreError> {
        if self.is_incremental().await? {
            return self.footprint().await;
        }
        self.reclaim(Reclaim::Full).await
    }

    /// Give freelist pages back to the filesystem. See [`Reclaim`] for the cost of
    /// each mode; the caller decides when the daemon can afford which.
    pub async fn reclaim(&self, mode: Reclaim) -> Result<StoreFootprint, StoreError> {
        let _write = self.admit_write().await?;
        let db = self.connection();
        match mode {
            Reclaim::Full => {
                // Set the mode first: VACUUM is what makes an auto_vacuum change
                // take effect, so one full pass buys every later incremental one.
                sqlite_only(db, SqliteOnly::AutoVacuumIncremental).await?;
                sqlite_only(db, SqliteOnly::Vacuum).await?;
                // In WAL mode the vacuumed image lands in the log, and the OS file keeps
                // its old length until a checkpoint truncates it (BigMama, 2026-09-26:
                // page accounting said 789 MB while the file was 2.92 GB plus a 1.6 GB
                // WAL). The doctor line reads the file, so the reclaim finishes the job.
                sqlite_only(db, SqliteOnly::WalCheckpointTruncate).await?;
            }
            Reclaim::Incremental => {
                // Bounded steps, yielding between them. A step that frees nothing
                // ends the loop: an `auto_vacuum=0` file ignores the pragma, and
                // waiting on its freelist to shrink would never end.
                let mut free = pragma_u64(self.connection(), Pragma::FreelistCount).await?;
                while free > 0 {
                    sqlite_only(db, SqliteOnly::IncrementalVacuum(RECLAIM_STEP_PAGES)).await?;
                    let now = pragma_u64(self.connection(), Pragma::FreelistCount).await?;
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

    /// Checkpoint the WAL on every pass, not only after a full vacuum: a passive checkpoint first,
    /// which never waits and reports what held it; then, only when it copied every frame with no
    /// one in the way, a TRUNCATE that gives the file back. A pinned log is reported, never waited
    /// on: a TRUNCATE behind an open reader would hold the write lock for the busy timeout.
    pub async fn checkpoint_wal(&self) -> Result<WalCheckpoint, StoreError> {
        let _write = self.admit_write().await?;
        let db = self.connection();
        let passive = checkpoint_row(&sqlite_only(db, SqliteOnly::WalCheckpointPassive).await?)?;
        if passive.pinned() {
            return Ok(passive);
        }
        let truncate = checkpoint_row(&sqlite_only(db, SqliteOnly::WalCheckpointTruncate).await?)?;
        Ok(WalCheckpoint {
            truncated: !truncate.busy,
            ..truncate
        })
    }

    /// Whether the file has LEFT `auto_vacuum = NONE` (0): `INCREMENTAL` (2) returns any
    /// freelist through the bounded steps, and `FULL` (1) shrinks itself on every commit
    /// (Fable on #1462). Either way a full VACUUM is never needed again.
    pub async fn is_incremental(&self) -> Result<bool, StoreError> {
        Ok(pragma_u64(self.connection(), Pragma::AutoVacuum).await? != 0)
    }
}

/// SQLite's `wal_checkpoint` row: `(busy, log, checkpointed)`. A database not in WAL mode reports
/// `-1` for both counts; that is no log at all, read as zero.
fn checkpoint_row(rows: &[sea_orm::QueryResult]) -> Result<WalCheckpoint, StoreError> {
    let row = rows
        .first()
        .ok_or_else(|| StoreError::Migration("wal_checkpoint returned no row".into()))?;
    let busy: i64 = row.try_get_by_index(0)?;
    let frames = |i: usize| -> Result<u64, StoreError> {
        let n: i64 = row.try_get_by_index(i)?;
        Ok(u64::try_from(n).unwrap_or(0)) // unwrap_or: -1 means "not in WAL mode", which has no frames
    };
    Ok(WalCheckpoint {
        busy: busy != 0,
        log_frames: frames(1)?,
        checkpointed_frames: frames(2)?,
        truncated: false,
    })
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
    use sea_orm::PaginatorTrait;

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

        let report = store.drain().await.expect("test: drain");
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
        let again = store.drain().await.expect("test: drain again");
        assert_eq!((again.heartbeat_rows, again.non_durable_rows), (0, 0));
    }

    // what this catches (2026-11-03, IntelMac: 2,205,371 of a resident store's rows were identity
    // cards and 1,626,658 of a project store's were cursor advances): the one drain reading each
    // kind's declared retention. A dead-on-arrival kind goes whole; a newest-per-key kind keeps
    // exactly its newest row per key (room and peer, or room); history stays; the header rows of
    // a deleted event go with it; windows smaller than the table cover it; a second pass finds
    // nothing.
    #[tokio::test]
    async fn the_drain_removes_what_each_kind_declares_dead_and_keeps_the_newest_per_key() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        let store = SqliteEventStore::open_path(&dir.path().join("events.sqlite"))
            .await
            .expect("test: store");
        let (room, other_room) = (RoomId::from_u128(0xa), RoomId::from_u128(0xb));
        let (ada, bo) = (PeerId::from_u128(0xad), PeerId::from_u128(0xb0));
        let row = |kind, room, peer, lamport| {
            let mut event =
                transcript_event_with(room, lamport, serde_json::json!({}), &[("k", "v")]);
            event.kind = kind;
            event.peer_id = peer;
            event
        };
        use TranscriptKind::{DoctrinePublished, IdentityPublished, Message, SubscriptionAdvanced};
        let rows = [
            row(IdentityPublished, room, ada, 1),
            row(IdentityPublished, room, ada, 2),
            row(IdentityPublished, room, ada, 3),
            row(IdentityPublished, room, bo, 4),
            row(IdentityPublished, other_room, ada, 5),
            row(SubscriptionAdvanced, room, ada, 6),
            row(SubscriptionAdvanced, room, bo, 7),
            row(DoctrinePublished, room, ada, 8),
            row(DoctrinePublished, room, bo, 9),
            row(Message, room, ada, 10),
        ];
        for event in rows.iter().cloned() {
            store.append(event).await.expect("test: append");
        }

        let report = store
            .drain_windowed(Window::fixed(3))
            .await
            .expect("test: drain");
        assert_eq!(
            report.declared_rows, 5,
            "two old cards, two cursor advances, one old doctrine"
        );

        let mut left: Vec<u64> = store
            .page_recent(None, 100)
            .await
            .expect("test: recent")
            .iter()
            .map(|event| event.lamport)
            .collect();
        left.sort_unstable();
        assert_eq!(
            left,
            [3, 4, 5, 9, 10],
            "the newest card per room and peer, the newest doctrine per room, the message"
        );
        let headers = crate::entities::event_header::Entity::find()
            .count(store.connection())
            .await
            .expect("test: header rows");
        assert_eq!(headers, 5, "a deleted event's header rows went with it");

        let again = store
            .drain_windowed(Window::fixed(3))
            .await
            .expect("test: drain again");
        assert_eq!(again.declared_rows, 0, "idempotent");
    }

    // what this catches (2026-11-03: a fixed 10,000-row window held the store's one connection
    // ~0.7 s a statement and the newest-page read beside it took 1.2 s p50): a drain statement
    // sized by a count instead of by its cost. The next span scales toward the budget, never
    // more than doubles, never drops below one row; a fixed window never moves.
    #[test]
    fn a_window_is_sized_from_what_the_last_statement_cost() {
        let mut window = Window::adaptive();
        let first = window.rows();
        window.after(STATEMENT_BUDGET * 4);
        assert_eq!(
            window.rows(),
            first / 4,
            "four times over budget: a quarter the span"
        );
        let shrunk = window.rows();
        window.after(std::time::Duration::ZERO);
        assert_eq!(window.rows(), shrunk * 2, "free: at most double");
        window.after(STATEMENT_BUDGET / 100);
        assert_eq!(
            window.rows(),
            shrunk * 4,
            "far under budget: still at most double a step"
        );
        window.after(std::time::Duration::from_secs(60));
        assert_eq!(window.rows(), 1, "never below one row");
        let mut fixed = Window::fixed(3);
        fixed.after(std::time::Duration::from_secs(60));
        assert_eq!(fixed.rows(), 3, "a fixed window never moves");
    }

    // what this catches: the part a DELETE cannot do. The freelist a drain leaves
    // behind is only returned by a vacuum; the full pass shrinks the file and
    // enables incremental reclaim for every pass after it.

    // what this catches (the M5, 2026-11-03, card e719540d): a WAL that never shrinks because
    // nothing checkpoints it outside a full vacuum, and a reader that pins it, unreported. An
    // open read transaction on another connection makes the checkpoint report PINNED without
    // waiting on it; once the reader ends, the next checkpoint copies every frame and truncates
    // the file to zero.
    #[tokio::test]
    async fn an_open_reader_pins_the_wal_and_a_checkpoint_says_so_then_truncates() {
        use sea_orm::{EntityTrait, TransactionTrait};
        let dir = tempfile::tempdir().expect("test: tempdir");
        let path = dir.path().join("events.sqlite");
        let store = SqliteEventStore::open_path(&path)
            .await
            .expect("test: store");
        let sink = SqliteDurableSink::open_path(&path)
            .await
            .expect("test: sink");
        let ch = RoomId::from_u128(0xcafe);
        for counter in 0..200u64 {
            sink.append(&bus_event(ch, counter, &[]))
                .await
                .expect("test: row");
        }
        // A second connection holds a read snapshot open, as the M5's supervisor join did.
        let reader = sea_orm::Database::connect(format!("sqlite://{}?mode=ro", path.display()))
            .await
            .expect("test: reader");
        let snapshot = reader.begin().await.expect("test: read txn");
        crate::entities::bus_event::Entity::find()
            .one(&snapshot)
            .await
            .expect("test: snapshot read");
        // Writes after the snapshot stay in the log past it.
        for counter in 200..400u64 {
            sink.append(&bus_event(ch, counter, &[]))
                .await
                .expect("test: row");
        }
        let started = std::time::Instant::now();
        let pinned = store.checkpoint_wal().await.expect("test: checkpoint");
        assert!(pinned.pinned(), "an open reader holds the log: {pinned:?}");
        assert!(
            started.elapsed() < crate::sqlite::POOL_ACQUIRE_TIMEOUT / 2,
            "a pinned checkpoint returns at once, never waiting out the busy timeout behind the reader"
        );
        assert!(
            !pinned.truncated,
            "a pinned log is never truncated (no wait behind the reader)"
        );

        snapshot.rollback().await.expect("test: end the read");
        drop(reader);
        let clear = store.checkpoint_wal().await.expect("test: checkpoint");
        assert!(
            !clear.pinned(),
            "the reader gone, every frame is checkpointed: {clear:?}"
        );
        assert!(clear.truncated, "and the file is given back");
        let wal = std::fs::metadata(dir.path().join("events.sqlite-wal"))
            .map(|m| m.len())
            .unwrap_or(0); // unwrap_or: a missing -wal file is an empty log
        assert_eq!(wal, 0, "the WAL file is truncated on disk");
    }

    /// Heartbeat rows enough to occupy pages a drain will free.
    async fn heartbeats(sink: &SqliteDurableSink, ch: RoomId, counters: std::ops::Range<u64>) {
        for counter in counters {
            sink.append(&bus_event(ch, counter, &[("airc.heartbeat.kind", "alive")]))
                .await
                .expect("test: heartbeat row");
        }
    }

    // what this catches (Fable on #1653: stores were born auto_vacuum=0, so every one would one day
    // need the full rebuild that holds every write): a new store that is not incremental from
    // birth. A drain's freed pages come back in bounded steps, the file shrinks on disk, and no
    // rebuild runs. Windows far below the row count cover the whole table.
    #[tokio::test]
    async fn a_new_store_is_incremental_and_a_drain_gives_its_pages_back_in_steps() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        let path = dir.path().join("events.sqlite");
        let store = SqliteEventStore::open_path(&path)
            .await
            .expect("test: store");
        let sink = SqliteDurableSink::open_path(&path)
            .await
            .expect("test: sink");
        assert!(
            store.is_incremental().await.expect("test: mode"),
            "born incremental"
        );
        heartbeats(&sink, RoomId::from_u128(0xbeef), 0..2_000).await;
        let before = store.footprint().await.expect("test: footprint");
        let drained = store
            .drain_windowed(Window::fixed(64))
            .await
            .expect("test: drain");
        assert_eq!(drained.heartbeat_rows, 2_000);
        assert!(
            drained.after.free_bytes > 0,
            "a DELETE leaves pages on the freelist"
        );
        assert_eq!(
            run::reclaim_for(drained.after, true),
            Some(Reclaim::Incremental)
        );
        let after = store
            .reclaim(Reclaim::Incremental)
            .await
            .expect("test: steps");
        assert_eq!(
            after.free_bytes, 0,
            "the bounded steps returned the freelist"
        );
        assert!(
            after.file_bytes < before.file_bytes,
            "{before:?} -> {after:?}"
        );
        store.checkpoint_wal().await.expect("test: checkpoint");
        let on_disk = std::fs::metadata(&path).expect("test: db file").len();
        assert!(
            on_disk <= after.file_bytes + 65_536,
            "the file on disk is the page accounting: {on_disk} vs {}",
            after.file_bytes
        );
    }

    // what this catches: a legacy auto_vacuum=0 file that a pass cannot shrink and that nothing
    // converts. A pass leaves it alone (it reports the freelist); `compact`, in a stop window, gives
    // the pages back and makes it incremental, after which it is never rebuilt again.
    #[tokio::test]
    async fn compact_converts_a_legacy_file_once() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        let path = dir.path().join("events.sqlite");
        // A store as every binary before #1653 created it: migrated with no vacuum mode set.
        let raw = sea_orm::Database::connect(crate::sqlite::sqlite_file_url(&path))
            .await
            .expect("test: connect");
        <crate::migration::Migrator as sea_orm_migration::MigratorTrait>::up(&raw, None)
            .await
            .expect("test: legacy migrate");
        drop(raw);
        let store = SqliteEventStore::open_path(&path)
            .await
            .expect("test: store");
        let sink = SqliteDurableSink::open_path(&path)
            .await
            .expect("test: sink");
        assert!(!store.is_incremental().await.expect("test: mode"), "legacy");
        heartbeats(&sink, RoomId::from_u128(0xfeed), 0..2_000).await;
        let drained = store.drain().await.expect("test: drain");
        assert!(drained.after.free_bytes > 0);
        assert_eq!(
            run::reclaim_for(drained.after, false),
            None,
            "a pass never rebuilds"
        );
        let compacted = store.compact().await.expect("test: compact");
        assert_eq!(
            compacted.free_bytes, 0,
            "the rebuild returned every free page"
        );
        assert!(
            store.is_incremental().await.expect("test: mode"),
            "and made it incremental"
        );
        assert_eq!(
            store.compact().await.expect("test: again"),
            store.footprint().await.expect("test: footprint"),
            "an incremental file is left as it is"
        );
    }
}
