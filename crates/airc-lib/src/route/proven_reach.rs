//! Transport-proven reachability: which enrolled peers this daemon has
//! actually exchanged acknowledged frames with.
//!
//! ## The failure this owns (M5 <-> BigMama, 2026-11-12 to 11-14)
//!
//! Route discovery skips a "ghost": an enrolled peer whose `last_seen_ms` is
//! older than the registry freshness TTL. `last_seen_ms` advances only on a
//! registry beacon import (and a manual `airc dial`), so liveness was decided
//! by GitHub alone. BigMama's beacon stopped refreshing; its LAN link to the
//! M5 then went half-open. The suspect purge dropped the session "for re-dial
//! this refresh", and the same refresh skipped the re-dial because the peer's
//! beacon was stale. Neither side dialed the other for 44 hours, although
//! BigMama's listener answered the first manual dial at once, after 166,354
//! acknowledged frames with a measured 31 ms rtt.
//!
//! An acknowledgement is stronger evidence of life than any beacon: the
//! remote daemon authenticated, stored, and answered. So a peer that
//! acknowledged frames in this daemon's lifetime is never a ghost. Its dead
//! endpoints are governed by the dial quarantine (timed backoff, then
//! failure-counted eviction and the retention sweep's re-probe), which is the
//! owner that bounds the cost of dialing a peer that really is gone. Peers
//! that never acknowledged anything (dead containers, abandoned scopes) are
//! still ghost-skipped exactly as before.
//!
//! A record counts when its own id or its declared transport host (the
//! machine daemon whose listener serves a scope peer) acknowledged. Ids are
//! stable: one public key names one peer in the trust store and the verifier
//! registry, so the ledger's id is the record's id.

use std::collections::HashSet;

use airc_core::PeerId;
use airc_store::StoredPeer;

use super::delivery_ledger::PeerDeliveryStats;

/// Peers that acknowledged at least one frame.
#[derive(Debug, Default)]
pub(crate) struct ProvenReach {
    acked: HashSet<PeerId>,
}

impl ProvenReach {
    pub(crate) fn from_ledger(ledger: &[(PeerId, PeerDeliveryStats)]) -> Self {
        Self {
            acked: ledger
                .iter()
                .filter(|(_, stats)| stats.acked > 0)
                .map(|(peer, _)| *peer)
                .collect(),
        }
    }

    /// Whether the record's own id, or the host answering at its endpoints,
    /// has acknowledged frames from this daemon.
    pub(crate) fn proves(&self, record: &StoredPeer) -> bool {
        [Some(record.peer_id), record.endpoints_peer_id]
            .into_iter()
            .flatten()
            .any(|peer| self.acked.contains(&peer))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(peer: u128, host: Option<u128>) -> StoredPeer {
        StoredPeer {
            peer_id: PeerId::from_u128(peer),
            pubkey_b64: format!("key-{peer}"),
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

    // what this catches: the 44-hour M5 <-> BigMama partition. The machine
    // daemon e85a5bb3 acknowledged frames; Codex's scope record ce8b9074
    // carries the endpoints with e85a5bb3 as host. Both must count as proven,
    // so discovery re-dials instead of skipping a stale beacon as a ghost.
    #[test]
    fn a_hosted_record_is_proven_by_its_hosts_acknowledgements() {
        let machine = stored(0xe85a, None);
        let scope = stored(0xce8b, Some(0xe85a));
        let reach = ProvenReach::from_ledger(&[acked(0xe85a, 166_354)]);
        assert!(reach.proves(&machine));
        assert!(reach.proves(&scope));
    }

    // what this catches: weakening the ghost rule for peers that never
    // answered. Attempts without a single acknowledgement prove nothing, so
    // a dead container stays skipped.
    #[test]
    fn attempts_without_an_acknowledgement_prove_nothing() {
        let reach = ProvenReach::from_ledger(&[acked(1, 0)]);
        assert!(!reach.proves(&stored(1, None)));
        assert!(!reach.proves(&stored(2, Some(1))));
    }
}
