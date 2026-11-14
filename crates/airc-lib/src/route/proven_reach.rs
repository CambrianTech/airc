//! Transport-proven reachability: which enrolled identities this daemon has
//! actually exchanged acknowledged frames with.
//!
//! ## The failure this owns (M5 <-> BigMama, 2026-11-12 to 11-14)
//!
//! Route discovery skips a "ghost": an enrolled peer whose `last_seen_ms` is
//! older than the registry freshness TTL. `last_seen_ms` advances only on a
//! registry beacon import (and a manual `airc dial`), so liveness was decided
//! by GitHub alone. BigMama's beacon stopped refreshing; its LAN link to the
//! M5 then went half-open. The suspect purge dropped the session "for re-dial
//! this refresh", and the very same refresh skipped the re-dial because the
//! peer's beacon was stale. Neither side dialed the other for 44 hours,
//! although BigMama's listener answered the first manual dial at once, after
//! 166,354 acknowledged frames with a measured 31 ms rtt.
//!
//! An acknowledgement is stronger evidence of life than any beacon: the
//! remote daemon authenticated, stored, and answered. So an identity that
//! acknowledged frames in this daemon's lifetime is never a ghost. Its dead
//! endpoints are governed by the dial quarantine (timed backoff, then
//! failure-counted eviction and the retention sweep's re-probe), which is the
//! owner that bounds the cost of dialing a peer that really is gone. Peers
//! that never acknowledged anything (dead containers, abandoned scopes) are
//! still ghost-skipped exactly as before.
//!
//! ## Keyed by public key, not peer id
//!
//! One machine key can be enrolled under several peer ids: a re-minted
//! identity (BigMama's `4731e245` and `e85a5bb3` share one key) and scope
//! records whose endpoints are hosted by the machine daemon. The ledger keys a
//! session by whichever enrolled id the handshake resolved to, while the
//! record that carries the endpoints may name another id for the same key.
//! Comparing keys joins them without guessing which id is current.

use std::collections::{HashMap, HashSet};

use airc_core::PeerId;
use airc_store::StoredPeer;

use super::delivery_ledger::PeerDeliveryStats;

/// Public keys of the identities that acknowledged at least one frame.
#[derive(Debug, Default)]
pub(crate) struct ProvenReach {
    keys: HashSet<String>,
    key_of: HashMap<PeerId, String>,
}

impl ProvenReach {
    /// Join the ledger's acknowledged peers to their enrolled keys.
    pub(crate) fn from_evidence(
        ledger: &[(PeerId, PeerDeliveryStats)],
        stored: &[StoredPeer],
    ) -> Self {
        let key_of: HashMap<PeerId, String> = stored
            .iter()
            .map(|peer| (peer.peer_id, peer.pubkey_b64.clone()))
            .collect();
        let keys = ledger
            .iter()
            .filter(|(_, stats)| stats.acked > 0)
            .filter_map(|(peer, _)| key_of.get(peer).cloned())
            .collect();
        Self { keys, key_of }
    }

    /// Whether the identity answering at this record's endpoints, or the
    /// record's own identity, has acknowledged frames from this daemon.
    pub(crate) fn proves(&self, record: &StoredPeer) -> bool {
        [Some(record.peer_id), record.endpoints_peer_id]
            .into_iter()
            .flatten()
            .filter_map(|peer| self.key_of.get(&peer))
            .any(|key| self.keys.contains(key))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(peer: u128, key: &str, host: Option<u128>) -> StoredPeer {
        StoredPeer {
            peer_id: PeerId::from_u128(peer),
            pubkey_b64: key.to_string(),
            added_at_ms: 0,
            tier: airc_store::TrustTier::OwnAccount,
            endpoints_json: None,
            last_seen_ms: 0,
            endpoints_advertised_at_ms: 0,
            endpoints_peer_id: host.map(PeerId::from_u128),
        }
    }

    fn acked(peer: u128, acked: u64) -> (PeerId, PeerDeliveryStats) {
        (
            PeerId::from_u128(peer),
            PeerDeliveryStats {
                attempts: acked + 2,
                acked,
                attempts_since_ack: 2,
                ..PeerDeliveryStats::default()
            },
        )
    }

    // what this catches: the 44-hour M5 <-> BigMama partition. The ledger
    // knew BigMama under its old id (4731e245), the endpoint-carrying scope
    // record named the re-minted machine id (e85a5bb3) as host, and both ids
    // carry one key. The scope record must count as proven, so discovery
    // re-dials it instead of skipping it as a ghost.
    #[test]
    fn a_reminted_host_is_proven_by_its_keys_acknowledgements() {
        let rows = [
            stored(0x4731, "machine-key", None),
            stored(0xe85a, "machine-key", None),
            stored(0xce8b, "scope-key", Some(0xe85a)),
        ];
        let reach = ProvenReach::from_evidence(&[acked(0x4731, 166_354)], &rows);
        assert!(reach.proves(&rows[2]));
        assert!(reach.proves(&rows[1]));
    }

    // what this catches: weakening the ghost rule for peers that never
    // answered. Attempts without a single acknowledgement prove nothing, so
    // a dead container stays skipped.
    #[test]
    fn attempts_without_an_acknowledgement_prove_nothing() {
        let rows = [stored(1, "ghost-key", None), stored(2, "other-key", None)];
        let reach = ProvenReach::from_evidence(&[acked(1, 0)], &rows);
        assert!(!reach.proves(&rows[0]));
        assert!(!reach.proves(&rows[1]));
    }
}
