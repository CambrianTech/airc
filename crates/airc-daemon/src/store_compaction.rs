//! Before the daemon serves its event store: the one full rebuild a legacy `auto_vacuum=0` file
//! needs (`airc_store::SqliteEventStore::compact`). It holds every write for the whole rebuild
//! (6.8 s on a 2.9 GB file), so it runs here, in the daemon's startup before it binds, where it
//! serves no one yet; at serving time it would hold every send (Fable on #1653). An incremental
//! file (every store created since then) costs one pragma read.

use std::time::Instant;

use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSeverity, DiagnosticSink,
    StderrJsonDiagnosticSink,
};
use airc_store::SqliteEventStore;

pub async fn before_serving(store: &SqliteEventStore) {
    let event = match store.is_incremental().await {
        Ok(true) => return,
        Ok(false) => compact(store).await,
        Err(error) => DiagnosticEvent::warn(
            DiagnosticComponent::Persistence,
            DiagnosticCode::StoreCompacted,
            "Could not read the event store's vacuum mode before serving; not compacted",
        )
        .with_field("error", error),
    };
    StderrJsonDiagnosticSink.emit(event);
}

async fn compact(store: &SqliteEventStore) -> DiagnosticEvent {
    let before = store.footprint().await;
    let started = Instant::now();
    match store.compact().await {
        Ok(after) => {
            let mut event = DiagnosticEvent::new(
                DiagnosticSeverity::Info,
                DiagnosticComponent::Persistence,
                DiagnosticCode::StoreCompacted,
                "Compacted the legacy event store once, before serving",
            )
            .with_field("held_ms", started.elapsed().as_millis())
            .with_field("file_bytes_after", after.file_bytes);
            if let Ok(before) = before {
                event = event
                    .with_field("file_bytes_before", before.file_bytes)
                    .with_field("free_bytes_before", before.free_bytes);
            }
            event
        }
        Err(error) => DiagnosticEvent::error(
            DiagnosticComponent::Persistence,
            DiagnosticCode::StoreCompacted,
            "Compacting the legacy event store before serving failed",
        )
        .with_field("held_ms", started.elapsed().as_millis())
        .with_field("error", error),
    }
}
