//! The drain's background task, the same for every store: the daemon's, a project's, a
//! resident's opened in-process. One task per store, its own `tokio::time::interval`, never on
//! an open or boot path; each pass is bounded windows in short transactions with a yield
//! between them ([`SqliteEventStore::drain`]), so it never holds a transaction or a lock across
//! a wait, and it can stop anywhere and resume on the next pass.
//!
//! Two costs, two gates. The DELETEs run on every pass. Giving the pages back needs a full
//! `VACUUM` ONCE per file (an `auto_vacuum=0` file cannot be shrunk any other way): that holds
//! the write lock for the whole rebuild, tens of seconds on a 2.7 GB file, and a send that
//! lands inside it waits or times out. So the full pass runs only when the freelist is worth it
//! AND the host says it is idle, and it flips the file to incremental mode so every later pass
//! returns pages in short transactions.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use super::{DrainReport, Reclaim, StoreFootprint, WalCheckpoint};
use crate::error::StoreError;
use crate::sqlite::SqliteEventStore;

/// The first pass waits for the host to settle (the boot window is when every peer backfills
/// against the file), but not long: the pressure this drains is what made a node dark, so it
/// is not a nightly.
pub const FIRST_PASS_AFTER: Duration = Duration::from_secs(120);

/// Then steady state: what a pass drains grows only from the writers between passes.
pub const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// A full vacuum is worth its lock when at least this share of the file is freelist. Below it,
/// the incremental pass on every tick is enough.
pub const FULL_RECLAIM_AT_FREE_PERCENT: u64 = 25;

/// Past this share the file is mostly air and the full vacuum runs whether the host is idle or
/// not (card 4de825ce): on BigMama the first pass after #1459 left 2,026 MB of 2,917 on the
/// freelist and the quiet gate never opened (a serving node is never quiet), so nothing came
/// back until an operator ran VACUUM by hand (6.8 s of held writes for 2.9 GB). A minute of
/// held writes is cheaper than two gigabytes of dead file forever.
pub const FULL_RECLAIM_REGARDLESS_AT_FREE_PERCENT: u64 = 60;

/// PURE: which reclaim a pass should run, from the footprint, the file's vacuum mode and the
/// idle signal. `None` when the freelist is not worth a transaction at all.
///
/// A full VACUUM is needed exactly ONCE per file: to leave `auto_vacuum=0` (card d0d5a927).
/// Once the file is `INCREMENTAL`, any freelist comes back through bounded
/// `incremental_vacuum` steps, so a full rebuild of an incremental file rewrites the whole file
/// for nothing.
pub fn reclaim_for(after: StoreFootprint, incremental: bool, is_idle: bool) -> Option<Reclaim> {
    if after.free_bytes == 0 {
        return None;
    }
    if incremental {
        return Some(Reclaim::Incremental);
    }
    let free = after.free_percent();
    if free >= FULL_RECLAIM_REGARDLESS_AT_FREE_PERCENT
        || (free >= FULL_RECLAIM_AT_FREE_PERCENT && is_idle)
    {
        Some(Reclaim::Full)
    } else {
        Some(Reclaim::Incremental)
    }
}

/// What one pass did, every step's outcome kept: a drain that silently stopped running is how
/// a store got to 2.65 GB, so the host logs every pass, failures included.
#[derive(Debug)]
pub struct PassReport {
    pub drain: Result<DrainReport, StoreError>,
    pub reclaim: Option<Result<(Reclaim, StoreFootprint), StoreError>>,
    /// `None` when the pass never reached the checkpoint (the drain's connection did not open).
    pub wal: Option<Result<WalCheckpoint, StoreError>>,
}

impl std::fmt::Display for PassReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |bytes: u64| bytes / 1_000_000;
        match &self.drain {
            Ok(report) => write!(
                f,
                "drained {} declared, {} heartbeat and {} non-durable rows; file {} MB before",
                report.declared_rows,
                report.heartbeat_rows,
                report.non_durable_rows,
                mb(report.before.file_bytes),
            )?,
            Err(error) => write!(f, "drain failed: {error}")?,
        }
        match &self.reclaim {
            Some(Ok((mode, after))) => write!(
                f,
                "; {mode:?} reclaim: file {} MB, free {} MB ({}%)",
                mb(after.file_bytes),
                mb(after.free_bytes),
                after.free_percent(),
            )?,
            Some(Err(error)) => write!(f, "; reclaim failed: {error}")?,
            None => write!(f, "; nothing to reclaim")?,
        }
        match &self.wal {
            None => write!(f, "; WAL not reached"),
            Some(Ok(wal)) if wal.pinned() => write!(
                f,
                "; WAL PINNED: {} of {} frames checkpointed (busy={}), an open read transaction \
                 holds the log, which cannot shrink until it ends",
                wal.checkpointed_frames, wal.log_frames, wal.busy,
            ),
            Some(Ok(wal)) => write!(
                f,
                "; WAL checkpointed {} frames{}",
                wal.checkpointed_frames,
                if wal.truncated {
                    ", file truncated"
                } else {
                    ""
                },
            ),
            Some(Err(error)) => write!(f, "; WAL checkpoint failed: {error}"),
        }
    }
}

/// One pass: the drain, then the reclaim the footprint and the idle signal allow, then the WAL
/// checkpoint. Every pass checkpoints, not only after a full vacuum: a reader that never lets
/// go is how the M5's WAL reached 14 GB beside a 2.9 GB file on 2026-11-03 (card e719540d).
pub async fn pass(store: &SqliteEventStore, is_idle: bool) -> PassReport {
    let drain = store.drain().await;
    let reclaim = match &drain {
        Ok(report) => {
            // Unknown mode reads as NOT incremental: the worst case is the one-time full pass
            // this file would have needed anyway.
            let incremental = store.is_incremental().await.unwrap_or(false); // unwrap_or: an unreadable pragma falls to the full-pass decision, never to skipping a reclaim
            match reclaim_for(report.after, incremental, is_idle) {
                Some(mode) => Some(store.reclaim(mode).await.map(|after| (mode, after))),
                None => None,
            }
        }
        Err(_) => None,
    };
    PassReport {
        drain,
        reclaim,
        wal: Some(store.checkpoint_wal().await),
    }
}

/// The drain's background task for the store file at `path`: a first pass after
/// [`FIRST_PASS_AFTER`], then one every [`INTERVAL`], while `any_open()` says a handle on the file
/// is open in this process. It ends when none is, or when `shutdown` resolves (then it returns
/// [`Ended::Shutdown`]). `report` is the host's log for every pass.
///
/// The drain runs on its OWN connection to the file, opened at its first pass. A store's pool is
/// one connection, so a read queued behind a drain statement on it waits the statement out; on
/// another connection, WAL lets it read beside the writer. Measured 2026-11-03 on a 1.1 GB resident
/// store copy, newest page (50) read every 100 ms: no drain p50 1.33 ms, p99 1.97 ms; during the
/// drain on the draining connection p50 41 ms, p99 69 ms, on a second connection p50 1.54 ms, p99
/// 3.27 ms; during the one full VACUUM (2.6 s) the draining connection blocked 2.5 s, a second
/// connection read p50 1.10 ms, p99 1.82 ms.
pub async fn run(
    path: PathBuf,
    any_open: impl Fn() -> bool,
    shutdown: impl Future<Output = ()>,
    is_idle: impl Fn() -> bool,
    report: impl Fn(&PassReport),
) -> Ended {
    tokio::pin!(shutdown);
    tokio::select! {
        _ = &mut shutdown => return Ended::Shutdown,
        _ = tokio::time::sleep(FIRST_PASS_AFTER) => {}
    }
    let mut ticker = tokio::time::interval(INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    // The first tick is immediate: the first pass runs now.
    ticker.tick().await;
    let mut own: Option<SqliteEventStore> = None;
    loop {
        if !any_open() {
            return Ended::NoHandle;
        }
        let opened = match own.take() {
            Some(store) => Ok(store),
            None => SqliteEventStore::open_path(&path).await,
        };
        match opened {
            Ok(store) => {
                report(&pass(&store, is_idle()).await);
                own = Some(store);
            }
            Err(error) => report(&PassReport {
                drain: Err(error),
                reclaim: None,
                wal: None,
            }),
        }
        tokio::select! {
            _ = &mut shutdown => return Ended::Shutdown,
            _ = ticker.tick() => {}
        }
    }
}

/// Why a drain task ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// Every handle on the file closed; its registration went with the last one.
    NoHandle,
    /// The host's `shutdown` resolved while handles may still be open.
    Shutdown,
}

/// Every open handle on each store file this process drains, by path: one task per file, kept
/// while any handle on the file lives, so a file opened by a short-lived handle first and a
/// long-lived one after is drained for as long as either lives.
static DRAINING: Mutex<Option<HashMap<PathBuf, Vec<Weak<SqliteEventStore>>>>> = Mutex::new(None);

/// Whether a handle on the file at `path` is still open, pruning the closed ones; when none is,
/// the file's entry goes, under the same lock a new handle registers with.
fn any_open(path: &Path) -> bool {
    // A poisoned lock still holds a valid map: no invariant spans an entry's update.
    let mut draining = DRAINING.lock().unwrap_or_else(|p| p.into_inner());
    let files = draining.get_or_insert_with(HashMap::new);
    let Some(handles) = files.get_mut(path) else {
        return false;
    };
    handles.retain(|handle| handle.strong_count() > 0);
    if handles.is_empty() {
        files.remove(path);
        return false;
    }
    true
}

/// Register `store`, a handle on the file at `path`, with the file's drain, starting the drain
/// (on its own connection, [`run`]) as a background task on the current tokio runtime when this
/// process does not already run one for the file. Returns whether it started one; `shutdown`, `is_idle` and `report` are the
/// starting host's.
pub fn spawn(
    store: &Arc<SqliteEventStore>,
    path: &Path,
    shutdown: impl Future<Output = ()> + Send + 'static,
    is_idle: impl Fn() -> bool + Send + Sync + 'static,
    report: impl Fn(&PassReport) + Send + Sync + 'static,
) -> bool {
    let path = path.to_path_buf();
    {
        let mut draining = DRAINING.lock().unwrap_or_else(|p| p.into_inner());
        let files = draining.get_or_insert_with(HashMap::new);
        if let Some(handles) = files.get_mut(&path) {
            handles.push(Arc::downgrade(store));
            return false;
        }
        files.insert(path.clone(), vec![Arc::downgrade(store)]);
    }
    tokio::spawn(async move {
        let ended = run(path.clone(), || any_open(&path), shutdown, is_idle, report).await;
        if ended == Ended::Shutdown {
            // The task is gone but handles may remain: the next open starts a new one.
            let mut draining = DRAINING.lock().unwrap_or_else(|p| p.into_inner());
            if let Some(files) = draining.as_mut() {
                files.remove(&path);
            }
        }
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the two gates on the expensive pass. A full vacuum on a busy host
    // would hold every send behind a rebuild; skipping reclaim entirely below the threshold
    // would let the freelist ride forever; reclaiming an empty freelist is a transaction for
    // nothing.
    #[test]
    fn a_full_reclaim_needs_both_a_worthwhile_freelist_and_a_quiet_host() {
        let footprint = |free_bytes| StoreFootprint {
            file_bytes: 2_700_000_000,
            free_bytes,
        };
        let (heavy, light, clean) = (
            footprint(1_300_000_000),
            footprint(100_000_000),
            footprint(0),
        );
        assert_eq!(reclaim_for(heavy, false, true), Some(Reclaim::Full));
        assert_eq!(
            reclaim_for(heavy, false, false),
            Some(Reclaim::Incremental),
            "busy: never the rebuild"
        );
        assert_eq!(
            reclaim_for(light, false, true),
            Some(Reclaim::Incremental),
            "not worth the lock"
        );
        assert_eq!(
            reclaim_for(clean, false, true),
            None,
            "nothing to give back"
        );
        // card d0d5a927: an INCREMENTAL file never takes the full rebuild, however much is
        // free, because the bounded steps return it all.
        assert_eq!(reclaim_for(heavy, true, true), Some(Reclaim::Incremental));
        // card 4de825ce: BigMama's first pass, 2,026 MB of 2,917 free, on a host that is never
        // quiet: the rebuild runs anyway, or the file stays dead weight forever.
        let air = StoreFootprint {
            file_bytes: 2_917_000_000,
            free_bytes: 2_026_000_000,
        };
        assert_eq!(reclaim_for(air, true, false), Some(Reclaim::Incremental));
        assert_eq!(
            reclaim_for(air, false, false),
            Some(Reclaim::Full),
            "mostly air: quiet or not"
        );
    }
}
