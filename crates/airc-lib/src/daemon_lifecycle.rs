//! Own daemon admission for transient maintenance and durable operator stops.
//! OS locks disappear on a crash; an explicit stop persists until an explicit resume.
use std::fs::{File, OpenOptions};
use std::io;
use std::path::Path;

use fs2::FileExt;

use airc_store::custody::{try_shared, LIFECYCLE_LOCK, MAINTENANCE_LOCK};

pub struct DaemonLifecycleGuard {
    active: File,
    _maintenance_intent: Option<File>,
    owner: crate::MachineAccountHome,
}

/// Another admitted daemon owns initialization. Its endpoint need not be bound yet.
#[derive(Debug, thiserror::Error)]
#[error("daemon initialization is already in progress; no second initializer was admitted")]
pub struct StartupInProgress;

fn acquire_startup(file: &File) -> io::Result<()> {
    FileExt::try_lock_exclusive(file).map_err(|error| {
        if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
            io::Error::new(io::ErrorKind::WouldBlock, StartupInProgress)
        } else {
            error
        }
    })
}

/// A child owns startup until its endpoint is bound, independently of the
/// updater's parent-held maintenance lock. This is never a lifetime daemon lock.
pub struct DaemonStartupGuard {
    active: File,
    owner: crate::MachineAccountHome,
}

/// Proof that this process is the daemon admitted through
/// [`DaemonStartupGuard::acquire`]. Presented once to
/// [`crate::store_admission::declare_daemon`], it makes this process's store
/// writes take no lifecycle lock (an `airc update` excludes the daemon by
/// stopping it), so they never contend with the startup guard.
/// Constructed only by [`DaemonStartupGuard::custody`].
#[derive(Clone, Debug)]
pub struct DaemonCustody {
    _admitted: (),
}

impl DaemonStartupGuard {
    /// This daemon's custody proof, minted while startup is held; it stays
    /// valid after [`Self::bound`] for the process's lifetime.
    pub fn custody(&self) -> DaemonCustody {
        DaemonCustody { _admitted: () }
    }

    pub fn acquire(home: &Path) -> io::Result<Self> {
        let owner = crate::machine_account_home(home);
        let active = DaemonLifecycleGuard::open(&owner, "daemon-startup.lock")?;
        acquire_startup(&active)?;
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
        let intent = Self::open(&owner, MAINTENANCE_LOCK)?;
        try_shared(&intent)?;
        let file = Self::open(&owner, LIFECYCLE_LOCK)?;
        try_shared(&file)?;
        let guard = Self {
            active: file,
            _maintenance_intent: None,
            owner,
        };
        require_running(&guard.owner)?;
        // Observe an admitted child before another caller attempts to spawn.
        // The child takes this same lock exclusively before any store open.
        let startup = Self::open(&guard.owner, "daemon-startup.lock")?;
        acquire_startup(&startup)?;
        FileExt::unlock(&startup)?;
        Ok(guard)
    }

    /// Serialize operator intent or maintenance; updaters acquire after building
    /// and hold through installation and verified restart.
    /// This synchronous updater boundary waits in the OS, without polling. It
    /// stops admitting new readers before draining existing autostart operations.
    /// Process cancellation releases both locks, including a pending acquisition.
    pub fn maintenance(home: &Path) -> io::Result<Self> {
        let owner = crate::machine_account_home(home);
        let intent = Self::open(&owner, MAINTENANCE_LOCK)?;
        FileExt::lock_exclusive(&intent)?;
        let file = Self::open(&owner, LIFECYCLE_LOCK)?;
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
        airc_store::custody::lock_file(owner.as_path(), name)
    }
}

/// Whether an operator stop is recorded for `home`'s machine account. Unserialized: for a
/// reader deciding whether to keep waiting for the daemon (a live feed across an update), where
/// a stop recorded a moment later is seen on its next look. Admission still reads it under
/// the lifecycle gate.
pub fn operator_stop_recorded(home: &Path) -> io::Result<bool> {
    operator_stopped(&crate::machine_account_home(home))
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

    use airc_store::custody::is_maintenance_in_progress;

    // what this catches (Codex on #1618): a CLI auto-start attempted in the update's
    // stop/swap window, from another thread as a hook would from another process, is
    // refused with the typed maintenance error (never a spawn); and an update that FAILS
    // while holding maintenance releases it, so the next auto-start is admitted.
    #[test]
    fn an_update_window_refuses_autostart_typed_and_a_failed_update_releases_it() {
        use std::sync::mpsc;
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_owned();
        let (held, window) = mpsc::channel();
        let (finish, finished) = mpsc::channel::<()>();
        let updater = std::thread::spawn(move || -> Result<(), io::Error> {
            let _custody = DaemonLifecycleGuard::maintenance(&home)?;
            held.send(()).map_err(io::Error::other)?;
            finished.recv().map_err(io::Error::other)?;
            Err(io::Error::other("injected update failure after the stop"))
        });
        window.recv().unwrap();
        let refused = DaemonLifecycleGuard::autostart(dir.path()).err().unwrap();
        assert!(is_maintenance_in_progress(&refused), "{refused}");
        assert!(refused.to_string().contains("update"), "{refused}");
        finish.send(()).unwrap();
        assert!(updater.join().unwrap().is_err(), "the update failed");
        assert!(
            DaemonLifecycleGuard::autostart(dir.path()).is_ok(),
            "custody released on the failure path"
        );
    }

    // A slow initializer keeps ownership after its parent times out. Neither
    // another child nor an autostart caller may launch competing store work.
    #[test]
    fn one_initializer_owns_startup_until_bound_or_dropped() -> io::Result<()> {
        let dir = tempfile::tempdir()?;
        let first = DaemonStartupGuard::acquire(dir.path())?;
        for error in [
            DaemonStartupGuard::acquire(dir.path()).err(),
            DaemonLifecycleGuard::autostart(dir.path()).err(),
        ] {
            assert!(error
                .and_then(|error| error.into_inner())
                .is_some_and(|source| source.is::<StartupInProgress>()));
        }
        first.bound()?;
        let second = DaemonStartupGuard::acquire(dir.path())?;
        drop(second);
        DaemonLifecycleGuard::autostart(dir.path())?;
        Ok(())
    }

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
        loop {
            if DaemonLifecycleGuard::autostart(dir.path())
                .err()
                .is_some_and(|error| is_maintenance_in_progress(&error))
            {
                break;
            }
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
