//! Typed setup for the relay client adapter.

use std::net::SocketAddr;
use std::sync::Arc;

use airc_core::PeerId;
use airc_protocol::{PeerKeyRegistry, PeerKeypair};

/// Embedder-supplied configuration for [`super::RelayAdapter`].
///
/// The relay's `peer_id` + matching pubkey MUST already be enrolled in
/// `registry` before connect — that's how the pinned-server verifier
/// recognises the relay's TLS cert. Missing-from-registry is a hard
/// error; the adapter will refuse to connect rather than fall back to
/// an unverified path.
pub struct RelayClientConfig {
    /// This peer's own `PeerId` — goes into the client cert so the
    /// relay can attribute inbound frames.
    pub self_peer_id: PeerId,
    /// This peer's Ed25519 identity.
    pub self_keypair: PeerKeypair,
    /// The relay's `PeerId`. Used by the pinned-server verifier to look
    /// up the expected pubkey in `registry`.
    pub relay_peer_id: PeerId,
    /// Where to connect to the relay.
    pub relay_addr: SocketAddr,
    /// Shared peer-key registry. MUST contain an enrolled entry for
    /// `relay_peer_id` before [`super::RelayAdapter::connect`] is
    /// called.
    pub registry: Arc<PeerKeyRegistry>,
}
