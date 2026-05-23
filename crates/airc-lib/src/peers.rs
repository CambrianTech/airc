use airc_core::PeerId;
use airc_daemon::peers_store;

use crate::error::AircError;
use crate::registry::PeerSpec;
use crate::Airc;

/// One row in `Airc::peers`. Mirrors the daemon's `PeerEntry`
/// without forcing consumers to pull the daemon crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrolledPeer {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
}

impl Airc {
    /// Return the peer-spec string suitable for sharing with another
    /// peer so they can enrol this identity into their trust registry.
    pub fn peer_spec(&self) -> String {
        crate::registry::format_peer_spec(
            self.inner.identity.peer_id,
            &self.inner.identity.keypair.public_bytes(),
        )
    }

    /// Enrol a peer into the local trust registry and persist it to
    /// the peer trust store.
    pub async fn add_peer(&self, spec: PeerSpec) -> Result<(), AircError> {
        peers_store::add(&self.inner.home, spec.peer_id, spec.pubkey).await?;
        self.enrol_volatile_peer(&spec)
    }

    /// Enrol a peer in the in-memory trust registry without writing
    /// durable peer trust state.
    pub fn enrol_volatile_peer(&self, spec: &PeerSpec) -> Result<(), AircError> {
        let mut registry = self
            .inner
            .registry
            .write()
            .map_err(|_| AircError::Crypto("registry lock poisoned".to_string()))?;
        registry
            .enrol(spec.peer_id, 0, spec.pubkey)
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Return a list of enrolled peers.
    pub async fn peers(&self) -> Result<Vec<EnrolledPeer>, AircError> {
        let stored =
            crate::airc::load_peer_registries(&self.inner.home, &self.inner.wire_root).await?;
        let mut peers = stored
            .into_iter()
            .filter(|p| p.peer_id != self.inner.identity.peer_id)
            .map(|p| EnrolledPeer {
                peer_id: p.peer_id,
                pubkey_b64: p.pubkey_b64,
            })
            .collect::<Vec<_>>();
        peers.sort_by_key(|p| p.peer_id.to_string());
        peers.dedup_by_key(|p| p.peer_id);
        Ok(peers)
    }
}
