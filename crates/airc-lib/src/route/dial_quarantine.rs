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
//!
//! ## Keyed by `(PeerId, SocketAddr)`, not the address alone
//!
//! Docker bridge IPs (and ephemeral ports) are RECYCLED: a dead peer's
//! `10.0.0.2:7717` can be reassigned to a DIFFERENT, live peer minutes
//! later. Keying on the address alone would let the dead peer's failure
//! shadow-ban the live peer that inherited the address — starving a real
//! connection for a full backoff window in exactly the containerized grid
//! this card targets. Keying on `(peer_id, addr)` means a corpse is
//! quarantined only for the peer it actually belongs to; a recycled
//! address under a new `peer_id` starts with a clean slate.
//!
//! ## Skips are SURFACED on their OWN channel, never silent
//!
//! When the dialer skips a quarantined endpoint it records a
//! [`crate::route::PeerDialSkip`] (with the remaining backoff) on the
//! discovery snapshot's separate `peer_dial_skips` channel — NOT a
//! [`crate::route::PeerDialFailure`]. `airc transport health` prints it as
//! "dial backoff: …" distinct from "dial failed: …", so the operator sees
//! the endpoint is intentionally backed off without it being miscounted or
//! mislabelled as an attempted-and-failed dial (which would emit false
//! `PeerDialFailed` warnings every refresh and inflate the failure count).
//! Silent suppression — the opposite error — would make the very tool used
//! to debug "two peers can't talk" lie during the window it matters most.
//!
//! ## Clock
//!
//! `now_ms` is the substrate wall clock (`crate::time::now_ms`), the same
//! source the rest of the discovery path uses. A backward step is handled
//! (saturating-sub reads as "window not yet elapsed" — we never EXTEND a
//! quarantine because our clock jumped back). A forward step can expire a
//! quarantine early; that is acceptable for a short backoff timer (the
//! worst case is one extra dial attempt, which simply re-quarantines).

use std::collections::HashMap;
use std::net::SocketAddr;

use airc_core::PeerId;

/// Backoff applied after the FIRST failed dial to an endpoint. Sized
/// well above any tight discovery-retry loop (so a corpse is dialed at
/// most ~once per window instead of every tick) yet short enough that a
/// peer which comes back on the SAME port reconnects within one window.
pub const INITIAL_BACKOFF_MS: u64 = 15_000;

/// Ceiling for the doubling backoff. Capped at 2 minutes — comparable to
/// the registry refresh cadence — so a same-port comeback is never locked
/// out longer than it takes the registry to re-confirm the peer anyway.
pub const MAX_BACKOFF_MS: u64 = 120_000;

/// How long after its LAST failure an entry is retained before being
/// swept (memory reclamation). Deliberately MUCH longer than
/// [`MAX_BACKOFF_MS`]: an endpoint still advertised in the registry is
/// re-dialed on the discovery cadence (~120s) and, if still dead, fails
/// again — each re-failure re-stamps `failed_at_ms`, so the entry must
/// OUTLIVE the backoff window or the doubling would reset every cycle and
/// a persistently-dead corpse would never escalate past the initial
/// backoff. An entry only ages out once it has NOT been re-failed for
/// this horizon — i.e. the endpoint succeeded (cleared explicitly) or
/// vanished from the registry (no longer dialed).
const QUARANTINE_RETENTION_MS: u64 = 600_000;

/// Self-healing join — failure-counted eviction (M5↔bigmama decay mode
/// #4, the 172.x ghost swarm): after this many CONSECUTIVE failures an
/// endpoint is DEAD — evicted from the dial set entirely (record kept,
/// endpoint skipped) until a FRESHER advertisement for the peer's
/// endpoint set arrives. Five attempts spans several timed-backoff
/// windows (~15s→120s), so a transiently-flapping endpoint recovers
/// long before eviction, while a genuinely-dead corpse stops burning a
/// 3s timeout on every discovery cycle forever.
pub const DEAD_AFTER_CONSECUTIVE_FAILURES: u32 = 5;

/// Quarantine key: the dead thing is a specific peer's endpoint, not the
/// bare address (which a recycled-IP live peer may now own).
type QuarantineKey = (PeerId, SocketAddr);

/// The dialer's verdict for one endpoint, from this quarantine's memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialGate {
    /// No failure memory (or the window elapsed, or a fresher
    /// advertisement revived the endpoint) — dial it.
    Dial,
    /// Inside the timed backoff window after recent failure(s).
    Backoff { remaining_ms: u64 },
    /// Failure-counted eviction: `DEAD_AFTER_CONSECUTIVE_FAILURES`+
    /// consecutive failures with no fresher advertisement since — do
    /// not dial until one arrives (or the retention sweep forgets the
    /// streak after [`QUARANTINE_RETENTION_MS`] idle, the bounded
    /// "second chance" horizon).
    Dead { consecutive_failures: u32 },
}

/// One quarantined endpoint: when it last failed and how long to skip it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct QuarantineEntry {
    /// Wall-clock ms of the most recent failed dial.
    failed_at_ms: u64,
    /// Current skip window. The endpoint is skipped until
    /// `failed_at_ms + backoff_ms`; doubles per consecutive failure up to
    /// [`MAX_BACKOFF_MS`].
    backoff_ms: u64,
    /// Self-healing join: length of the CURRENT consecutive-failure
    /// streak. At [`DEAD_AFTER_CONSECUTIVE_FAILURES`] the endpoint is
    /// evicted from the dial set (see [`DialGate::Dead`]). Cleared with
    /// the entry on success.
    consecutive_failures: u32,
    /// Self-healing join: the peer's endpoint-set freshness stamp
    /// (`StoredPeer::endpoints_advertised_at_ms`) as of the most recent
    /// failure. A CURRENT stamp strictly greater than this means a
    /// fresher advertisement arrived since the streak — the eviction is
    /// lifted for ONE probe (a failure under the fresher stamp re-kills
    /// immediately, so a live-but-unreachable publisher costs at most
    /// one 3s dial per fresh advertisement, not one per tick).
    advert_stamp_ms: u64,
}

impl QuarantineEntry {
    /// Milliseconds left in the backoff window at `now_ms`, or `None` if
    /// the window has elapsed (saturating-sub: a backward clock step
    /// reads as not-yet-elapsed, never as a negative/extended window).
    fn remaining_ms(&self, now_ms: u64) -> Option<u64> {
        let elapsed = now_ms.saturating_sub(self.failed_at_ms);
        self.backoff_ms
            .checked_sub(elapsed)
            .filter(|left| *left > 0)
    }
}

/// In-memory, per-handle quarantine of recently-failed dial endpoints.
#[derive(Debug, Default)]
pub struct DialQuarantine {
    entries: HashMap<QuarantineKey, QuarantineEntry>,
}

impl DialQuarantine {
    /// Milliseconds of backoff remaining for `key` at `now_ms`, or `None`
    /// when the endpoint is not quarantined (never failed, or its window
    /// has elapsed). The dialer surfaces the `Some(remaining)` value on
    /// the discovery snapshot so the skip is visible, not silent.
    pub fn remaining_ms(&self, key: &QuarantineKey, now_ms: u64) -> Option<u64> {
        self.entries.get(key).and_then(|e| e.remaining_ms(now_ms))
    }

    /// The full dial verdict for `key` (self-healing join): DEAD after
    /// [`DEAD_AFTER_CONSECUTIVE_FAILURES`] consecutive failures unless the
    /// endpoint is revived for a probe; otherwise the timed backoff;
    /// otherwise dial.
    ///
    /// A DEAD endpoint is revived for ONE probe when EITHER revival signal
    /// is fresher than this streak's last failure:
    ///
    /// - `current_advert_stamp_ms` STRICTLY greater than the stamp the
    ///   streak accrued under — a fresher endpoint ADVERTISEMENT arrived
    ///   (a rendezvous import rewrote the peer's endpoint set), or
    /// - `proof_of_life_ms` STRICTLY greater than the last failure's
    ///   wall-clock — the peer has been seen ALIVE since we last failed to
    ///   reach it (a fresh presence beacon; `StoredPeer::last_seen_ms`).
    ///
    /// Proof-of-life is the strictly more robust of the two: `last_seen_ms`
    /// advances on EVERY registry import (`airc_trust::touch_last_seen`),
    /// whereas the advert stamp advances only when that same import ALSO
    /// rewrites the endpoint set (`set_endpoints_json`, gated on the beacon
    /// carrying endpoints + the monotonic guard). A provably-live peer whose
    /// endpoint set is unchanged — or whose stamp write is skipped for a
    /// cycle — would otherwise stay evicted FOREVER despite beaconing,
    /// which is exactly the "live peer stuck at 0-connected until a manual
    /// `airc join`" gap (#240). Recognizing proof-of-life closes it while
    /// keeping the eviction's purpose: the anti-noise contract is identical
    /// — a failure under the fresher signal re-kills immediately (see
    /// [`Self::record_failure`], which re-stamps `failed_at_ms` to the probe
    /// instant), so a live-but-unreachable endpoint costs at most one 3s
    /// dial per fresh beacon, never one per tick.
    pub fn gate(
        &self,
        key: &QuarantineKey,
        now_ms: u64,
        current_advert_stamp_ms: u64,
        proof_of_life_ms: u64,
    ) -> DialGate {
        let Some(entry) = self.entries.get(key) else {
            return DialGate::Dial;
        };
        let advert_fresher = current_advert_stamp_ms > entry.advert_stamp_ms;
        let alive_since_failure = proof_of_life_ms > entry.failed_at_ms;
        if entry.consecutive_failures >= DEAD_AFTER_CONSECUTIVE_FAILURES
            && !advert_fresher
            && !alive_since_failure
        {
            return DialGate::Dead {
                consecutive_failures: entry.consecutive_failures,
            };
        }
        match entry.remaining_ms(now_ms) {
            Some(remaining_ms) => DialGate::Backoff { remaining_ms },
            None => DialGate::Dial,
        }
    }

    /// Record a failed dial to `key`: start the backoff at
    /// [`INITIAL_BACKOFF_MS`], or double the existing window (capped at
    /// [`MAX_BACKOFF_MS`]) for a repeat failure; the consecutive-failure
    /// streak grows toward [`DEAD_AFTER_CONSECUTIVE_FAILURES`] and the
    /// entry remembers the freshest advertisement stamp it failed under
    /// (`advert_stamp_ms`, monotonic). Also sweeps entries whose window
    /// has fully elapsed, so the map can't grow unbounded as a
    /// long-lived daemon churns through ephemeral container addresses.
    pub fn record_failure(&mut self, key: QuarantineKey, now_ms: u64, advert_stamp_ms: u64) {
        // Sweep entries not re-failed within the retention horizon. This
        // is INTENTIONALLY keyed off `QUARANTINE_RETENTION_MS`, NOT the
        // (shorter) backoff window — sweeping at window expiry would drop
        // an entry just before a re-failure could double its backoff,
        // resetting a persistently-dead corpse to the initial window every
        // cycle (it would never escalate to the cap).
        self.entries.retain(|_, entry| {
            now_ms.saturating_sub(entry.failed_at_ms) <= QUARANTINE_RETENTION_MS
        });
        let (backoff_ms, consecutive_failures, prev_stamp) = match self.entries.get(&key) {
            Some(prev) => (
                prev.backoff_ms.saturating_mul(2).min(MAX_BACKOFF_MS),
                prev.consecutive_failures.saturating_add(1),
                prev.advert_stamp_ms,
            ),
            None => (INITIAL_BACKOFF_MS, 1, 0),
        };
        self.entries.insert(
            key,
            QuarantineEntry {
                failed_at_ms: now_ms,
                backoff_ms,
                consecutive_failures,
                // Monotonic max: a failure under a FRESHER advertisement
                // re-arms the eviction against that stamp (the one-probe-
                // per-fresh-advertisement contract); a stale caller value
                // must never rewind it.
                advert_stamp_ms: prev_stamp.max(advert_stamp_ms),
            },
        );
    }

    /// A successful dial clears any quarantine for `key` so the endpoint
    /// is immediately eligible again — a peer that flapped and came back
    /// must not stay shadow-banned behind a stale backoff.
    pub fn record_success(&mut self, key: &QuarantineKey) {
        self.entries.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(port: u16) -> QuarantineKey {
        (
            PeerId::from_u128(0xab),
            SocketAddr::from(([10, 0, 0, 2], port)),
        )
    }

    // what this catches: a fresh, never-failed endpoint is NEVER
    // quarantined — the dial loop must always attempt an endpoint it has
    // no failure memory for (the live-endpoint path must not be gated).
    #[test]
    fn unseen_endpoint_is_never_quarantined() {
        let q = DialQuarantine::default();
        assert_eq!(q.remaining_ms(&key(7717), 1_000_000), None);
    }

    // what this catches: a single failure suppresses re-dials for exactly
    // INITIAL_BACKOFF_MS, then the endpoint becomes eligible again. The
    // boundary (== window) must be eligible, not skipped — off-by-one here
    // would either hammer a corpse one tick early or lock it an extra tick.
    #[test]
    fn failure_quarantines_for_initial_window_then_expires() {
        let mut q = DialQuarantine::default();
        let now = 1_000_000;
        q.record_failure(key(7717), now, 0);

        assert!(q.remaining_ms(&key(7717), now).is_some(), "skipped now");
        assert!(
            q.remaining_ms(&key(7717), now + INITIAL_BACKOFF_MS - 1)
                .is_some(),
            "still skipped just inside the window"
        );
        assert_eq!(
            q.remaining_ms(&key(7717), now + INITIAL_BACKOFF_MS),
            None,
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

        q.record_failure(key(7717), now, 0);
        now += INITIAL_BACKOFF_MS;
        q.record_failure(key(7717), now, 0);
        assert!(
            q.remaining_ms(&key(7717), now + INITIAL_BACKOFF_MS + 1)
                .is_some(),
            "second window must exceed the first (doubled)"
        );

        for _ in 0..20 {
            q.record_failure(key(7717), now, 0);
        }
        assert!(q
            .remaining_ms(&key(7717), now + MAX_BACKOFF_MS - 1)
            .is_some());
        assert_eq!(q.remaining_ms(&key(7717), now + MAX_BACKOFF_MS), None);
    }

    // what this catches: a successful dial clears the quarantine so a
    // recovered endpoint is immediately eligible — without this a peer
    // that came back on its old port would stay shadow-banned for the
    // remaining backoff.
    #[test]
    fn success_clears_quarantine_immediately() {
        let mut q = DialQuarantine::default();
        let now = 1_000_000;
        q.record_failure(key(7717), now, 0);
        assert!(q.remaining_ms(&key(7717), now).is_some());

        q.record_success(&key(7717));
        assert_eq!(
            q.remaining_ms(&key(7717), now),
            None,
            "success must lift the quarantine in the same instant"
        );
    }

    // what this catches: quarantine is per (peer, addr) — a dead peer's
    // failure on an address must NOT shadow-ban a DIFFERENT peer that
    // inherited that recycled Docker-bridge address (the live-peer
    // starvation Finding-2). Same port, different peer => clean slate.
    #[test]
    fn recycled_address_under_a_different_peer_is_not_quarantined() {
        let mut q = DialQuarantine::default();
        let now = 1_000_000;
        let addr = SocketAddr::from(([10, 0, 0, 2], 7717));
        let dead_peer = (PeerId::from_u128(0x01), addr);
        let live_peer = (PeerId::from_u128(0x02), addr);

        q.record_failure(dead_peer, now, 0);
        assert!(
            q.remaining_ms(&dead_peer, now).is_some(),
            "dead peer backed off"
        );
        assert_eq!(
            q.remaining_ms(&live_peer, now),
            None,
            "a live peer inheriting the recycled address is NOT shadow-banned"
        );
    }

    // what this catches: a BACKWARD clock step (now < failed_at) must read
    // as "still quarantined", never as a negative/extended or expired
    // window — we don't punish OR reward a peer for our clock jumping back.
    // The doc claims this; pin it so a refactor can't silently break it.
    #[test]
    fn backward_clock_step_does_not_extend_or_expire() {
        let mut q = DialQuarantine::default();
        let failed_at = 1_000_000;
        q.record_failure(key(7717), failed_at, 0);
        let remaining = q.remaining_ms(&key(7717), failed_at - 5_000);
        assert_eq!(
            remaining,
            Some(INITIAL_BACKOFF_MS),
            "saturating-sub: a rewound clock reads zero elapsed, full window remains"
        );
    }

    // what this catches: the unbounded-growth guard sweeps entries not
    // re-failed within QUARANTINE_RETENTION_MS, so a daemon churning
    // through ephemeral addresses doesn't leak QuarantineEntry forever —
    // WITHOUT sweeping so eagerly that it breaks doubling (see
    // doubling_survives_re_failure_across_the_backoff_window).
    #[test]
    fn record_failure_sweeps_entries_past_retention() {
        let mut q = DialQuarantine::default();
        q.record_failure(key(1), 0, 0);
        // A failure long past key(1)'s retention horizon sweeps it.
        q.record_failure(key(2), QUARANTINE_RETENTION_MS + 1, 0);
        assert_eq!(
            q.entries.len(),
            1,
            "key(1) past retention swept; only key(2) remains"
        );
        assert!(q.entries.contains_key(&key(2)));
    }

    // what this catches (self-healing join — failure-counted eviction,
    // the 172.x ghost swarm): the 5th consecutive failure marks the
    // endpoint DEAD — skipped even after every timed backoff window has
    // elapsed — and ONLY a strictly-fresher advertisement stamp revives
    // it, for exactly one probe (a failure under the fresher stamp
    // re-kills immediately). Mutation checks: dropping the dead arm from
    // `gate` fails the "still dead after backoff expiry" assert; using
    // `<` instead of `<=` on the stamp comparison fails the same-stamp
    // assert; dropping the monotonic stamp max in `record_failure` fails
    // the re-kill assert.
    #[test]
    fn fifth_consecutive_failure_is_dead_until_a_fresher_advertisement() {
        let mut q = DialQuarantine::default();
        let stamp = 1_000u64;
        let mut now = 0u64;
        for _ in 0..DEAD_AFTER_CONSECUTIVE_FAILURES {
            q.record_failure(key(7717), now, stamp);
            now += MAX_BACKOFF_MS + 1;
        }

        // Every backoff window has elapsed — a timed quarantine would
        // dial again. Failure-counted eviction must NOT. `proof_of_life=0`
        // here means "peer not seen alive since the streak" — isolates the
        // advertisement-revival axis from proof-of-life revival.
        assert!(
            matches!(
                q.gate(&key(7717), now, stamp, 0),
                DialGate::Dead {
                    consecutive_failures
                } if consecutive_failures == DEAD_AFTER_CONSECUTIVE_FAILURES
            ),
            "5 consecutive failures with no fresher advertisement = dead, \
             even after the backoff expired"
        );

        // Four failures is NOT dead (streak below the threshold).
        let mut four = DialQuarantine::default();
        for i in 0..(DEAD_AFTER_CONSECUTIVE_FAILURES - 1) {
            four.record_failure(key(1), u64::from(i), stamp);
        }
        assert!(
            !matches!(four.gate(&key(1), now, stamp, 0), DialGate::Dead { .. }),
            "below the threshold the timed backoff still governs"
        );

        // A STRICTLY fresher advertisement revives the endpoint for a probe.
        assert_eq!(
            q.gate(&key(7717), now, stamp + 1, 0),
            DialGate::Dial,
            "a fresher advertisement must lift the eviction for one probe"
        );
        // ...and a failure under that fresher stamp re-kills immediately
        // (one probe per fresh advertisement, not one per tick).
        q.record_failure(key(7717), now, stamp + 1);
        assert!(
            matches!(
                q.gate(&key(7717), now + MAX_BACKOFF_MS + 1, stamp + 1, 0),
                DialGate::Dead { .. }
            ),
            "a failure under the fresher stamp re-arms the eviction against it"
        );

        // A success wipes the streak entirely.
        q.record_success(&key(7717));
        assert_eq!(q.gate(&key(7717), now, stamp, 0), DialGate::Dial);
    }

    // what this catches (#240 — live-beaconing peer stuck at 0-connected):
    // a DEAD endpoint whose peer is PROVABLY ALIVE (a presence beacon seen
    // AFTER the streak's last failure — `proof_of_life_ms > failed_at_ms`)
    // is revived for one probe EVEN WITH NO fresher endpoint advertisement.
    // This is the robustness gap: `last_seen_ms` advances on every registry
    // import while the advert stamp can stay frozen (unchanged endpoint set
    // / skipped stamp write), so without this a beaconing peer stays evicted
    // forever and needs a manual `airc join`. Mutation checks: dropping the
    // `alive_since_failure` term fails the revival assert; comparing against
    // anything other than `failed_at_ms` breaks the re-kill assert below.
    #[test]
    fn fresh_proof_of_life_revives_a_dead_endpoint_without_a_new_advertisement() {
        let mut q = DialQuarantine::default();
        let stamp = 1_000u64;
        // Fail 5×, most recent failure at `last_failure_at`. The advertised
        // stamp NEVER advances (the frozen-endpoint-set condition), so the
        // advert-revival axis stays inert throughout this test.
        let mut now = 0u64;
        let mut last_failure_at = 0u64;
        for _ in 0..DEAD_AFTER_CONSECUTIVE_FAILURES {
            last_failure_at = now;
            q.record_failure(key(7717), now, stamp);
            now += MAX_BACKOFF_MS + 1;
        }

        // Same stamp + no proof of life (last_seen at/behind the failure) =>
        // still DEAD, exactly as before this fix.
        assert!(
            matches!(
                q.gate(&key(7717), now, stamp, last_failure_at),
                DialGate::Dead { .. }
            ),
            "a stale-or-equal proof-of-life must NOT revive (last_seen == failed_at)"
        );

        // A presence beacon seen AFTER the last failure revives for one probe
        // — with the SAME advertisement stamp (no rendezvous re-read needed).
        assert_eq!(
            q.gate(&key(7717), now, stamp, last_failure_at + 1),
            DialGate::Dial,
            "fresh proof-of-life must lift the eviction for one probe, no new advert"
        );

        // The probe fails again: re-stamps `failed_at_ms` to the probe
        // instant, so the SAME proof-of-life no longer counts as "since the
        // failure" — one probe per fresh beacon, not one per tick.
        let probe_at = now;
        q.record_failure(key(7717), probe_at, stamp);
        assert!(
            matches!(
                q.gate(
                    &key(7717),
                    probe_at + MAX_BACKOFF_MS + 1,
                    stamp,
                    last_failure_at + 1
                ),
                DialGate::Dead { .. }
            ),
            "the beacon that already bought a probe must not buy another (re-killed)"
        );

        // A newer beacon (after the probe failure) buys the next probe.
        assert_eq!(
            q.gate(
                &key(7717),
                probe_at + MAX_BACKOFF_MS + 1,
                stamp,
                probe_at + 1
            ),
            DialGate::Dial,
            "a beacon fresher than the probe failure revives again"
        );
    }

    // what this catches: doubling must SURVIVE a re-failure that lands
    // after the backoff window elapsed but within the retention horizon —
    // the sweep must not drop the entry there, or a persistently-dead
    // corpse re-dialed each cadence would reset to INITIAL forever and
    // never escalate to the cap (the regression the eager sweep caused).
    #[test]
    fn doubling_survives_re_failure_across_the_backoff_window() {
        let mut q = DialQuarantine::default();
        q.record_failure(key(7717), 0, 0);
        // Re-fail AFTER the initial window elapsed (INITIAL+1) but well
        // within retention — must double, not reset.
        let t = INITIAL_BACKOFF_MS + 1;
        q.record_failure(key(7717), t, 0);
        assert!(
            q.remaining_ms(&key(7717), t + INITIAL_BACKOFF_MS + 1)
                .is_some(),
            "backoff doubled across the window boundary (not reset to INITIAL)"
        );
    }
}
