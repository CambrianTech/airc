//! Coordinate daemon autostart with the short install/restart window.
//! The OS owns lock lifetime: a crashed updater cannot leave a stale flag.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

pub struct DaemonLifecycleGuard {
    active: File,
    _maintenance_intent: Option<File>,
}

impl DaemonLifecycleGuard {
    /// Hold through status inspection and any resulting daemon spawn.
    pub fn autostart(home: &Path) -> io::Result<Self> {
        // Hold the admission gate only while acquiring the active read lock.
        // A waiting updater closes this gate before draining existing readers,
        // so continuous new CLI calls cannot starve installation.
        let intent = Self::open(home, "daemon-maintenance.lock")?;
        FileExt::try_lock_shared(&intent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon maintenance requested; autostart refused: {error}"),
            )
        })?;
        let file = Self::open(home, "daemon-lifecycle.lock")?;
        FileExt::try_lock_shared(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon maintenance in progress; autostart refused: {error}"),
            )
        })?;
        Ok(Self {
            active: file,
            _maintenance_intent: None,
        })
    }

    /// Acquire only after building; hold through installation and verified restart.
    /// This synchronous updater boundary waits in the OS, without polling. It
    /// stops admitting new readers before draining existing autostart operations.
    /// Process cancellation releases both locks, including a pending acquisition.
    pub fn maintenance(home: &Path) -> io::Result<Self> {
        let intent = Self::open(home, "daemon-maintenance.lock")?;
        FileExt::lock_exclusive(&intent)?;
        let file = Self::open(home, "daemon-lifecycle.lock")?;
        FileExt::lock_exclusive(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon lifecycle busy; maintenance not started: {error}"),
            )
        })?;
        Ok(Self {
            active: file,
            _maintenance_intent: Some(intent),
        })
    }

    fn open(home: &Path, name: &str) -> io::Result<File> {
        let owner = crate::machine_account_home(home);
        std::fs::create_dir_all(&owner)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(owner.join(name))
    }
}

impl Drop for DaemonLifecycleGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.active);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A reconnect must not respawn the old executable inside stop/install;
    // releasing the updater's OS lock must immediately restore autostart.
    #[test]
    fn maintenance_excludes_autostart_and_releases_without_stale_state() {
        let dir = tempfile::tempdir().unwrap();
        let update = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_err());
        drop(update);
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_ok());
    }

    // A busy node drains its existing reader rather than abandoning an update;
    // once maintenance is requested, new clients cannot extend that drain.
    #[test]
    fn maintenance_drains_existing_autostart_without_admitting_new_readers() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let start = DaemonLifecycleGuard::autostart(dir.path()).unwrap();
        let home = dir.path().to_owned();
        let (send, received) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            send.send(DaemonLifecycleGuard::maintenance(&home).unwrap())
                .unwrap();
        });
        // Observe the real admission gate, not a sleep assumed to be long enough.
        let deadline = Instant::now() + Duration::from_secs(5);
        while DaemonLifecycleGuard::autostart(dir.path()).is_ok() {
            assert!(
                Instant::now() < deadline,
                "maintenance never closed admission"
            );
            std::thread::yield_now();
        }
        assert!(matches!(
            received.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        drop(start);
        let update = received.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_err());
        drop(update);
        worker.join().unwrap();
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_ok());
    }
}
