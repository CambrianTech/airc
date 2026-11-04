//! Before the daemon serves its event store: the one full rebuild a legacy `auto_vacuum=0` file
//! needs (`airc_store::SqliteEventStore::compact`). It holds every write for the whole rebuild
//! (6.8 s on a 2.9 GB file), so it runs here, in the daemon's startup before it binds, where it
//! serves no one yet; at serving time it would hold every send (Fable on #1653). An incremental
//! file (every store created since then) costs one pragma read.

use std::time::Instant;

use airc_store::SqliteEventStore;

pub async fn before_serving(store: &SqliteEventStore) {
    match store.is_incremental().await {
        Ok(true) => return,
        Ok(false) => {}
        Err(error) => {
            eprintln!("airc store: could not read the file's vacuum mode before serving: {error}");
            return;
        }
    }
    let started = Instant::now();
    match store.compact().await {
        Ok(after) => eprintln!(
            "airc store: compacted the legacy file before serving: {} MB (free {} MB) in {} ms",
            after.file_bytes / 1_000_000,
            after.free_bytes / 1_000_000,
            started.elapsed().as_millis(),
        ),
        Err(error) => eprintln!("airc store: compaction before serving failed: {error}"),
    }
}
