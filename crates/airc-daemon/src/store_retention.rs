//! The daemon's tick on the drain (card 6781d7e9): `events.sqlite` must not grow
//! without bound under the daemon that writes it. Same shape as
//! [`crate::auto_update`]: an owned loop on the daemon runtime, exiting on the
//! shared shutdown notifier, with an `is_idle` predicate the caller supplies.
//!
//! Two costs, two gates. The DELETEs (`drain_dead_rows`) are short write
//! transactions and run on every tick. Giving the pages back needs a full `VACUUM`
//! ONCE per file (an `auto_vacuum=0` file cannot be shrunk any other way): that
//! holds the write lock for the whole rebuild — tens of seconds on a 2.7 GB file —
//! and a send that lands inside it waits or times out. So the full pass runs only
//! when the freelist is worth it AND the mesh is quiet, and it flips the file to
//! incremental mode so every later tick returns pages in short transactions.

use std::sync::Arc;
use std::time::Duration;

use airc_store::retention::{Reclaim, StoreFootprint};
use airc_store::SqliteEventStore;

/// The first pass waits for the daemon to settle — the boot window is when every
/// peer backfills against this file — but not long: the pressure this drains is
/// what made the node dark, so it is not a nightly.
pub const FIRST_PASS_AFTER: Duration = Duration::from_secs(120);
/// Then steady state. Heartbeats no longer land (#1458) and paging frames route
/// live (#1457), so between passes the dead classes grow only from older peers.
pub const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);
/// A full vacuum is worth its lock when at least this share of the file is
/// freelist. Below it, the incremental pass on every tick is enough.
pub const FULL_RECLAIM_AT_FREE_PERCENT: u64 = 25;

/// PURE: which reclaim a pass should run, from the footprint and the idle signal.
/// `None` when the freelist is not worth a transaction at all.
pub fn reclaim_for(after: StoreFootprint, is_idle: bool) -> Option<Reclaim> {
    if after.free_bytes == 0 {
        return None;
    }
    if after.free_percent() >= FULL_RECLAIM_AT_FREE_PERCENT && is_idle {
        Some(Reclaim::Full)
    } else {
        Some(Reclaim::Incremental)
    }
}

/// One drain pass: delete the dead classes, then reclaim what the footprint and
/// the idle signal allow. Loud on every outcome — the daemon has no tracing dep;
/// stderr is its log sink — because a drain that silently stopped running is how
/// the file got to 2.65 GB.
pub async fn pass(store: &SqliteEventStore, is_idle: bool) {
    let report = match store.drain_dead_rows().await {
        Ok(report) => report,
        Err(error) => {
            eprintln!("airc store retention: drain failed: {error}");
            return;
        }
    };
    let reclaim = reclaim_for(report.after, is_idle);
    let footprint = match reclaim {
        Some(mode) => match store.reclaim(mode).await {
            Ok(footprint) => footprint,
            Err(error) => {
                eprintln!("airc store retention: {mode:?} reclaim failed: {error}");
                report.after
            }
        },
        None => report.after,
    };
    eprintln!(
        "airc store retention: removed {} heartbeat rows and {} non-durable event rows; reclaim {:?}; file {} MB, free {} MB ({}%)",
        report.heartbeat_rows,
        report.non_durable_rows,
        reclaim,
        footprint.file_bytes / 1_000_000,
        footprint.free_bytes / 1_000_000,
        footprint.free_percent(),
    );
}

/// The periodic loop. Spawn on the daemon runtime; exits on `shutdown`.
pub async fn run<F>(shutdown: &tokio::sync::Notify, store: Arc<SqliteEventStore>, is_idle: F)
where
    F: Fn() -> bool,
{
    tokio::select! {
        _ = shutdown.notified() => return,
        _ = tokio::time::sleep(FIRST_PASS_AFTER) => {}
    }
    pass(&store, is_idle()).await;
    let mut ticker = tokio::time::interval(INTERVAL);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ticker.tick().await; // the immediate first tick: the pass above was it
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            _ = ticker.tick() => pass(&store, is_idle()).await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the two gates on the expensive pass. A full vacuum on a
    // busy mesh would hold every send behind a rebuild; skipping reclaim entirely
    // below the threshold would let the freelist ride forever; reclaiming an
    // empty freelist is a transaction for nothing.
    #[test]
    fn a_full_reclaim_needs_both_a_worthwhile_freelist_and_a_quiet_mesh() {
        let heavy = StoreFootprint {
            file_bytes: 2_700_000_000,
            free_bytes: 1_300_000_000,
        };
        let light = StoreFootprint {
            file_bytes: 2_700_000_000,
            free_bytes: 100_000_000,
        };
        let clean = StoreFootprint {
            file_bytes: 2_700_000_000,
            free_bytes: 0,
        };
        assert_eq!(reclaim_for(heavy, true), Some(Reclaim::Full));
        assert_eq!(
            reclaim_for(heavy, false),
            Some(Reclaim::Incremental),
            "busy: never the rebuild"
        );
        assert_eq!(
            reclaim_for(light, true),
            Some(Reclaim::Incremental),
            "not worth the lock"
        );
        assert_eq!(reclaim_for(clean, true), None, "nothing to give back");
    }
}
