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

use tokio::sync::{mpsc, oneshot, Mutex};

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

/// One queued outbound payload plus a completion signal the write loop
/// fires AFTER it has flushed the bytes to the TLS stream.
///
/// `send()` awaits `flushed` so it returns only once the frame is on the
/// wire (in the kernel send buffer), not merely enqueued. Without this a
/// one-shot caller — e.g. the `lan-send` CLI — could enqueue, see `Ok`,
/// print "sent", and exit, aborting the background write task before it
/// flushed, silently losing the frame. (Enqueue-only `Ok` was a real
/// intermittent frame drop under load.) The payload is pre-validated
/// bytes (length-checked + serialized in `send()`), so the write loop
/// still has no post-acceptance failure mode beyond a dead socket —
/// grievance §9 / Codex audit 2026-05-19.
pub(super) struct Outbound {
    pub(super) payload: Vec<u8>,
    pub(super) flushed: oneshot::Sender<()>,
}

/// Active connection's sender half — the write task receives
/// [`Outbound`] items from this and pushes the framed bytes onto the
/// TLS stream, signalling `flushed` once each is on the wire.
pub(super) type OutboundTx = mpsc::Sender<Outbound>;

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
    pub(super) registry: Arc<PeerKeyRegistry>,
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
