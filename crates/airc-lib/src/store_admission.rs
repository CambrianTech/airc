//! How this process's store writes are admitted ([`airc_store::custody`]),
//! decided once: every store open in the process (`SqliteEventStore::open_path`,
//! `SqliteDurableSink::open_path`, and so every crate that opens one: trust,
//! identity, `Airc::open`, the CLI commands, the daemon) resolves through the
//! resolver installed here.
//!
//! - A CLI process: each write takes its machine account's lifecycle lock
//!   shared for that one write ([`airc_store::custody::Admission::Writer`]),
//!   refused typed while an update or stop holds maintenance, which in turn
//!   waits only for writes in flight.
//! - The daemon, once it presents its [`DaemonCustody`] proof
//!   ([`declare_daemon`]): its writes take no lifecycle lock, so they never
//!   contend with the startup guard it holds; maintenance stops it instead.

use std::path::Path;
use std::sync::OnceLock;

use airc_store::custody::Admission;

use crate::daemon_lifecycle::DaemonCustody;

static DAEMON: OnceLock<DaemonCustody> = OnceLock::new();

/// Install this process's admission resolver (idempotent). Called by the CLI
/// at startup and by every airc-lib entry that opens a store.
pub fn install() {
    airc_store::custody::install_resolver(resolve);
}

/// Declare this process the daemon, proven by its startup guard: from here on
/// its store writes take no lifecycle lock.
pub fn declare_daemon(proof: &DaemonCustody) {
    let _ = DAEMON.set(proof.clone());
    install();
}

fn resolve(db_path: &Path) -> Admission {
    if DAEMON.get().is_some() {
        return Admission::Daemon;
    }
    // A store file always has a parent directory; only `/` has none, and it
    // is not a store.
    let home = db_path.parent().unwrap_or(db_path);
    Admission::Writer {
        owner: crate::machine_account_home(home).into_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::daemon_lifecycle::{DaemonLifecycleGuard, DaemonStartupGuard};
    use airc_store::custody::is_maintenance_in_progress;
    use airc_store::{EventStore, SqliteEventStore};

    async fn write(store: &SqliteEventStore, key: &str) -> Result<(), airc_store::StoreError> {
        store
            .set_scoped_state(airc_store::StoredScopedState {
                scope_key: "test".into(),
                key: key.into(),
                value_json: "1".into(),
                version: 1,
                updated_at_ms: 0,
                updated_by: None,
            })
            .await
    }

    // what this catches (Codex on #1618): a long-lived process that merely holds
    // a store open (an `airc join` feed) never blocks an update: maintenance
    // starts while the store stays open, a write during maintenance is refused
    // with the typed error, and writes resume when maintenance ends.
    #[tokio::test]
    async fn an_open_store_does_not_block_maintenance_and_writes_are_refused_during_it() {
        install();
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteEventStore::open_path(&dir.path().join("events.sqlite"))
            .await
            .unwrap();
        write(&store, "before").await.unwrap();
        let update = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        let refused = write(&store, "during").await.err().unwrap();
        assert!(is_maintenance_in_progress(&refused), "{refused}");
        drop(update);
        write(&store, "after").await.unwrap();
    }

    // what this catches (Codex on #1618): a write in flight delays maintenance
    // until it ends; maintenance closes admission meanwhile.
    #[test]
    fn a_write_in_flight_delays_maintenance_until_it_ends() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let custody = airc_store::custody::Admission::Writer {
            owner: crate::machine_account_home(dir.path()).into_path_buf(),
        };
        let custody = airc_store::custody::WriteGate::new(custody).unwrap();
        let in_flight = custody.admit().unwrap();
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
            "maintenance waits for the write in flight"
        );
        drop(in_flight);
        started
            .recv_timeout(Duration::from_secs(5))
            .unwrap()
            .unwrap();
        updater.join().unwrap();
    }

    // what this catches (Codex on #1618): the daemon's writes take no lifecycle
    // lock, so they cannot deadlock against its startup guard, and an update's
    // maintenance (taken before it stops the daemon) is not blocked by them.
    #[test]
    fn the_daemon_writes_without_lifecycle_locks() {
        let dir = tempfile::tempdir().unwrap();
        let startup = DaemonStartupGuard::acquire(dir.path()).unwrap();
        let gate = airc_store::custody::WriteGate::new(Admission::Daemon).unwrap();
        let in_flight = gate.admit().unwrap();
        startup.bound().unwrap();
        let update = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        drop((in_flight, update));
    }
}
