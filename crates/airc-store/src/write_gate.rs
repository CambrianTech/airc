//! One write at a time per store file in this process, in arrival order (Fable on #1653).
//!
//! Every handle on a file (a resident's store, the drain's own connection, the daemon's store and
//! sink) takes the file's gate around each write transaction, through its `admit_write`. Two handles
//! that wrote at once used to meet in SQLite's busy handler, which sleeps in growing steps (1, 2, 5,
//! 10 ms...) rather than until the lock frees. On 2026-11-04, on a 980 MB resident store copy, an
//! append beside the drain had p99 25.7 ms against 5.33 ms with no drain. Behind the gate a writer
//! waits at most one window and wakes on the commit before it. While it waits it holds no
//! connection, so reads on its handle never queue behind it.
//!
//! The gate is a `tokio::sync::Mutex`, which grants in FIFO order: a drain that releases and asks
//! again queues behind the append that was waiting. A write from another process still meets this
//! one in SQLite's busy handler; that is rare (a CLI command beside the daemon).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

/// One file's write gate.
#[derive(Debug, Default)]
pub(crate) struct FileGate {
    lock: Arc<tokio::sync::Mutex<()>>,
    /// Writers queued for the gate now: what a test reads to see a writer waiting HERE, where the
    /// commit before it wakes it, rather than inside SQLite.
    waiting: AtomicUsize,
    /// The slowest foreground write since the drain last read it, by layer: the latency the
    /// drain's pace steers by ([`crate::retention::Pace`]), and where it went. It includes the I/O
    /// a write pays while the drain's checkpoint runs with the gate free, which a gate wait alone
    /// never sees.
    slowest: Mutex<Option<WriteLayers>>,
    /// Bumped as the drain's checkpoint starts and again as it ends (odd while one runs), so a
    /// write can tell whether a checkpoint ran during its life.
    checkpoint_epoch: std::sync::atomic::AtomicU64,
}

impl FileGate {
    /// Wait for this file's turn to write; the guard holds it until the write ends.
    pub(crate) async fn enter(&self) -> tokio::sync::OwnedMutexGuard<()> {
        self.waiting.fetch_add(1, Ordering::AcqRel);
        let guard = Arc::clone(&self.lock).lock_owned().await;
        self.waiting.fetch_sub(1, Ordering::AcqRel);
        guard
    }

    /// The slowest foreground write since the last call, by layer, and reset: what one writer
    /// paid, and where.
    pub(crate) fn take_slowest_write(&self) -> Option<WriteLayers> {
        // A poisoned lock still holds a valid record: no invariant spans its update.
        self.slowest
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .take()
    }

    /// The drain's checkpoint is starting or has ended: each call moves the epoch one step.
    pub(crate) fn checkpoint_edge(&self) {
        self.checkpoint_epoch.fetch_add(1, Ordering::AcqRel);
    }

    fn epoch(&self) -> u64 {
        self.checkpoint_epoch.load(Ordering::Acquire)
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.load(Ordering::Acquire)
    }
}

/// The gate a handle no other handle shares gets: a store opened by URL (memory, a test's own file).
pub(crate) fn unshared() -> Arc<FileGate> {
    Arc::new(FileGate::default())
}

/// The gate of the store file at `path`, shared by every handle on it in this process. Keyed by the
/// path as given, so two spellings of one file get two gates and meet in SQLite's busy handler as
/// before, never worse.
pub(crate) fn for_path(path: &Path) -> Arc<FileGate> {
    // A poisoned lock still holds a valid map: no invariant spans an entry's update.
    let mut gates = GATES.lock().unwrap_or_else(|p| p.into_inner());
    let gates = gates.get_or_insert_with(HashMap::new);
    if let Some(gate) = gates.get(path).and_then(Weak::upgrade) {
        return gate;
    }
    gates.retain(|_, gate| gate.strong_count() > 0);
    let gate = unshared();
    gates.insert(path.to_path_buf(), Arc::downgrade(&gate));
    gate
}

static GATES: Mutex<Option<HashMap<PathBuf, Weak<FileGate>>>> = Mutex::new(None);

/// One admitted write: the file's turn, then this process's custody for it ([`crate::custody`]).
/// Held for the write's whole transaction.
/// One foreground write's time, by layer: queued for the gate, BEGIN plus its statements, and
/// COMMIT; and whether a drain checkpoint ran during it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WriteLayers {
    /// Queued for the file's gate.
    pub wait: std::time::Duration,
    /// After the gate: custody and the version check (`migration::refuse_if_ahead`, a query).
    pub admission: std::time::Duration,
    pub statement: std::time::Duration,
    pub commit: std::time::Duration,
    pub overlapped_checkpoint: bool,
}

impl WriteLayers {
    pub fn total(&self) -> std::time::Duration {
        self.wait + self.admission + self.statement + self.commit
    }
}

pub(crate) struct Write<'a> {
    _turn: tokio::sync::OwnedMutexGuard<()>,
    _custody: crate::custody::WriteTicket<'a>,
    gate: &'a FileGate,
    /// When this write queued for the gate, and the checkpoint epoch then.
    queued: std::time::Instant,
    epoch: u64,
    /// When the gate granted it, and when custody and the version check then admitted it.
    entered: std::time::Instant,
    granted: std::time::Instant,
    /// When its COMMIT began ([`Self::committing`]); `None` for a path that never marks it.
    commit_started: Option<std::time::Instant>,
    /// A write the drain's pace steers by; the drain's own windows are not.
    foreground: bool,
}

impl<'a> Write<'a> {
    pub(crate) fn new(
        gate: &'a FileGate,
        queued: std::time::Instant,
        entered: std::time::Instant,
        epoch: u64,
        foreground: bool,
        turn: tokio::sync::OwnedMutexGuard<()>,
        custody: crate::custody::WriteTicket<'a>,
    ) -> Self {
        Self {
            _turn: turn,
            _custody: custody,
            gate,
            queued,
            entered,
            epoch,
            granted: std::time::Instant::now(),
            commit_started: None,
            foreground,
        }
    }

    /// The write's COMMIT begins now: the layers split statement time from commit time.
    pub(crate) fn committing(&mut self) {
        self.commit_started = Some(std::time::Instant::now());
    }
}

impl FileGate {
    /// The checkpoint epoch a write starting now records.
    pub(crate) fn epoch_now(&self) -> u64 {
        self.epoch()
    }
}

impl Drop for Write<'_> {
    fn drop(&mut self) {
        if !self.foreground {
            return;
        }
        let end = std::time::Instant::now();
        let commit_started = self.commit_started.unwrap_or(end); // unwrap_or: a path that never marks its commit counts it all as statement
        let now = self.gate.epoch();
        let layers = WriteLayers {
            wait: self.entered - self.queued,
            admission: self.granted - self.entered,
            statement: commit_started - self.granted,
            commit: end - commit_started,
            overlapped_checkpoint: self.epoch % 2 == 1 || now != self.epoch,
        };
        let mut slowest = self.gate.slowest.lock().unwrap_or_else(|p| p.into_inner());
        if slowest.is_none_or(|s| layers.total() > s.total()) {
            *slowest = Some(layers);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::EventStore;
    use crate::SqliteEventStore;
    use airc_core::{
        Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
        TranscriptKind,
    };

    fn message(lamport: u64) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::from_u128(0x9a7e),
            peer_id: PeerId::from_u128(0xa1),
            client_id: ClientId::from_u128(0xc1),
            kind: TranscriptKind::Message,
            occurred_at_ms: lamport,
            lamport,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::Json(serde_json::json!({ "text": "hello" }))),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    // what this catches (Fable on #1653: an append beside the drain had p99 25.7 ms against 5.33 ms
    // with no drain, the two handles meeting in SQLite's busy handler): a second handle's write
    // contending inside SQLite. Every handle on one file shares one gate; a write waits HERE while
    // another holds the file, and the first's commit wakes it.
    #[tokio::test]
    async fn a_second_handle_waits_on_the_files_gate_and_the_commit_wakes_it() {
        let dir = tempfile::tempdir().expect("test: tempdir");
        let path = dir.path().join("events.sqlite");
        let first = SqliteEventStore::open_path(&path)
            .await
            .expect("test: first handle");
        let second = SqliteEventStore::open_path(&path)
            .await
            .expect("test: second handle");
        let gate = for_path(&path);
        assert!(Arc::ptr_eq(&gate, &for_path(&path)), "one gate per file");
        assert!(
            !Arc::ptr_eq(&gate, &for_path(&dir.path().join("other.sqlite"))),
            "another file has its own"
        );

        let held = first.admit_write().await.expect("test: the first write");
        let mut append = Box::pin(second.append(message(1)));
        assert!(
            futures::poll!(append.as_mut()).is_pending(),
            "the file is held"
        );
        assert_eq!(
            gate.waiting(),
            1,
            "the second write waits on the gate, not inside SQLite"
        );
        drop(held);
        append.await.expect("test: the commit before it woke it");
        assert_eq!(gate.waiting(), 0);
        assert_eq!(
            first.page_recent(None, 10).await.expect("test: page").len(),
            1
        );
    }
}
