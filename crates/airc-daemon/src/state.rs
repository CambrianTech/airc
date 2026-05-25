//! Daemon's shared state — peer identity, registry, owned transports,
//! event store, shutdown notifier.
//!
//! `DaemonState` is constructed once at startup and passed (via Arc)
//! to every per-connection handler. Handlers read fields directly;
//! the substrate enforces its own internal locking (e.g.
//! `PeerKeyRegistry` owns its own concurrent map).
//!
//! Slice 5b: the per-wire `InboxBuffer` ring is gone. Subscribers now
//! convert each received `Frame` into a `TranscriptEvent` and append
//! to the shared [`EventStore`]; `Inbox` requests query the store via
//! cursor + channel filter. Closes grievance §7 (stronger cursor
//! semantics + no cross-room leakage).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::{broadcast, Mutex, Notify};

use airc_core::PeerId;
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::EventStore;
use airc_transport::{LocalFsAdapter, SignedTransport};

use crate::trust_refresh;

const TRUST_REFRESH_INTERVAL: Duration = Duration::from_millis(750);

/// Everything a daemon needs at runtime. Cheap to clone via Arc; the
/// underlying handles (registry, transports, store) do their own
/// interior locking.
pub struct DaemonState {
    pub peer_id: PeerId,
    pub keypair: PeerKeypair,
    pub registry: Arc<PeerKeyRegistry>,
    pub policy: VerificationPolicy,
    /// Home directory the daemon was started against. Lets handlers
    /// reach the store and IPC state without re-deriving the path.
    pub home: PathBuf,
    /// When the daemon started — used for the Status uptime field.
    pub started_at: Instant,
    /// One signed-local-fs transport per wire directory. Lazily
    /// opened on first `Send` referencing the wire so daemons that
    /// never send don't allocate state.
    pub local_fs_transports: Mutex<HashMap<PathBuf, Arc<SignedTransport<LocalFsAdapter>>>>,
    /// Durable event store backing `Inbox` queries. Subscribers
    /// convert received frames to `TranscriptEvent` and append here;
    /// handlers query via `resume_from(cursor)` with channel filter.
    pub event_store: Arc<dyn EventStore>,
    /// Tracks which wires have an active subscriber task. `Subscribe`
    /// is idempotent — repeated calls find the wire here and skip
    /// the spawn.
    pub subscribed_wires: Mutex<HashMap<PathBuf, ()>>,
    /// Trust roots already watched by this daemon. Each root is an
    /// AIRC home/store whose peer trust rows should feed the live
    /// verifier. This is separate from `subscribed_wires`: many wires
    /// can share one root.
    pub trusted_roots: Mutex<HashMap<PathBuf, ()>>,
    /// Authoritative live fan-out for daemon consumers.
    pub live_tx: broadcast::Sender<airc_core::TranscriptEvent>,
    /// Notified when the daemon should stop accepting + exit cleanly.
    pub shutdown: Notify,
    /// Runtime metadata reported through IPC status so clients can
    /// replace stale daemons after updates without guessing from
    /// process lists or socket names.
    pub runtime: DaemonRuntimeInfo,
}

impl DaemonState {
    /// Construct a state with the given event store. The store is the
    /// durable backing for transcript reads; callers that don't need
    /// persistence (in-process tests) can pass an
    /// `airc_store::InMemoryEventStore`.
    pub fn new(
        peer_id: PeerId,
        keypair: PeerKeypair,
        registry: Arc<PeerKeyRegistry>,
        policy: VerificationPolicy,
        home: PathBuf,
        event_store: Arc<dyn EventStore>,
    ) -> Self {
        Self::new_with_runtime(
            peer_id,
            keypair,
            registry,
            policy,
            home,
            event_store,
            DaemonRuntimeInfo::unknown(),
        )
    }

    pub fn new_with_runtime(
        peer_id: PeerId,
        keypair: PeerKeypair,
        registry: Arc<PeerKeyRegistry>,
        policy: VerificationPolicy,
        home: PathBuf,
        event_store: Arc<dyn EventStore>,
        runtime: DaemonRuntimeInfo,
    ) -> Self {
        let (live_tx, _) = broadcast::channel(1024);
        Self {
            peer_id,
            keypair,
            registry,
            policy,
            home,
            started_at: Instant::now(),
            local_fs_transports: Mutex::new(HashMap::new()),
            event_store,
            subscribed_wires: Mutex::new(HashMap::new()),
            trusted_roots: Mutex::new(HashMap::new()),
            live_tx,
            shutdown: Notify::new(),
            runtime,
        }
    }

    /// True if a subscriber task is already running for `wire`. The
    /// `Subscribe` handler uses this to keep the call idempotent —
    /// repeated subscribes don't spawn duplicate drain tasks.
    pub async fn has_subscriber(&self, wire: &std::path::Path) -> bool {
        self.subscribed_wires.lock().await.contains_key(wire)
    }

    /// Mark a wire as having an active subscriber task. Returns true
    /// if this call was the first to claim the wire, false if a task
    /// was already registered.
    pub async fn register_subscriber(&self, wire: &std::path::Path) -> bool {
        let mut subs = self.subscribed_wires.lock().await;
        if subs.contains_key(wire) {
            return false;
        }
        subs.insert(wire.to_path_buf(), ());
        true
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

    /// Start live verifier synchronization for `root` if this daemon
    /// is not already watching it. Returns true when a new watcher was
    /// registered. The caller should refresh once before subscribing;
    /// the watcher keeps later sibling-scope trust additions visible.
    pub async fn register_trust_root(&self, root: &Path) -> bool {
        let mut roots = self.trusted_roots.lock().await;
        if roots.contains_key(root) {
            return false;
        }
        roots.insert(root.to_path_buf(), ());
        true
    }

    pub async fn refresh_trust_root(
        &self,
        root: &Path,
    ) -> Result<usize, trust_refresh::TrustRefreshError> {
        trust_refresh::refresh_root(self.registry.clone(), root).await
    }

    pub fn spawn_trust_refresher(self: &Arc<Self>, root: PathBuf) {
        let state = self.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = state.shutdown.notified() => break,
                    _ = tokio::time::sleep(TRUST_REFRESH_INTERVAL) => {
                        if let Err(error) = state.refresh_trust_root(&root).await {
                            StderrJsonDiagnosticSink.emit(
                                DiagnosticEvent::warn(
                                    DiagnosticComponent::Daemon,
                                    DiagnosticCode::TrustRefreshFailed,
                                    "daemon trust refresh failed",
                                )
                                .with_field("root", root.display())
                                .with_field("error", error),
                            );
                        }
                    }
                }
            }
        });
    }

    pub fn uptime_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs()
    }
}

#[derive(Debug, Clone, Default)]
pub struct DaemonRuntimeInfo {
    pub ipc_protocol_version: Option<u32>,
    pub build_commit: Option<String>,
    pub build_branch: Option<String>,
    pub executable: Option<String>,
}

impl DaemonRuntimeInfo {
    pub fn unknown() -> Self {
        Self::default()
    }
}
