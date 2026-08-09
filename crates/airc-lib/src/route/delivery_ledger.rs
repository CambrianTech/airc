//! Per-peer delivery ledger — the end-to-end truth layer (#1306).
//!
//! ## The gap this closes
//!
//! Delivery acks exist on the wire (cards 39d37629 + 1998f6cb: every
//! routed forward requests one, and the remote acks only after durable
//! store). But their outcomes terminated in a global counter — nothing
//! fed them back into route health or route refresh. Health was stamped
//! `Healthy` from "a listener is bound or a connection object exists"
//! (`state=healthy (not measured)`), and route refresh SKIPPED any peer
//! present in the adapter's connection map. A half-open TCP session
//! (peer force-killed, NAT state dropped, machine rebooted) keeps its
//! map entry forever, so the one signal that proves non-delivery — sent
//! frames going unacked — was observed, counted, and ignored, while
//! both sides' doctors reported 8/8 clean. 2026-07-31: hours of silent
//! one-directional loss between two "healthy" daemons, twice in one day.
//!
//! ## The mechanism
//!
//! The [`RoutedForwarder`](crate::RoutedForwarder) records every frame
//! it flushes to a peer ([`DeliveryLedger::record_attempt`]) and every
//! ack that comes back ([`DeliveryLedger::record_ack`] — ANY typed
//! outcome proves the pipe and the remote daemon, `delivered` and
//! `undeliverable` alike). From that, two derived truths:
//!
//! - **Suspect** ([`DeliveryLedger::is_suspect`]): ≥
//!   [`SUSPECT_UNACKED_ATTEMPTS`] consecutive flushed-but-unacked
//!   frames. Route refresh drops a suspect peer's connection and
//!   re-dials instead of skipping it as "connected" — the half-open
//!   lie dies here.
//! - **Measured health** ([`DeliveryLedger::aggregate`]): real
//!   `rtt_ms` / `success_ppm` for the lan-tcp health sample, replacing
//!   `not measured` whenever at least one forward has been attempted.
//!
//! Pure accounting, no I/O, no clock reads of its own — callers pass
//! `now_ms` (one wall-clock read per refresh, matching discovery's
//! discipline).

use airc_core::PeerId;
use dashmap::DashMap;

/// Consecutive flushed-but-unacked forwards after which a peer's
/// "connected" state is no longer believed. Each attempt already waited
/// the forwarder's full ack timeout (10s default), so 2 means ≥ ~20s of
/// proven non-delivery — decisive for a half-open socket, tolerant of a
/// single dropped ack frame.
pub const SUSPECT_UNACKED_ATTEMPTS: u32 = 2;

/// Per-peer delivery accounting. All counters are lifetime totals for
/// this daemon process; recency is carried by the `*_ms` stamps.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PeerDeliveryStats {
    /// Frames actually flushed to this peer's connection (post
    /// `send_to` success — kernel buffer, not enqueue).
    pub attempts: u64,
    /// Typed delivery acks received from this peer (any outcome).
    pub acked: u64,
    /// Flushed frames since the last ack — the suspect trigger.
    pub attempts_since_ack: u32,
    pub last_attempt_ms: Option<u64>,
    pub last_ack_ms: Option<u64>,
    /// Exponential moving average of send→ack round trips.
    pub rtt_ema_ms: Option<u32>,
}

impl PeerDeliveryStats {
    /// True when enough consecutive flushed frames went unacked that
    /// this peer's live connection is presumed half-open. Pure — unit
    /// tested without a forwarder.
    pub fn suspect(&self) -> bool {
        self.attempts_since_ack >= SUSPECT_UNACKED_ATTEMPTS
    }
}

/// Cross-peer aggregate for the transport-health sample.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeliveryAggregate {
    pub attempts: u64,
    pub acked: u64,
    /// Success rate in parts-per-million over all attempts.
    pub success_ppm: u32,
    /// Best (minimum) per-peer RTT EMA — the latency of the healthiest
    /// live route, the number an operator compares against ping.
    pub best_rtt_ms: Option<u32>,
    /// Every peer we have attempted delivery to is currently suspect —
    /// the "all pipes lie" state that must surface as Degraded.
    pub all_suspect: bool,
}

/// Shared per-daemon delivery ledger. One instance per process,
/// created by the [`RoutedForwarder`](crate::RoutedForwarder) and
/// shared with the daemon's route-refresh handle.
#[derive(Debug, Default)]
pub struct DeliveryLedger {
    peers: DashMap<PeerId, PeerDeliveryStats>,
}

impl DeliveryLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// A frame was flushed to `peer`'s connection.
    pub fn record_attempt(&self, peer: PeerId, now_ms: u64) {
        let mut stats = self.peers.entry(peer).or_default();
        stats.attempts += 1;
        stats.attempts_since_ack = stats.attempts_since_ack.saturating_add(1);
        stats.last_attempt_ms = Some(now_ms);
    }

    /// A typed delivery ack arrived from `peer`. ANY outcome counts —
    /// `undeliverable` still proves the pipe and the remote daemon; the
    /// forwarder's own retry/diagnostic logic handles the outcome.
    pub fn record_ack(&self, peer: PeerId, now_ms: u64, rtt_ms: Option<u32>) {
        let mut stats = self.peers.entry(peer).or_default();
        stats.acked += 1;
        stats.attempts_since_ack = 0;
        stats.last_ack_ms = Some(now_ms);
        if let Some(sample) = rtt_ms {
            stats.rtt_ema_ms = Some(match stats.rtt_ema_ms {
                // EMA α=1/4: smooth enough to ignore one slow ack,
                // fresh enough to track a route change within ~4 acks.
                Some(ema) => (ema / 4).saturating_mul(3).saturating_add(sample / 4),
                None => sample,
            });
        }
    }

    /// See [`PeerDeliveryStats::suspect`].
    pub fn is_suspect(&self, peer: PeerId) -> bool {
        self.peers
            .get(&peer)
            .map(|stats| stats.suspect())
            .unwrap_or(false)
    }

    /// A suspect peer's connection was dropped for re-dial. Reset the
    /// unacked run so the FRESH connection gets a full
    /// [`SUSPECT_UNACKED_ATTEMPTS`] window to prove itself instead of
    /// being purged again on its first frame.
    pub fn note_purged(&self, peer: PeerId) {
        if let Some(mut stats) = self.peers.get_mut(&peer) {
            stats.attempts_since_ack = 0;
        }
    }

    pub fn stats(&self, peer: PeerId) -> Option<PeerDeliveryStats> {
        self.peers.get(&peer).map(|stats| *stats)
    }

    /// All peers with any recorded activity, for surfacing ("last
    /// confirmed delivery to X: N ago").
    pub fn snapshot(&self) -> Vec<(PeerId, PeerDeliveryStats)> {
        let mut rows: Vec<_> = self
            .peers
            .iter()
            .map(|entry| (*entry.key(), *entry.value()))
            .collect();
        rows.sort_by_key(|(peer, _)| peer.as_uuid());
        rows
    }

    /// Cross-peer aggregate, `None` until the first attempt — callers
    /// keep their unmeasured fallback for the no-traffic case.
    pub fn aggregate(&self) -> Option<DeliveryAggregate> {
        let mut attempts = 0u64;
        let mut acked = 0u64;
        let mut best_rtt_ms: Option<u32> = None;
        let mut attempted_peers = 0u32;
        let mut suspect_peers = 0u32;
        for entry in self.peers.iter() {
            let stats = entry.value();
            if stats.attempts == 0 {
                continue;
            }
            attempted_peers += 1;
            attempts += stats.attempts;
            acked += stats.acked;
            if stats.suspect() {
                suspect_peers += 1;
            }
            if let Some(rtt) = stats.rtt_ema_ms {
                best_rtt_ms = Some(best_rtt_ms.map_or(rtt, |best| best.min(rtt)));
            }
        }
        if attempts == 0 {
            return None;
        }
        let success_ppm =
            u32::try_from(acked.saturating_mul(1_000_000) / attempts).unwrap_or(1_000_000);
        Some(DeliveryAggregate {
            attempts,
            acked,
            success_ppm: success_ppm.min(1_000_000),
            best_rtt_ms,
            all_suspect: attempted_peers > 0 && suspect_peers == attempted_peers,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(n: u128) -> PeerId {
        PeerId::from_u128(n)
    }

    /// what this catches: the suspect trigger — the exact half-open
    /// signature (frames flushed, acks absent) must flip the verdict at
    /// the threshold, and one ack must clear the whole run.
    #[test]
    fn unacked_run_flips_suspect_and_one_ack_clears_it() {
        let ledger = DeliveryLedger::new();
        let p = peer(1);
        assert!(!ledger.is_suspect(p), "no traffic = no verdict");
        ledger.record_attempt(p, 1_000);
        assert!(!ledger.is_suspect(p), "one unacked attempt is tolerated");
        ledger.record_attempt(p, 2_000);
        assert!(ledger.is_suspect(p), "threshold reached");
        ledger.record_ack(p, 3_000, Some(40));
        assert!(!ledger.is_suspect(p), "any ack proves the pipe");
        let stats = ledger.stats(p).expect("stats recorded");
        assert_eq!(stats.attempts, 2);
        assert_eq!(stats.acked, 1);
        assert_eq!(stats.attempts_since_ack, 0);
    }

    /// what this catches: the purge reset — a re-dialed peer must get a
    /// fresh SUSPECT_UNACKED_ATTEMPTS window, not be instantly purged
    /// again on its first frame.
    #[test]
    fn note_purged_resets_the_unacked_run() {
        let ledger = DeliveryLedger::new();
        let p = peer(2);
        ledger.record_attempt(p, 1_000);
        ledger.record_attempt(p, 2_000);
        assert!(ledger.is_suspect(p));
        ledger.note_purged(p);
        assert!(!ledger.is_suspect(p));
        ledger.record_attempt(p, 3_000);
        assert!(!ledger.is_suspect(p), "fresh window after purge");
        ledger.record_attempt(p, 4_000);
        assert!(ledger.is_suspect(p), "suspect re-earned honestly");
    }

    /// what this catches: aggregate truth for the health sample — ppm
    /// arithmetic, best-RTT selection, and the all-suspect degraded
    /// signal (one healthy peer must veto it).
    #[test]
    fn aggregate_reports_ppm_best_rtt_and_all_suspect() {
        let ledger = DeliveryLedger::new();
        assert_eq!(ledger.aggregate(), None, "no attempts = unmeasured");

        let good = peer(3);
        let bad = peer(4);
        ledger.record_attempt(good, 1_000);
        ledger.record_ack(good, 1_050, Some(50));
        ledger.record_attempt(bad, 1_000);
        ledger.record_attempt(bad, 2_000);

        let agg = ledger.aggregate().expect("attempts recorded");
        assert_eq!(agg.attempts, 3);
        assert_eq!(agg.acked, 1);
        assert_eq!(agg.success_ppm, 333_333);
        assert_eq!(agg.best_rtt_ms, Some(50));
        assert!(!agg.all_suspect, "one acking peer vetoes all-suspect");

        ledger.record_attempt(good, 3_000);
        ledger.record_attempt(good, 4_000);
        assert!(
            ledger.aggregate().expect("attempts recorded").all_suspect,
            "every attempted peer suspect = all_suspect"
        );
    }

    /// what this catches: RTT EMA smoothing — one slow ack must not
    /// replace the average wholesale (α=1/4 blend).
    #[test]
    fn rtt_ema_smooths_rather_than_replaces() {
        let ledger = DeliveryLedger::new();
        let p = peer(5);
        ledger.record_attempt(p, 1_000);
        ledger.record_ack(p, 1_040, Some(40));
        ledger.record_attempt(p, 2_000);
        ledger.record_ack(p, 3_000, Some(1_000));
        let ema = ledger.stats(p).expect("stats").rtt_ema_ms.expect("rtt");
        assert!(
            ema > 40 && ema < 1_000,
            "one outlier blends, never replaces: got {ema}"
        );
    }
}
