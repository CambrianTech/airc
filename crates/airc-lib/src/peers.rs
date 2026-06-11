use airc_core::PeerId;
use airc_trust as peers_store;

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

        self.inner.registry.remove_peer(peer_id);

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
        self.inner
            .registry
            .enrol(spec.peer_id, 0, spec.pubkey)
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Return a list of enrolled peers.
    ///
    /// A node legitimately carries MORE THAN ONE of its own
    /// identities under a single machine account: the scope identity
    /// (`home`) that this handle was opened with, AND the
    /// machine-account identity that the daemon's account-registry
    /// loop opens against `machine_account_home(home)` (its own
    /// distinct keypair). Both are SELF — neither is a paired remote
    /// peer. The account-registry auto-discovery keystone (card
    /// a134b370) made this split observable: the loop's `Airc::open`
    /// enrols its machine-account identity into the SHARED `wire_root`
    /// trust store, and a `peers()` that filtered only the scope
    /// identity then counted the node's OWN beacon as a "1 paired
    /// remote peer" — violating the no-lying-about-delivery invariant
    /// a lone node depends on (events_commands
    /// `send_receipt_distinguishes_zero_paired_peers_without_lying_about_delivery`).
    /// Excluding EVERY self identity (across all of the node's homes)
    /// is the correct boundary: a peer is "remote" only if its key is
    /// none of ours.
    pub async fn peers(&self) -> Result<Vec<EnrolledPeer>, AircError> {
        let self_ids = self.self_peer_ids().await?;
        let stored =
            crate::airc::load_peer_registries(&self.inner.home, &self.inner.wire_root).await?;
        let mut peers = stored
            .into_iter()
            .filter(|p| !self_ids.contains(&p.peer_id))
            .map(|p| EnrolledPeer {
                peer_id: p.peer_id,
                pubkey_b64: p.pubkey_b64,
            })
            .collect::<Vec<_>>();
        peers.sort_by_key(|p| p.peer_id.to_string());
        peers.dedup_by_key(|p| p.peer_id);
        Ok(peers)
    }

    /// The set of peer ids that are THIS node — never a paired remote
    /// peer. Always includes the in-memory scope identity this handle
    /// holds. When the machine-account home differs from the scope
    /// home (the daemon/account-registry-loop case), it ALSO includes
    /// the machine-account identity recorded in the shared wire-root
    /// coordinator store, so a node's own auto-discovery beacon can
    /// never round-trip back as a "remote" peer. Best-effort on the
    /// wire-root lookup: a missing/unreadable machine-account identity
    /// degrades to the scope-only filter (the prior behaviour), never
    /// an error on the hot send path.
    pub(crate) async fn self_peer_ids(&self) -> Result<Vec<PeerId>, AircError> {
        let mut ids = vec![self.inner.identity.peer_id];
        if self.inner.wire_root != self.inner.home {
            if let Ok(Some(stored)) = self.coordinator_store().load_local_identity().await {
                if !ids.contains(&stored.peer_id) {
                    ids.push(stored.peer_id);
                }
            }
        }
        Ok(ids)
    }
}

#[cfg(test)]
mod tests {
    use crate::Airc;
    use tempfile::tempdir;

    /// Regression for the account-registry auto-discovery keystone
    /// (card a134b370): the daemon's registry-refresh loop opens its
    /// OWN `Airc` handle against the machine-account home — a distinct
    /// keypair from any scope handle — and `Airc::open` enrols that
    /// machine-account identity into the SHARED wire-root trust store.
    /// A scope handle's `peers()` must NOT count that as a paired
    /// remote peer: it is the node's own auto-discovery beacon. This
    /// is exactly the topology behind the events_commands failure
    /// (`send` with zero real peers reported "1 paired peer").
    #[tokio::test]
    async fn peers_excludes_machine_account_self_identity() {
        let dir = tempdir().unwrap();
        // The machine-account home == the shared wire root, mirroring
        // `machine_account_home(scope)` resolving to `$HOME/.airc`.
        let machine_account_home = dir.path().join(".airc");
        let scope_home = dir.path().join("scope");

        // The daemon's registry loop opens against the machine-account
        // home (its wire_root == its home there). This writes the
        // loop's distinct identity into the wire-root trust store —
        // the node's own beacon.
        let loop_handle =
            Airc::open_with_wire_root_for_test(&machine_account_home, &machine_account_home)
                .await
                .unwrap();

        // The foreground send client: a separate scope home sharing the
        // same wire root (the `attached_airc` topology).
        let send_client = Airc::open_with_wire_root_for_test(&scope_home, &machine_account_home)
            .await
            .unwrap();

        // Both identities are now in the shared wire-root trust store,
        // but BOTH are self (scope identity + machine-account identity).
        assert_ne!(
            loop_handle.peer_id(),
            send_client.peer_id(),
            "loop and send client must have distinct identities for this to be a real test"
        );

        let peers = send_client.peers().await.unwrap();
        assert!(
            peers.is_empty(),
            "a lone node with no real remote peers must report 0 paired peers — \
             the machine-account loop's own beacon is self, not a remote peer; got: {peers:?}"
        );
    }

    /// The inverse: a genuine remote peer enrolled into the wire root
    /// IS counted. Guards against the self-filter over-reaching and
    /// hiding real peers.
    #[tokio::test]
    async fn peers_still_counts_a_genuine_remote_peer() {
        let dir = tempdir().unwrap();
        let machine_account_home = dir.path().join(".airc");
        let scope_home = dir.path().join("scope");

        let _loop_handle =
            Airc::open_with_wire_root_for_test(&machine_account_home, &machine_account_home)
                .await
                .unwrap();
        let send_client = Airc::open_with_wire_root_for_test(&scope_home, &machine_account_home)
            .await
            .unwrap();

        let remote_id = airc_core::PeerId::new();
        let remote = airc_protocol::PeerKeypair::generate();
        airc_trust::add(&machine_account_home, remote_id, remote.public_bytes())
            .await
            .unwrap();

        let peers = send_client.peers().await.unwrap();
        assert_eq!(
            peers.len(),
            1,
            "a genuine enrolled remote peer must still surface; got: {peers:?}"
        );
        assert_eq!(peers[0].peer_id, remote_id);
    }
}
