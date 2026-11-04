//! `airc store-compact`: the one full rebuild of a legacy `auto_vacuum=0` store file, run in a
//! lifecycle stop window (`airc_store::SqliteEventStore::compact`). The daemon compacts its own
//! file before it serves; this verb is for a store a process opens itself (a continuum resident's),
//! called while that process is down.

use std::path::Path;
use std::time::Instant;

pub async fn compact(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let store = airc_store::SqliteEventStore::open_path(path).await?;
    let before = store.footprint().await?;
    let started = Instant::now();
    let after = store.compact().await?;
    println!(
        "compacted {}: file {} MB (free {} MB) -> {} MB (free {} MB) in {} ms",
        path.display(),
        before.file_bytes / 1_000_000,
        before.free_bytes / 1_000_000,
        after.file_bytes / 1_000_000,
        after.free_bytes / 1_000_000,
        started.elapsed().as_millis(),
    );
    Ok(())
}
