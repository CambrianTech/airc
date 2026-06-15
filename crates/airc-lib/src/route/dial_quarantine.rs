//! In-session dial-failure quarantine for route discovery.
//!
//! Card 7e3c9a1f — "fix airc peer-connection for good." The registry
//! merge ([`crate::account_registry::merge_registry_documents`]) already
//! keeps only the freshest beacon per peer and prunes dead publishers,
//! and the import path overwrites a peer's `endpoints_json` with its
//! latest advertised set. What it does NOT do is stop the dial loop from
//! re-attempting an endpoint that just failed: between registry refreshes
//! (~120s cadence) a stale endpoint and a fresh one coexist in the trust
//! store and [`crate::route::discovery`] dials BOTH, each costing up to
//! `PEER_DIAL_TIMEOUT` (3s) on a SYN-dropping firewall — so reaching a
//! live peer is starved behind 3s-per-corpse dials every single refresh.
//!
//! The grid symptom Joel flagged: "two trusted peers can't maintain a
//! conversation." A daemon that restarts on a new port leaves its old
//! `addr` in every other node's trust store until the next registry
//! converge; without quarantine each node re-dials that dead `addr` on
//! every `transport health` / `doctor` / discovery tick.
//!
//! This is the in-memory memory of "that endpoint just failed, back off
//! before dialing it again." It is intentionally NOT persisted: a restart
//! clears it, which is correct — the registry re-converges on the live
//! endpoint set anyway, and a persisted quarantine could lock out a peer
//! that came back on its old port. A successful dial clears the entry
//! immediately, so a flapping-but-reachable endpoint recovers on its next
//! good dial rather than waiting out a backoff.

use std::collections::HashMap;
use std::net::SocketAddr;

/// Backoff applied after the FIRST failed dial to an endpoint. Sized
/// well above any tight discovery-retry loop (so a corpse is dialed at
/// most ~once per window instead of every tick) yet short enough that a
/// peer which comes back on the SAME port reconnects within one window.
pub const INITIAL_BACKOFF_MS: u64 = 15_000;

/// Ceiling for the doubling backoff. Capped at 2 minutes — comparable to
/// the registry refresh cadence — so a same-port comeback is never locked
/// out longer than it takes the registry to re-confirm the peer anyway.
pub const MAX_BACKOFF_MS: u64 = 120_000;

/// One quarantined endpoint: when it last failed and how long to skip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuarantineEntry {
    /// Wall-clock ms of the most recent failed dial.
    failed_at_ms: u64,
    /// Current skip window. The endpoint is skipped until
    /// `failed_at_ms + backoff_ms`; doubles per consecutive failure up to
    /// [`MAX_BACKOFF_MS`].
    backoff_ms: u64,
}

/// In-memory, per-handle quarantine of recently-failed dial endpoints.
///
/// Keyed by the dialed [`SocketAddr`] rather than `(peer_id, endpoint)`:
/// the dead thing is the address (a freed port), and the same address can
/// be advertised under more than one stale peer record — keying on the
/// address quarantines the corpse once for all of them.
#[derive(Debug, Default)]
pub struct DialQuarantine {
    entries: HashMap<SocketAddr, QuarantineEntry>,
}

impl DialQuarantine {
    /// True when `addr` failed recently enough that it is still inside its
    /// backoff window at `now_ms` and must be skipped this refresh. A
    /// future-dated `now_ms` (clock rewind) reads as "window elapsed" via
    /// saturating-sub — we never extend a quarantine because our clock
    /// jumped backwards.
    pub fn is_quarantined(&self, addr: &SocketAddr, now_ms: u64) -> bool {
        match self.entries.get(addr) {
            Some(entry) => now_ms.saturating_sub(entry.failed_at_ms) < entry.backoff_ms,
            None => false,
        }
    }

    /// Record a failed dial to `addr`: start the backoff at
    /// [`INITIAL_BACKOFF_MS`], or double the existing window (capped at
    /// [`MAX_BACKOFF_MS`]) for a repeat failure.
    pub fn record_failure(&mut self, addr: SocketAddr, now_ms: u64) {
        let backoff_ms = match self.entries.get(&addr) {
            Some(prev) => prev.backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS),
            None => INITIAL_BACKOFF_MS,
        };
        self.entries.insert(
            addr,
            QuarantineEntry {
                failed_at_ms: now_ms,
                backoff_ms,
            },
        );
    }

    /// A successful dial clears any quarantine for `addr` so the endpoint
    /// is immediately eligible again — a peer that flapped and came back
    /// must not stay shadow-banned behind a stale backoff.
    pub fn record_success(&mut self, addr: &SocketAddr) {
        self.entries.remove(addr);
    }

    /// Number of currently-tracked endpoints. Diagnostics only.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether any endpoint is tracked. Diagnostics only.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(port: u16) -> SocketAddr {
        SocketAddr::from(([10, 0, 0, 2], port))
    }

    // what this catches: a fresh, never-failed endpoint is NEVER
    // quarantined — the dial loop must always attempt an endpoint it has
    // no failure memory for (the live-endpoint path must not be gated).
    #[test]
    fn unseen_endpoint_is_never_quarantined() {
        let q = DialQuarantine::default();
        assert!(!q.is_quarantined(&addr(7717), 1_000_000));
    }

    // what this catches: a single failure suppresses re-dials for exactly
    // INITIAL_BACKOFF_MS, then the endpoint becomes eligible again. The
    // boundary (== window) must be eligible, not skipped — off-by-one here
    // would either hammer a corpse one tick early or lock it an extra tick.
    #[test]
    fn failure_quarantines_for_initial_window_then_expires() {
        let mut q = DialQuarantine::default();
        let now = 1_000_000;
        q.record_failure(addr(7717), now);

        assert!(q.is_quarantined(&addr(7717), now), "skipped immediately");
        assert!(
            q.is_quarantined(&addr(7717), now + INITIAL_BACKOFF_MS - 1),
            "still skipped just inside the window"
        );
        assert!(
            !q.is_quarantined(&addr(7717), now + INITIAL_BACKOFF_MS),
            "eligible again at the window boundary"
        );
    }

    // what this catches: consecutive failures double the backoff (capped),
    // so a persistently-dead corpse is dialed exponentially less often
    // instead of every tick — the starvation fix. Mutation check: making
    // record_failure reset to INITIAL each time fails the "second window is
    // longer" assertion.
    #[test]
    fn repeat_failures_double_backoff_up_to_cap() {
        let mut q = DialQuarantine::default();
        let mut now = 0u64;

        q.record_failure(addr(7717), now);
        // After the first window, still dead → fail again, window doubles.
        now += INITIAL_BACKOFF_MS;
        q.record_failure(addr(7717), now);
        assert!(
            q.is_quarantined(&addr(7717), now + INITIAL_BACKOFF_MS + 1),
            "second window must exceed the first (doubled)"
        );

        // Drive many failures; the window must saturate at the cap, never
        // overflow past it.
        for _ in 0..20 {
            q.record_failure(addr(7717), now);
        }
        assert!(q.is_quarantined(&addr(7717), now + MAX_BACKOFF_MS - 1));
        assert!(!q.is_quarantined(&addr(7717), now + MAX_BACKOFF_MS));
    }

    // what this catches: a successful dial clears the quarantine so a
    // recovered endpoint is immediately eligible — without this a peer
    // that came back on its old port would stay shadow-banned for the
    // remaining backoff.
    #[test]
    fn success_clears_quarantine_immediately() {
        let mut q = DialQuarantine::default();
        let now = 1_000_000;
        q.record_failure(addr(7717), now);
        assert!(q.is_quarantined(&addr(7717), now));

        q.record_success(&addr(7717));
        assert!(
            !q.is_quarantined(&addr(7717), now),
            "success must lift the quarantine in the same instant"
        );
        assert!(q.is_empty());
    }

    // what this catches: quarantine is per-address, so one dead port does
    // not suppress dials to a peer's OTHER (live) endpoint — the key is the
    // SocketAddr, not the peer.
    #[test]
    fn quarantine_is_scoped_per_address() {
        let mut q = DialQuarantine::default();
        let now = 1_000_000;
        q.record_failure(addr(7717), now);
        assert!(q.is_quarantined(&addr(7717), now));
        assert!(
            !q.is_quarantined(&addr(65438), now),
            "a different port (the live endpoint) stays eligible"
        );
    }
}
