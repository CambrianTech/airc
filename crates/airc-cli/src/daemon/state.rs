//! Daemon's shared state — peer identity, registry, owned transports,
//! shutdown notifier.
//!
//! `DaemonState` is constructed once at startup and passed (via Arc)
//! to every per-connection handler. Handlers read fields directly;
//! the substrate enforces its own internal locking (e.g.
//! `PeerKeyRegistry` is wrapped in `Arc<RwLock>`).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Instant;

use tokio::sync::{Mutex, Notify};

use airc_core::PeerId;
use airc_protocol::{Frame, PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_transport::{LocalFsAdapter, SignedTransport};

/// Default ring-buffer capacity per wire — recent N frames kept in
/// memory for `Inbox` pulls. Tuned to ~chat-cadence retention; can be
/// promoted to a config field later.
pub const DEFAULT_INBOX_CAPACITY: usize = 1024;

/// Bounded ring buffer of recently-received frames for one wire.
/// Subscribers push into the buffer; `Inbox` requests read from it.
#[derive(Debug)]
pub struct InboxBuffer {
    frames: VecDeque<Frame>,
    capacity: usize,
}

impl InboxBuffer {
    pub fn new(capacity: usize) -> Self {
        Self {
            frames: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Append a frame, dropping the oldest if at capacity.
    pub fn push(&mut self, frame: Frame) {
        if self.frames.len() >= self.capacity {
            self.frames.pop_front();
        }
        self.frames.push_back(frame);
    }

    /// Return frames with `lamport > since`, lamport-ascending, up
    /// to `limit`.
    pub fn since(&self, since: Option<u64>, limit: usize) -> Vec<Frame> {
        let cutoff = since.unwrap_or(0);
        self.frames
            .iter()
            .filter(|frame| since.is_none() || frame.envelope.lamport > cutoff)
            .take(limit)
            .cloned()
            .collect()
    }

    /// Largest lamport in the buffer (or 0 if empty).
    pub fn newest_lamport(&self) -> u64 {
        self.frames
            .iter()
            .map(|f| f.envelope.lamport)
            .max()
            .unwrap_or(0)
    }
}

/// Everything a daemon needs at runtime. Cheap to clone via Arc; the
/// underlying handles (registry, transports) do their own interior
/// locking.
pub struct DaemonState {
    pub peer_id: PeerId,
    pub keypair: PeerKeypair,
    pub registry: Arc<RwLock<PeerKeyRegistry>>,
    pub policy: VerificationPolicy,
    /// Home directory the daemon was started against. Lets handlers
    /// reach `<home>/peers.json` etc. without re-deriving the path.
    pub home: PathBuf,
    /// When the daemon started — used for the Status uptime field.
    pub started_at: Instant,
    /// One signed-local-fs transport per wire directory. Lazily
    /// opened on first `Send` referencing the wire so daemons that
    /// never send don't allocate state.
    pub local_fs_transports: Mutex<HashMap<PathBuf, Arc<SignedTransport<LocalFsAdapter>>>>,
    /// One inbox ring buffer per wire — populated by the daemon's
    /// background subscriber task. `Subscribe` requests create the
    /// task on first call (idempotent).
    pub inboxes: Mutex<HashMap<PathBuf, Arc<Mutex<InboxBuffer>>>>,
    /// Notified when the daemon should stop accepting + exit cleanly.
    pub shutdown: Notify,
}

impl DaemonState {
    pub fn new(
        peer_id: PeerId,
        keypair: PeerKeypair,
        registry: Arc<RwLock<PeerKeyRegistry>>,
        policy: VerificationPolicy,
        home: PathBuf,
    ) -> Self {
        Self {
            peer_id,
            keypair,
            registry,
            policy,
            home,
            started_at: Instant::now(),
            local_fs_transports: Mutex::new(HashMap::new()),
            inboxes: Mutex::new(HashMap::new()),
            shutdown: Notify::new(),
        }
    }

    /// Get-or-create the inbox buffer for a wire. The caller is
    /// responsible for ensuring a subscriber task is pushing into it
    /// (`ensure_inbox_subscriber`).
    pub async fn inbox_for(&self, wire: &std::path::Path) -> Arc<Mutex<InboxBuffer>> {
        let key = wire.to_path_buf();
        let mut inboxes = self.inboxes.lock().await;
        if let Some(existing) = inboxes.get(&key) {
            return existing.clone();
        }
        let buffer = Arc::new(Mutex::new(InboxBuffer::new(DEFAULT_INBOX_CAPACITY)));
        inboxes.insert(key, buffer.clone());
        buffer
    }

    /// True if a subscriber task is already running for `wire`.
    /// The handler uses this to keep `Subscribe` idempotent — repeated
    /// calls don't spawn additional drain tasks.
    pub async fn has_inbox(&self, wire: &std::path::Path) -> bool {
        self.inboxes.lock().await.contains_key(wire)
    }

    /// Get-or-create a SignedTransport<LocalFsAdapter> for the given
    /// wire directory. Cached so repeated sends to the same wire
    /// reuse the same adapter (and its subscribers, eventually).
    pub async fn local_fs_for(
        &self,
        wire: &std::path::Path,
    ) -> Arc<SignedTransport<LocalFsAdapter>> {
        let key = wire.to_path_buf();
        let mut transports = self.local_fs_transports.lock().await;
        if let Some(existing) = transports.get(&key) {
            return existing.clone();
        }
        let signed = SignedTransport::new(
            LocalFsAdapter::new(&key),
            self.keypair.clone(),
            self.peer_id,
            self.registry.clone(),
            self.policy,
        );
        let handle = Arc::new(signed);
        transports.insert(key, handle.clone());
        handle
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}
