//! The drain's background task, the same for every store: the daemon's, a project's, a
//! resident's opened in-process. One task per store, its own `tokio::time::interval`, never on
//! an open or boot path; each pass is bounded windows in short transactions with a yield
//! between them ([`SqliteEventStore::drain`]), so it never holds a transaction or a lock across
//! a wait, and it can stop anywhere and resume on the next pass.
//!
//! A pass never rebuilds a file. Its deletes return pages to the freelist, and an incremental file
//! (every store created since #1653, `migration::apply_migrations`) gives them back in bounded
//! `incremental_vacuum` steps. A legacy `auto_vacuum=0` file can only shrink by a full `VACUUM`,
//! which holds every write for the whole rebuild (6.8 s on a 2.9 GB file), so a pass only reports
//! its freelist; the rebuild is [`SqliteEventStore::compact`], run in a lifecycle stop window where
//! nothing writes (Fable on #1653).

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

/// PURE: the reclaim a pass runs: the bounded incremental steps, on an incremental file with a
/// freelist. A legacy `auto_vacuum=0` file is never rebuilt by a pass (`None`): its freelist waits
/// for [`SqliteEventStore::compact`] in a stop window.
pub fn reclaim_for(after: StoreFootprint, incremental: bool) -> Option<Reclaim> {
    (incremental && after.free_bytes > 0).then_some(Reclaim::Incremental)
}

/// What one pass did, every step's outcome kept: a drain that silently stopped running is how
/// a store got to 2.65 GB, so the host logs every pass, failures included.
#[derive(Debug)]
pub struct PassReport {
    pub drain: Result<DrainReport, StoreError>,
    pub reclaim: Option<Result<StoreFootprint, StoreError>>,
    /// A legacy `auto_vacuum=0` file's freelist, bytes, that only [`SqliteEventStore::compact`]
    /// in a stop window gives back; `None` for an incremental file.
    pub awaiting_compaction: Option<u64>,
    /// `None` when the pass never reached the checkpoint (the drain's connection did not open).
    pub wal: Option<Result<WalCheckpoint, StoreError>>,
}

impl std::fmt::Display for PassReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mb = |bytes: u64| bytes / 1_000_000;
        match &self.drain {
            Ok(report) => write!(
                f,
                "drained {} declared rows in {} windows (gate held {} ms in all, {} ms at most), {} \
                 heartbeat and {} non-durable rows; file {} MB before",
                report.declared_rows,
                report.declared_windows.windows,
                report.declared_windows.held.as_millis(),
                report.declared_windows.max_held.as_millis(),
                report.heartbeat_rows,
                report.non_durable_rows,
                mb(report.before.file_bytes),
            )?,
            Err(error) => write!(f, "drain failed: {error}")?,
        }
        if let Ok(report) = &self.drain {
            if report.declared_windows.stopped_pinned {
                write!(
                    f,
                    "; stopped early: a reader holds the WAL, the next pass resumes"
                )?;
            }
        }
        match &self.reclaim {
            Some(Ok(after)) => write!(
                f,
                "; reclaimed: file {} MB, free {} MB ({}%)",
                mb(after.file_bytes),
                mb(after.free_bytes),
                after.free_percent(),
            )?,
            Some(Err(error)) => write!(f, "; reclaim failed: {error}")?,
            None => write!(f, "; nothing to reclaim")?,
        }
        if let Some(free) = self.awaiting_compaction {
            write!(
                f,
                "; legacy file: {} MB free awaits compaction in a stop window",
                mb(free)
            )?;
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
pub async fn pass(store: &SqliteEventStore) -> PassReport {
    let drain = store.drain().await;
    let (reclaim, awaiting_compaction) = match &drain {
        Ok(report) => match store.is_incremental().await {
            Ok(incremental) => (
                match reclaim_for(report.after, incremental) {
                    Some(mode) => Some(store.reclaim(mode).await),
                    None => None,
                },
                (!incremental && report.after.free_bytes > 0).then_some(report.after.free_bytes),
            ),
            Err(error) => (Some(Err(error)), None),
        },
        Err(_) => (None, None),
    };
    PassReport {
        drain,
        reclaim,
        awaiting_compaction,
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
            None => SqliteEventStore::open_path_for_drain(&path).await,
        };
        match opened {
            Ok(store) => {
                report(&pass(&store).await);
                own = Some(store);
            }
            Err(error) => report(&PassReport {
                drain: Err(error),
                reclaim: None,
                awaiting_compaction: None,
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
/// process does not already run one for the file. Returns whether it started one; `shutdown` and `report` are the
/// starting host's.
pub fn spawn(
    store: &Arc<SqliteEventStore>,
    path: &Path,
    shutdown: impl Future<Output = ()> + Send + 'static,
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
        let ended = run(path.clone(), || any_open(&path), shutdown, report).await;
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

    // what this catches (Fable on #1653: the full VACUUM held every send for the whole rebuild on
    // a serving node, 6.8 s on 2.9 GB): a pass that rebuilds a file. An incremental file with a
    // freelist takes the bounded steps; a legacy file and a file with nothing free take none.
    #[test]
    fn a_pass_never_rebuilds_a_file_it_only_takes_bounded_steps() {
        let footprint = |free_bytes| StoreFootprint {
            file_bytes: 2_917_000_000,
            free_bytes,
        };
        assert_eq!(
            reclaim_for(footprint(2_026_000_000), true),
            Some(Reclaim::Incremental)
        );
        assert_eq!(
            reclaim_for(footprint(2_026_000_000), false),
            None,
            "legacy: waits for a stop window"
        );
        assert_eq!(
            reclaim_for(footprint(0), true),
            None,
            "nothing to give back"
        );
    }
}
