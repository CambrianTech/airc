//! The one admission for opening the machine store (`events.sqlite`) for
//! writing. A writer opens it here and nowhere else:
//!
//! - a CLI command holds [`DaemonLifecycleGuard::writer`] custody inside the
//!   returned store for as long as the store lives, so an `airc update` cannot
//!   start between the open and the last write (maintenance waits for it), and
//!   while maintenance is held the open is refused with
//!   [`crate::daemon_lifecycle::MaintenanceInProgress`];
//! - the daemon presents its [`DaemonCustody`] proof instead, never
//!   re-acquiring the lifecycle locks it was admitted under.

use std::path::Path;

use airc_store::{SqliteEventStore, StoreError};

use crate::daemon_lifecycle::{DaemonCustody, DaemonLifecycleGuard};

/// Why a writable open was not made.
#[derive(Debug, thiserror::Error)]
pub enum StoreAdmissionError {
    /// Maintenance holds the lifecycle (typed inside), or its lock files failed.
    #[error("{0}")]
    Custody(#[from] std::io::Error),
    #[error("{0}")]
    Store(#[from] StoreError),
}

/// Open `db_path` for writing from a process that is not the daemon, under
/// `home`'s machine-account lifecycle custody, held by the returned store.
pub async fn open_writer_store(
    home: &Path,
    db_path: &Path,
) -> Result<SqliteEventStore, StoreAdmissionError> {
    let custody = DaemonLifecycleGuard::writer(home)?;
    Ok(SqliteEventStore::open_path_in_custody(db_path, Box::new(custody)).await?)
}

/// Open `db_path` for writing from the daemon itself, under its admission proof.
pub async fn open_daemon_store(
    db_path: &Path,
    custody: &DaemonCustody,
) -> Result<SqliteEventStore, StoreError> {
    SqliteEventStore::open_path_in_custody(db_path, Box::new(custody.clone())).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_lifecycle::{is_maintenance_in_progress, DaemonStartupGuard};

    // what this catches (Codex on #1618): a CLI writer cannot open the machine
    // store while an update holds maintenance; the refusal is the typed one.
    #[tokio::test]
    async fn a_writer_open_during_maintenance_is_refused_typed() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("events.sqlite");
        let update = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        let refused = open_writer_store(dir.path(), &db).await.err().unwrap();
        assert!(is_maintenance_in_progress(&refused), "{refused}");
        drop(update);
        assert!(open_writer_store(dir.path(), &db).await.is_ok());
    }

    // what this catches (Codex on #1618): custody lives as long as the store,
    // not just the open. While a writer holds the store, maintenance closes
    // admission but cannot start; when the store drops, it starts.
    #[tokio::test]
    async fn a_writer_holding_the_store_holds_maintenance_off_until_it_drops() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let store = open_writer_store(dir.path(), &dir.path().join("events.sqlite"))
            .await
            .unwrap();
        let home = dir.path().to_owned();
        let (send, started) = mpsc::channel();
        let updater = std::thread::spawn(move || {
            send.send(DaemonLifecycleGuard::maintenance(&home).map(|_| ()))
                .unwrap();
        });
        let deadline = Instant::now() + Duration::from_secs(5);
        while DaemonLifecycleGuard::autostart(dir.path()).is_ok() {
            assert!(Instant::now() < deadline, "maintenance never asked");
            std::thread::yield_now();
        }
        assert!(
            matches!(started.try_recv(), Err(mpsc::TryRecvError::Empty)),
            "maintenance must wait for the open store"
        );
        drop(store);
        started
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        updater.join().unwrap();
    }

    // what this catches (Codex on #1618): the daemon's own opens take no
    // lifecycle lock, so they cannot deadlock against the startup guard it
    // holds, and an update's maintenance (taken before it stops the daemon)
    // is not blocked by the daemon's open store.
    #[tokio::test]
    async fn the_daemon_store_opens_under_its_proof_without_lifecycle_locks() {
        let dir = tempfile::tempdir().unwrap();
        let db = dir.path().join("events.sqlite");
        let startup = DaemonStartupGuard::acquire(dir.path()).unwrap();
        let custody = startup.custody();
        let first = open_daemon_store(&db, &custody).await.unwrap();
        let second = open_daemon_store(&db, &custody).await.unwrap();
        startup.bound().unwrap();
        let update = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        drop((first, second, update));
    }
}
