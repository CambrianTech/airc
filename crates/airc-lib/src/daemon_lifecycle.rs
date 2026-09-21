//! Coordinate daemon autostart with the short install/restart window.
//! The OS owns lock lifetime: a crashed updater cannot leave a stale flag.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

pub struct DaemonLifecycleGuard(File);

impl DaemonLifecycleGuard {
    /// Hold through status inspection and any resulting daemon spawn.
    pub fn autostart(home: &Path) -> io::Result<Self> {
        let file = Self::open(home)?;
        FileExt::try_lock_shared(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon maintenance in progress; autostart deferred: {error}"),
            )
        })?;
        Ok(Self(file))
    }

    /// Acquire only after building; hold through installation and verified restart.
    pub fn maintenance(home: &Path) -> io::Result<Self> {
        let file = Self::open(home)?;
        FileExt::try_lock_exclusive(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon lifecycle busy; maintenance not started: {error}"),
            )
        })?;
        Ok(Self(file))
    }

    fn open(home: &Path) -> io::Result<File> {
        let owner = crate::machine_account_home(home);
        std::fs::create_dir_all(&owner)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(owner.join("daemon-lifecycle.lock"))
    }
}

impl Drop for DaemonLifecycleGuard {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
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
        let start = DaemonLifecycleGuard::autostart(dir.path()).unwrap();
        assert!(DaemonLifecycleGuard::maintenance(dir.path()).is_err());
        drop(start);
        let update = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_err());
        assert!(DaemonLifecycleGuard::maintenance(dir.path()).is_err());
        drop(update);
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_ok());
    }
}
