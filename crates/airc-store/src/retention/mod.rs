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
    /// How the declared deletes held the file's write gate: the drain's rate and its cost to
    /// every other writer.
    pub declared_windows: WindowStats,
    pub heartbeat_rows: u64,
    /// `events` rows of a non-durable class (backfill frames above all).
    pub non_durable_rows: u64,
    pub after: StoreFootprint,
}

/// The drain's pace: how long it rests after each window, steered by the foreground latency it can
/// see (Fable on #1653: a loop on the measurement, never a fixed duty, since "every time we use a
/// constant we get stuck at that constant"). Above the bar (this store's no-drain latency, measured
/// before the pass, plus one window's budget), the rest doubles; under it, it shrinks by one budget
/// (AIMD). So the drain converges on the fastest pace that keeps the foreground within its bar, on
/// any disk, and moves both ways.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Pace {
    rest: std::time::Duration,
    /// `None`: never rests (the tests' fixed windows).
    bar: Option<std::time::Duration>,
}

impl Pace {
    /// A pace against `baseline`, this store's foreground latency with no drain running.
    pub(crate) fn against(baseline: std::time::Duration) -> Self {
        Self {
            rest: std::time::Duration::ZERO,
            bar: Some(baseline + STATEMENT_BUDGET),
        }
    }

    #[cfg(test)]
    pub(crate) fn never() -> Self {
        Self {
            rest: std::time::Duration::ZERO,
            bar: None,
        }
    }

    /// Steer by the slowest foreground write since the last window, or hold when there was none:
    /// the pace moves only on evidence. Shrinking on every window with no write in it (a write every
    /// 100 ms, a window every ~25 ms) gave back each excursion's rest before the next write could
    /// show it was still needed: 580 ms rested over 18,014 windows on 2026-11-04, appends' p99 still
    /// 23.3 ms against 3.0 ms with no drain.
    pub(crate) fn observe(&mut self, slowest_write: Option<std::time::Duration>) {
        let (Some(bar), Some(foreground)) = (self.bar, slowest_write) else {
            return;
        };
        self.rest = match foreground > bar {
            true => (self.rest * 2).max(STATEMENT_BUDGET),
            false => self.rest.saturating_sub(STATEMENT_BUDGET),
        };
    }

    pub(crate) fn rest(&self) -> std::time::Duration {
        self.rest
    }

    /// The foreground latency the drain is held to; zero when it never rests.
    pub(crate) fn bar(&self) -> std::time::Duration {
        self.bar.unwrap_or_default() // unwrap_or_default: a pace that never rests holds the drain to nothing
    }
}

/// How a drain's windows held the file's write gate, each timed from the grant: how many there
/// were, how long they held it in all and at most (what one waiting writer can pay), and where the
/// time went.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WindowStats {
    pub windows: u64,
    pub held: std::time::Duration,
    pub max_held: std::time::Duration,
    pub header_delete: std::time::Duration,
    pub event_delete: std::time::Duration,
    pub commit: std::time::Duration,
    /// The drain's PASSIVE checkpoints after each window, outside the gate: what used to run inside
    /// every commit.
    pub checkpoint: std::time::Duration,
    /// The pass stopped early: a reader held a snapshot no checkpoint could move past. The next
    /// pass resumes.
    pub stopped_pinned: bool,
    /// Rested between windows by the pace ([`Pace`]).
    pub rested: std::time::Duration,
    /// Windows whose slowest foreground write was over the bar, with that write's layers summed,
    /// and how many of those writes overlapped a drain checkpoint: where the tail goes.
    pub slow_writes: u64,
    pub slow_overlapping_checkpoint: u64,
    pub slow_wait: std::time::Duration,
    pub slow_admission: std::time::Duration,
    pub slow_statement: std::time::Duration,
    pub slow_commit: std::time::Duration,
    /// The foreground latency the pace steered against: the bar it held the drain to.
    pub bar: std::time::Duration,
}

impl WindowStats {
    /// Count a window whose slowest foreground write was over the bar.
    pub(super) fn slow(&mut self, layers: crate::write_gate::WriteLayers) {
        self.slow_writes += 1;
        self.slow_overlapping_checkpoint += u64::from(layers.overlapped_checkpoint);
        self.slow_wait += layers.wait;
        self.slow_admission += layers.admission;
        self.slow_statement += layers.statement;
        self.slow_commit += layers.commit;
    }

    fn add(
        &mut self,
        header_delete: std::time::Duration,
        event_delete: std::time::Duration,
        commit: std::time::Duration,
    ) {
        let held = header_delete + event_delete + commit;
        self.windows += 1;
        self.held += held;
        self.max_held = self.max_held.max(held);
        self.header_delete += header_delete;
        self.event_delete += event_delete;
        self.commit += commit;
    }
}

/// How a drain window's checkpoint ended: its frames copied (and how long that took), or a reader
/// holding a snapshot that no try moves past, which stops the pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Checkpointed {
    Copied(std::time::Duration),
    Pinned(WalCheckpoint),
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
        let baseline = self.foreground_baseline().await?;
        self.drain_windowed(Window::adaptive(), Pace::against(baseline))
            .await
    }

    pub(crate) async fn drain_windowed(
        &self,
        window: Window,
        mut pace: Pace,
    ) -> Result<DrainReport, StoreError> {
        let before = self.footprint().await?;
        let (declared_rows, declared_windows) = match declared::dead_rows() {
            Some(dead) => {
                self.delete_declared_in_windows(&dead, window, &mut pace)
                    .await?
            }
            None => (0, WindowStats::default()),
        };
        let heartbeat_rows = self
            .delete_in_windows::<bus_event::Entity>(&heartbeat_row(), window, &mut pace)
            .await?;
        let non_durable_rows = self
            .delete_in_windows::<event::Entity>(&non_durable_row(), window, &mut pace)
            .await?;
        Ok(DrainReport {
            before,
            declared_rows,
            declared_windows,
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
        pace: &mut Pace,
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
            let held = started.elapsed();
            window.after(held);
            if let Checkpointed::Pinned(_) = self.checkpoint_after_window(held).await? {
                break;
            }
            let _ = self.rest_at_pace(pace).await?;
            low = high;
            tokio::task::yield_now().await;
        }
        Ok(removed)
    }

    /// This store's foreground latency with no drain running, measured before a pass: the longest
    /// of a hundred cheap timed reads (a p99 needs about a hundred samples). The pace's bar is this
    /// plus one window's budget ([`Pace`]).
    pub(super) async fn foreground_baseline(&self) -> Result<std::time::Duration, StoreError> {
        let mut longest = std::time::Duration::ZERO;
        for _ in 0..100 {
            longest = longest.max(self.timed_read().await?);
        }
        self.gate().take_slowest_write();
        Ok(longest)
    }

    /// One cheap read on this connection, timed: what a reader beside the drain pays now.
    async fn timed_read(&self) -> Result<std::time::Duration, StoreError> {
        let started = std::time::Instant::now();
        let _: Option<uuid::Uuid> = event::Entity::find()
            .select_only()
            .column(event::Column::EventId)
            .limit(1)
            .into_tuple()
            .one(self.connection())
            .await?;
        Ok(started.elapsed())
    }

    /// After a window: steer the pace by the slowest foreground write since the last window (its I/O
    /// included), and rest as long as the pace says. Returns the rest.
    /// Returns the rest, and the slowest foreground write's layers when it was over the bar.
    pub(super) async fn rest_at_pace(
        &self,
        pace: &mut Pace,
    ) -> Result<(std::time::Duration, Option<crate::write_gate::WriteLayers>), StoreError> {
        let slowest = self.gate().take_slowest_write();
        pace.observe(slowest.map(|layers| layers.total()));
        let rest = pace.rest();
        if !rest.is_zero() {
            tokio::time::sleep(rest).await;
        }
        Ok((rest, slowest.filter(|layers| layers.total() > pace.bar())))
    }

    /// The checkpoint after a drain window: PASSIVE, outside the write gate and custody, on the
    /// drain's own connection, whose commits run no automatic checkpoint
    /// ([`crate::sqlite::drain_options`]). PASSIVE never takes the write lock, so writers append
    /// while it copies, and their own automatic checkpoint then finds only their own frames. A
    /// checkpoint changes no data, so it needs no write admission. Returns how long it took.
    ///
    /// The next window waits for this one's frames to be copied: a PASSIVE checkpoint copies only up
    /// to the oldest open reader's snapshot, and on 2026-11-04 (a 982 MB copy, a reader every 100
    /// ms) the uncopied tail grew the WAL to 1,782 MB. Every append's own automatic checkpoint then
    /// copied from it (p99 25.1 ms against 3.7 ms with no drain), and reads walked it. So while a
    /// reader pins frames this window wrote, it waits (as long as the window held the gate, at
    /// least the statement budget) and checkpoints again, for as long as each try copies more: the
    /// drain goes at the pace its writes are copied, and the WAL stays small. A try that copies
    /// nothing new means a snapshot is held open (the M5's 14 GB WAL on 2026-11-03), so the pass
    /// stops there, reported, and the next pass resumes; it never waits on a reader that does not
    /// move.
    pub(super) async fn checkpoint_after_window(
        &self,
        held: std::time::Duration,
    ) -> Result<Checkpointed, StoreError> {
        let started = std::time::Instant::now();
        self.gate().checkpoint_edge();
        let ended = self.checkpoint_until_copied(held).await;
        self.gate().checkpoint_edge();
        ended.map(|ended| match ended {
            Checkpointed::Copied(_) => Checkpointed::Copied(started.elapsed()),
            pinned => pinned,
        })
    }

    async fn checkpoint_until_copied(
        &self,
        held: std::time::Duration,
    ) -> Result<Checkpointed, StoreError> {
        let started = std::time::Instant::now();
        let mut copied = None;
        loop {
            let rows = sqlite_only(self.connection(), SqliteOnly::WalCheckpointPassive).await?;
            let wal = checkpoint_row(&rows)?;
            if !wal.pinned() {
                return Ok(Checkpointed::Copied(started.elapsed()));
            }
            if copied == Some(wal.checkpointed_frames) {
                return Ok(Checkpointed::Pinned(wal));
            }
            copied = Some(wal.checkpointed_frames);
            tokio::time::sleep(held.max(STATEMENT_BUDGET)).await;
        }
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
mod tests;
