//! The event store's footprint on the doctor line (card 6781d7e9).
//!
//! `events.sqlite` at 2.65 GB is what stretched the daemon's acquires past 2 s
//! and darkened BigMama for seven hours on 2026-09-26 — and nothing said so. A
//! store that is silently large is the class of failure the doctor exists for:
//! the number belongs on the line the operator already reads, every run, not
//! behind a flag, because by the time anyone thinks to ask it has already cost a
//! node.

use std::path::Path;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Above this the daemon's drain (`airc_daemon::store_retention`) should already
/// have brought the file down; a file still here on a daemon that has run for a
/// day means the drain is not running or not enough.
pub const LARGE_STORE_BYTES: u64 = 512 * 1_000_000;

pub(super) struct StoreFootprintCheck;

#[async_trait::async_trait]
impl Check for StoreFootprintCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::always("store")
    }
    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        vec![check_store(ctx.home)]
    }
}

/// PURE over the two lengths: the finding for a store of `db_bytes` with
/// `wal_bytes` still unflushed.
pub fn store_finding(db_bytes: u64, wal_bytes: u64) -> Finding {
    let detail = format!(
        "events.sqlite {} MB (+ {} MB wal)",
        db_bytes / 1_000_000,
        wal_bytes / 1_000_000
    );
    if db_bytes > LARGE_STORE_BYTES {
        Finding::warn(
            "store",
            format!("{detail} — above {} MB", LARGE_STORE_BYTES / 1_000_000),
            "the daemon drains dead rows every 6 h and vacuums when the mesh is quiet; \
             if this does not fall within a day the drain is not running (see \
             `airc store retention` lines on the daemon's stderr)",
        )
    } else {
        Finding::ok("store", detail)
    }
}

fn check_store(home: &Path) -> Finding {
    let db = airc_lib::machine_account_home(home).join("events.sqlite");
    let len = |p: &Path| std::fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    let db_bytes = len(&db);
    if db_bytes == 0 {
        return Finding::info("store", "no events.sqlite yet (nothing has been written)");
    }
    store_finding(db_bytes, len(&db.with_extension("sqlite-wal")))
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: the threshold is the line between "a number on the
    // report" and "a warning with the fix" — a 2.65 GB store must warn, and the
    // WAL must be shown separately so a checkpoint backlog is not read as growth.
    #[test]
    fn a_large_store_warns_and_a_small_one_is_just_the_number() {
        let large = store_finding(2_650_000_000, 12_000_000);
        assert!(large.detail.contains("2650 MB") && large.detail.contains("12 MB wal"));
        assert!(large.fix.is_some(), "a large store carries the fix");
        let small = store_finding(40_000_000, 0);
        assert!(small.fix.is_none(), "a small store is just the number");
    }
}
