//! Daemon's shared state — peer identity, registry, owned transports,
//! shutdown notifier.
//!
//! `DaemonState` is constructed once at startup and passed (via Arc)
//! to every per-connection handler. Handlers read fields directly;
//! the substrate enforces its own internal locking (e.g.
//! `PeerKeyRegistry` is wrapped in `Arc<RwLock>`).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::RwLock;
use std::time::Instant;

use tokio::sync::{Mutex, Notify};

use airc_core::PeerId;
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_transport::{LocalFsAdapter, SignedTransport};

/// Everything a daemon needs at runtime. Cheap to clone via Arc; the
/// underlying handles (registry, transports) do their own interior
/// locking.
pub struct DaemonState {
    pub peer_id: PeerId,
    pub keypair: PeerKeypair,
    pub registry: Arc<RwLock<PeerKeyRegistry>>,
    pub policy: VerificationPolicy,
    /// When the daemon started — used for the Status uptime field.
    pub started_at: Instant,
    /// One signed-local-fs transport per wire directory. Lazily
    /// opened on first `Send` referencing the wire so daemons that
    /// never send don't allocate state.
    pub local_fs_transports: Mutex<HashMap<PathBuf, Arc<SignedTransport<LocalFsAdapter>>>>,
    /// Notified when the daemon should stop accepting + exit cleanly.
    pub shutdown: Notify,
}

impl DaemonState {
    pub fn new(
        peer_id: PeerId,
        keypair: PeerKeypair,
        registry: Arc<RwLock<PeerKeyRegistry>>,
        policy: VerificationPolicy,
    ) -> Self {
        Self {
            peer_id,
            keypair,
            registry,
            policy,
            started_at: Instant::now(),
            local_fs_transports: Mutex::new(HashMap::new()),
            shutdown: Notify::new(),
        }
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
