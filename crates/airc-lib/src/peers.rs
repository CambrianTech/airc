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
    /// the peer trust store. Public API; defaults the `via` tag on
    /// the lifecycle event to `"manual"`.
    pub async fn add_peer(&self, spec: PeerSpec) -> Result<(), AircError> {
        self.add_peer_via(spec, "manual").await
    }

    /// Remove a peer from local durable trust and in-memory
    /// verification state. Emits `PeerDeparted` only when a stored peer
    /// was actually removed.
    pub async fn remove_peer(&self, peer_id: PeerId, reason: &str) -> Result<bool, AircError> {
        let removed_home = peers_store::remove(&self.inner.home, peer_id).await?;
        let removed_wire_root = if self.inner.wire_root != self.inner.home {
            peers_store::remove(&self.inner.wire_root, peer_id).await?
        } else {
            None
        };
        let removed = removed_home.or(removed_wire_root).is_some();

        {
            let mut registry = self
                .inner
                .registry
                .write()
                .map_err(|_| AircError::Crypto("registry lock poisoned".to_string()))?;
            registry.remove_peer(peer_id);
        }

        if !removed {
            return Ok(false);
        }

        let room_id = self.current_room().await?.channel;
        let body = airc_core::Body::Json(
            serde_json::to_value(crate::lifecycle::PeerDepartedBody {
                peer_id,
                reason: reason.to_string(),
            })
            .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?,
        );
        self.emit_lifecycle(airc_core::TranscriptKind::PeerDeparted, room_id, body)
            .await?;
        Ok(true)
    }

    /// Internal: enrol a peer and emit `PeerArrived` with the
    /// caller-supplied `via` tag (`"invite"`, `"account_registry"`,
    /// `"manual"`, etc.). Callers that know how the peer was
    /// discovered call this directly so subscribers see the typed
    /// provenance.
    pub(crate) async fn add_peer_via(&self, spec: PeerSpec, via: &str) -> Result<(), AircError> {
        let already_known = self
            .peers()
            .await?
            .iter()
            .any(|p| p.peer_id == spec.peer_id);
        peers_store::add(&self.inner.home, spec.peer_id, spec.pubkey).await?;
        self.enrol_volatile_peer(&spec)?;

        // Only emit on first arrival — re-adding an already-known
        // peer (idempotent enrol) shouldn't fire a duplicate
        // lifecycle event. Also requires a current default room to
        // route through; if the local scope hasn't joined any room
        // yet, the event has nowhere to live and the consumer can
        // introspect `Airc::peers()` directly on first join.
        if already_known {
            return Ok(());
        }
        let room_id = match self.current_room().await {
            Ok(room) => room.channel,
            Err(_) => return Ok(()),
        };
        let body = airc_core::Body::Json(
            serde_json::to_value(crate::lifecycle::PeerArrivedBody {
                peer_id: spec.peer_id,
                via: via.to_string(),
            })
            .map_err(|e| AircError::Crypto(format!("lifecycle body serialize: {e}")))?,
        );
        self.emit_lifecycle(airc_core::TranscriptKind::PeerArrived, room_id, body)
            .await?;
        Ok(())
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
