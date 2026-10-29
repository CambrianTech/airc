//! Write custody: the one owner of "may this process write the machine's
//! stores right now". `airc update` and `airc stop` take daemon maintenance
//! (exclusive locks on [`MAINTENANCE_LOCK`] and [`LIFECYCLE_LOCK`] in the
//! machine-account home); every store write takes the same locks SHARED for
//! the length of that one write and releases them when it returns. So:
//!
//! - maintenance waits only for writes already in flight, never for a
//!   long-lived process that merely holds a store open (an `airc join` feed);
//! - a write attempted while maintenance is held is refused with the typed
//!   [`MaintenanceInProgress`], never queued behind it;
//! - the daemon's writes take no lock ([`Admission::Daemon`]): maintenance
//!   stops the daemon before it proceeds, and its startup guard is never
//!   re-acquired.
//!
//! Which admission a store gets is decided by the process (airc-lib installs
//! [`install_resolver`] once: daemon or CLI writer, and the machine-account
//! home of a store path); [`crate::SqliteEventStore::open_path`] and
//! [`crate::SqliteDurableSink::open_path`] resolve it at open. A file store in
//! a process that installed no resolver is refused ([`NotAdmitted`]).

use std::fs::{File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use fs2::FileExt;

/// The admission gate: held exclusively by maintenance from the moment it is
/// requested, so new writers and autostarts are refused while it drains.
pub const MAINTENANCE_LOCK: &str = "daemon-maintenance.lock";
/// The lifecycle lock: shared by autostarts and in-flight writes, exclusive
/// for maintenance.
pub const LIFECYCLE_LOCK: &str = "daemon-lifecycle.lock";

/// A write or an automatic daemon start refused because maintenance (an
/// `airc update`, or an operator stop or resume) holds the lifecycle. Carried
/// inside an `io::Error`; [`is_maintenance_in_progress`] recognizes it.
#[derive(Debug)]
pub struct MaintenanceInProgress {
    lock: io::Error,
}

impl MaintenanceInProgress {
    pub fn refusal(lock: io::Error) -> io::Error {
        io::Error::new(lock.kind(), Self { lock })
    }
}

impl std::fmt::Display for MaintenanceInProgress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "an airc update (or operator stop/resume) is in progress; writes and automatic \
             daemon start are refused until it finishes, then retry ({})",
            self.lock
        )
    }
}

impl std::error::Error for MaintenanceInProgress {}

/// Whether `error` (anywhere in its source chain) is a [`MaintenanceInProgress`] refusal.
pub fn is_maintenance_in_progress(error: &(dyn std::error::Error + 'static)) -> bool {
    let mut next = Some(error);
    while let Some(error) = next {
        if error.is::<MaintenanceInProgress>()
            || error
                .downcast_ref::<io::Error>()
                .and_then(|io| io.get_ref())
                .is_some_and(|inner| inner.is::<MaintenanceInProgress>())
        {
            return true;
        }
        next = error.source();
    }
    false
}

/// Open (creating) one of the lifecycle lock files in `owner`.
pub fn lock_file(owner: &Path, name: &str) -> io::Result<File> {
    std::fs::create_dir_all(owner)?;
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(owner.join(name))
}

/// Take `file` shared without waiting; a held exclusive lock (maintenance) is
/// the typed refusal.
pub fn try_shared(file: &File) -> io::Result<()> {
    FileExt::try_lock_shared(file).map_err(MaintenanceInProgress::refusal)
}

/// How a store's writes are admitted.
#[derive(Debug, Clone)]
pub enum Admission {
    /// A non-daemon process: each write takes the lifecycle of the machine
    /// account at `owner` shared.
    Writer { owner: PathBuf },
    /// The daemon itself (admitted by its startup guard): writes take no lock.
    Daemon,
    /// A store outside any machine account (in memory): writes take no lock.
    Unmanaged,
}

/// Decides the admission of a store at a path; installed once per process.
pub type Resolver = fn(&Path) -> Admission;

static RESOLVER: OnceLock<Resolver> = OnceLock::new();

/// Install this process's admission resolver. The first install wins; later
/// calls are no-ops (one process, one rule).
pub fn install_resolver(resolver: Resolver) {
    let _ = RESOLVER.set(resolver);
}

/// A file store opened in a process that never declared how its writes are
/// admitted.
#[derive(Debug, thiserror::Error)]
#[error("store at {0} opened without an installed write admission (airc_lib::store_admission)")]
pub struct NotAdmitted(pub PathBuf);

/// The admission of a file store at `path`.
pub(crate) fn resolve(path: &Path) -> Result<Admission, NotAdmitted> {
    match RESOLVER.get() {
        Some(resolver) => Ok(resolver(path)),
        #[cfg(any(test, feature = "test-fixtures"))]
        None => Ok(Admission::Unmanaged),
        #[cfg(not(any(test, feature = "test-fixtures")))]
        None => Err(NotAdmitted(path.to_owned())),
    }
}

/// A store's write custody: its admission, and for a writer the lock files
/// (opened once) and how many writes of this store are in flight.
pub struct WriteGate {
    locks: Option<WriterLocks>,
}

struct WriterLocks {
    gate: File,
    lifecycle: File,
    /// Writes in flight through `lifecycle`; it is locked from 0 to 1 and
    /// unlocked from 1 to 0 (one open file description holds one lock).
    in_flight: Mutex<usize>,
}

impl WriteGate {
    pub fn new(admission: Admission) -> io::Result<Self> {
        let locks = match admission {
            Admission::Writer { owner } => Some(WriterLocks {
                gate: lock_file(&owner, MAINTENANCE_LOCK)?,
                lifecycle: lock_file(&owner, LIFECYCLE_LOCK)?,
                in_flight: Mutex::new(0),
            }),
            Admission::Daemon | Admission::Unmanaged => None,
        };
        Ok(Self { locks })
    }

    /// Admit one write, held until the returned ticket drops (the write's end).
    pub fn admit(&self) -> io::Result<WriteTicket<'_>> {
        let Some(locks) = &self.locks else {
            return Ok(WriteTicket { locks: None });
        };
        let mut in_flight = locks.in_flight.lock().unwrap_or_else(|p| p.into_inner());
        if *in_flight == 0 {
            // The gate only while taking the lifecycle, as autostart does: a
            // requested maintenance closes it before draining in-flight writes.
            try_shared(&locks.gate)?;
            let taken = try_shared(&locks.lifecycle);
            FileExt::unlock(&locks.gate)?;
            taken?;
        }
        *in_flight += 1;
        Ok(WriteTicket { locks: Some(locks) })
    }
}

/// One admitted write; releases its share of the lifecycle when dropped.
pub struct WriteTicket<'a> {
    locks: Option<&'a WriterLocks>,
}

impl Drop for WriteTicket<'_> {
    fn drop(&mut self) {
        let Some(locks) = self.locks else {
            return;
        };
        let mut in_flight = locks.in_flight.lock().unwrap_or_else(|p| p.into_inner());
        *in_flight = in_flight.saturating_sub(1);
        if *in_flight == 0 {
            // Explicit unlock: closing alone would wait for every duplicate.
            let _ = FileExt::unlock(&locks.lifecycle);
        }
    }
}
