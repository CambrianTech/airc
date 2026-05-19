//! Shared adapter state — held inside `Arc<Inner>` and reachable from
//! every spawned task (accept loop, per-connection read/write loops,
//! subscriber dispatch).
//!
//! Centralises constants + the SubscriberHandle / OutboundTx type
//! aliases so siblings (`connection.rs`, `dispatch.rs`, parent
//! `mod.rs`) can reference one canonical source.

use std::collections::HashMap;
use std::sync::atomic::AtomicU64;
use std::sync::Arc;
use std::sync::RwLock;

use tokio::sync::{mpsc, Mutex};

use airc_core::PeerId;
use airc_protocol::{Frame, PeerKeyRegistry, PeerKeypair, Subscription};

use crate::lan_tcp::adapter::error::LanTcpError;

/// Per-frame payload size limit. Defense against a malicious or
/// misconfigured peer sending an absurd length prefix. Honest senders
/// stay well under via the body-lift policy (default 16 KiB ceiling).
pub(super) const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Outbound channel depth per connection. Slow remote → senders
/// applying backpressure pile up here before the Transport's
/// kind-aware dispatch kicks in.
pub(super) const OUTBOUND_CHANNEL_DEPTH: usize = 256;

/// Subscriber inbound channel depth — matches local-fs.
pub(super) const SUBSCRIBER_CHANNEL_DEPTH: usize = 64;

/// Active connection's sender half — write-task receives the
/// serialized payload (no length prefix; the write loop adds it)
/// from this and pushes the framed bytes onto the TLS stream.
/// Carrying pre-validated bytes rather than `Frame` ensures
/// `send()` can return synchronously if the frame is oversized or
/// unserializable — no post-acceptance silent drops in the write
/// loop. (Closes grievance §9 "Silent drop after acceptance is not
/// acceptable" / Codex audit 2026-05-19.)
pub(super) type OutboundTx = mpsc::Sender<Vec<u8>>;

/// One subscriber's filtered inbound channel + matching predicate.
pub(super) struct SubscriberHandle {
    pub(super) id: u64,
    pub(super) subscription: Subscription,
    pub(super) tx: mpsc::Sender<Result<Frame, LanTcpError>>,
}

/// Everything shared across the accept loop, per-connection tasks,
/// and subscriber dispatch. Constructed once at `LanTcpAdapter::new`
/// and threaded via `Arc`.
pub(super) struct Inner {
    pub(super) self_peer_id: PeerId,
    pub(super) keypair: PeerKeypair,
    pub(super) registry: Arc<RwLock<PeerKeyRegistry>>,
    pub(super) server_config: Arc<rustls::ServerConfig>,
    /// Active connections keyed by remote `PeerId`. Filled by both
    /// the accept loop (server-side handshakes) and `connect()`
    /// (client-side dials). Entries removed when the read loop
    /// detects a closed connection.
    pub(super) connections: Mutex<HashMap<PeerId, OutboundTx>>,
    /// True after the first `listen()` call so subsequent calls
    /// error rather than silently spawning a second accept loop.
    pub(super) listening: Mutex<bool>,
    pub(super) subscribers: Mutex<Vec<SubscriberHandle>>,
    pub(super) next_sub_id: AtomicU64,
}
