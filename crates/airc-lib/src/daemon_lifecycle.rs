//! Own daemon admission for transient maintenance and durable operator stops.
//! OS locks disappear on a crash; an explicit stop persists until an explicit resume.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

pub struct DaemonLifecycleGuard {
    active: File,
    _maintenance_intent: Option<File>,
    owner: crate::MachineAccountHome,
}

/// A child owns startup until its endpoint is bound, independently of the
/// updater's parent-held maintenance lock. This is never a lifetime daemon lock.
pub struct DaemonStartupGuard {
    active: File,
    owner: crate::MachineAccountHome,
}

impl DaemonStartupGuard {
    pub fn acquire(home: &Path) -> io::Result<Self> {
        let owner = crate::machine_account_home(home);
        let active = DaemonLifecycleGuard::open(&owner, "daemon-startup.lock")?;
        FileExt::try_lock_shared(&active)?;
        require_running(&owner)?;
        Ok(Self { active, owner })
    }

    /// Call only after the endpoint is bound, before serving. Release before
    /// checking intent: a concurrent stop either observes that bound endpoint
    /// or sees startup still held and refuses to report a completed shutdown.
    pub fn bound(self) -> io::Result<()> {
        FileExt::unlock(&self.active)?;
        require_running(&self.owner)
    }
}

impl Drop for DaemonStartupGuard {
    fn drop(&mut self) {
        // Explicit unlock also releases a Unix flock briefly inherited by a
        // concurrent fork; merely closing this descriptor waits for its exec.
        let _ = FileExt::unlock(&self.active);
    }
}

impl DaemonLifecycleGuard {
    /// Hold through status inspection and any resulting daemon spawn.
    pub fn autostart(home: &Path) -> io::Result<Self> {
        let owner = crate::machine_account_home(home);
        // Hold the admission gate only while acquiring the active read lock.
        // A waiting updater closes this gate before draining existing readers,
        // so continuous new CLI calls cannot starve installation.
        let intent = Self::open(&owner, "daemon-maintenance.lock")?;
        FileExt::try_lock_shared(&intent).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon maintenance requested; autostart refused: {error}"),
            )
        })?;
        let file = Self::open(&owner, "daemon-lifecycle.lock")?;
        FileExt::try_lock_shared(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon maintenance in progress; autostart refused: {error}"),
            )
        })?;
        let guard = Self {
            active: file,
            _maintenance_intent: None,
            owner,
        };
        require_running(&guard.owner)?;
        Ok(guard)
    }

    /// Serialize operator intent or maintenance; updaters acquire after building
    /// and hold through installation and verified restart.
    /// This synchronous updater boundary waits in the OS, without polling. It
    /// stops admitting new readers before draining existing autostart operations.
    /// Process cancellation releases both locks, including a pending acquisition.
    pub fn maintenance(home: &Path) -> io::Result<Self> {
        let owner = crate::machine_account_home(home);
        let intent = Self::open(&owner, "daemon-maintenance.lock")?;
        FileExt::lock_exclusive(&intent)?;
        let file = Self::open(&owner, "daemon-lifecycle.lock")?;
        FileExt::lock_exclusive(&file).map_err(|error| {
            io::Error::new(
                error.kind(),
                format!("daemon lifecycle busy; maintenance not started: {error}"),
            )
        })?;
        Ok(Self {
            active: file,
            _maintenance_intent: Some(intent),
            owner,
        })
    }

    /// Read while admission is serialized. Unreadable state never means permission
    /// to start; a stop has no expiry and survives the operator command exiting.
    pub fn operator_stopped(&self) -> io::Result<bool> {
        operator_stopped(&self.owner)
    }

    /// Persist intent before requesting shutdown, under the same exclusive gate
    /// that drains existing autostarts. An interrupted stop therefore stays stopped.
    pub fn record_operator_stop(&self) -> io::Result<()> {
        self.require_exclusive()?;
        let path = self.owner.join("daemon-operator-stop");
        match OpenOptions::new().create_new(true).write(true).open(path) {
            Ok(file) => file.sync_all()?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        self.sync_owner_directory()
    }

    /// After recording stop intent and stopping the visible endpoint, prove no
    /// admitted child is still pre-bind. A slow/hung child is an error, never
    /// absence; it will recheck the retained intent when it reaches bind.
    pub fn confirm_no_startup(&self) -> io::Result<()> {
        self.require_exclusive()?;
        if !self.operator_stopped()? {
            return Err(io::Error::other(
                "stop intent is required before confirming shutdown",
            ));
        }
        let startup = Self::open(&self.owner, "daemon-startup.lock")?;
        FileExt::try_lock_exclusive(&startup).map_err(|error| {
            io::Error::new(error.kind(), format!(
                "operator stop recorded, but daemon startup is still active; shutdown not confirmed: {error}"
            ))
        })?;
        FileExt::unlock(&startup)
    }

    /// Only an explicit resume/adoption may clear intent; maintenance alone never does.
    pub fn clear_operator_stop(&self) -> io::Result<()> {
        self.require_exclusive()?;
        match std::fs::remove_file(self.owner.join("daemon-operator-stop")) {
            Ok(()) => self.sync_owner_directory(),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn require_exclusive(&self) -> io::Result<()> {
        if self._maintenance_intent.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "operator intent requires exclusive daemon lifecycle ownership",
            ));
        }
        Ok(())
    }

    fn sync_owner_directory(&self) -> io::Result<()> {
        #[cfg(unix)]
        File::open(&self.owner)?.sync_all()?;
        Ok(())
    }

    fn open(owner: &crate::MachineAccountHome, name: &str) -> io::Result<File> {
        std::fs::create_dir_all(owner)?;
        OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(owner.join(name))
    }
}

fn operator_stopped(owner: &crate::MachineAccountHome) -> io::Result<bool> {
    match std::fs::symlink_metadata(owner.join("daemon-operator-stop")) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn require_running(owner: &crate::MachineAccountHome) -> io::Result<()> {
    if operator_stopped(owner)? {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "daemon intentionally stopped; automatic startup refused; run `airc join` to resume",
        ));
    }
    Ok(())
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
        // The updater owns maintenance while its replacement reaches bind.
        DaemonStartupGuard::acquire(dir.path())
            .unwrap()
            .bound()
            .unwrap();
        drop(update);
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_ok());

        // Deliberate intent has the opposite lifetime from the OS maintenance
        // lock: dropping either the command or a later updater must preserve it.
        let stopped = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        stopped.record_operator_stop().unwrap();
        drop(stopped);
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_err());
        assert!(DaemonStartupGuard::acquire(dir.path()).is_err());
        let restarted_owner = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        assert!(restarted_owner.operator_stopped().unwrap());
        drop(restarted_owner);
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_err());
        let resumed = DaemonLifecycleGuard::maintenance(dir.path()).unwrap();
        resumed.clear_operator_stop().unwrap();
        drop(resumed);
        let automatic = DaemonLifecycleGuard::autostart(dir.path()).unwrap();
        assert!(!automatic.operator_stopped().unwrap());
        assert!(automatic.record_operator_stop().is_err());
        assert!(automatic.clear_operator_stop().is_err());
    }

    // A busy node drains its existing reader rather than abandoning an update;
    // once maintenance is requested, new clients cannot extend that drain.
    #[test]
    fn maintenance_drains_existing_autostart_without_admitting_new_readers() {
        use std::sync::mpsc;
        use std::time::{Duration, Instant};
        let dir = tempfile::tempdir().unwrap();
        let start = DaemonLifecycleGuard::autostart(dir.path()).unwrap();
        let child = DaemonStartupGuard::acquire(dir.path()).unwrap();
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
        // Losing the parent admission (timeout/cancellation) cannot make a
        // still-starting child count as absent. Stop is bounded and retains its
        // intent; the child rejects that intent at the actual bind callback.
        update.record_operator_stop().unwrap();
        assert!(update.confirm_no_startup().is_err());
        assert!(child.bound().is_err());
        update.confirm_no_startup().unwrap();
        assert!(DaemonStartupGuard::acquire(dir.path()).is_err());
        update.clear_operator_stop().unwrap();
        drop(update);
        worker.join().unwrap();
        assert!(DaemonLifecycleGuard::autostart(dir.path()).is_ok());
    }
}
