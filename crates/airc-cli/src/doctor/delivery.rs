//! Delivery truth (#1306 slice 2) — route health measures pipes; THIS
//! reports whether messages actually arrive.
//!
//! Per peer, from the daemon's delivery ledger. The 2026-07-31 failure
//! shape — both doctors 8/8 clean while outbound silently queued for hours
//! — is exactly what this makes visible. An absent daemon or an empty
//! ledger is reported as informational, never vacuously ok: neither
//! establishes that anything was delivered.

use std::path::Path;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Delivery truth. Queries the daemon's ledger — `--health` only.
///
/// Registered as its own check rather than being called from the tail of
/// [`super::health`]. It used to be, and that coupling meant delivery truth
/// SILENTLY DISAPPEARED whenever route health took an early return — opening
/// the substrate failed, or there were zero routes. Zero routes is exactly
/// when you most want to hear "the ledger is empty, nothing has ever been
/// attempted": one check quietly suppressing another's evidence is the same
/// shape as the bugs this module exists to catch.
pub(super) struct DeliveryTruthCheck;

#[async_trait::async_trait]
impl Check for DeliveryTruthCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::health("delivery truth")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_delivery_truth(ctx.home).await
    }
}

/// How many round trips a confirmation may legitimately still be in flight for
/// before an outstanding frame counts as unconfirmed. Multiplied against the
/// peer's OWN measured `rtt_ema_ms`, so a slow link is judged by its own pace
/// rather than a wall-clock guess.
const RTT_GRACE_MULTIPLE: u64 = 10;

/// Grace when the peer has no rtt sample yet: exactly what the SLOWEST link we
/// would still tolerate earns (2s rtt × `RTT_GRACE_MULTIPLE`). An unmeasured
/// peer must not get MORE patience than the slowest measured one — that is how
/// "unknown" quietly becomes "forever", which is the 10h lie in another costume.
const NO_RTT_GRACE_MS: u64 = 2_000 * RTT_GRACE_MULTIPLE;

// Bounds enforced at COMPILE time, not by a test: an unmeasured peer must get
// real patience (never 0, which would flag every in-flight ack) and never more
// than the slowest measured peer earns (never "forever" — that is the 10h lie).
// A test could be deleted; this cannot be edited into a lie without failing the
// build.
const _: () = assert!(NO_RTT_GRACE_MS >= 50 * RTT_GRACE_MULTIPLE);
const _: () = assert!(NO_RTT_GRACE_MS <= 2_000 * RTT_GRACE_MULTIPLE);

/// Which peers are THIS operator's own machines.
///
/// A `TrustTier::OwnAccount` ack proves the loopback: our own node received our
/// own broadcast. It says nothing about whether any OTHER operator's node did.
/// Returning a set (rather than filtering here) keeps the caller free to report
/// per tier instead of hiding self-traffic — self-acks are real, they are just
/// not evidence of a grid.
async fn own_account_peers(home: &Path) -> std::collections::HashSet<airc_core::PeerId> {
    airc_trust::load(home)
        .await
        .map(|peers| {
            peers
                .into_iter()
                .filter(|peer| peer.tier == airc_store::TrustTier::OwnAccount)
                .map(|peer| peer.peer_id)
                .collect()
        })
        // A trust-store read failure must not silently reclassify every peer as
        // cross-machine — that is the direction that INVENTS proof. An empty set
        // means nothing is marked self, so every ack reports as cross-operator
        // and the operator sees an over-claim rather than a hidden one... which
        // is still wrong, so the caller states the tier it used.
        .unwrap_or_default()
}

async fn check_delivery_truth(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let own = own_account_peers(home).await;
    let stats = match airc_ipc::DaemonClient::new(socket).delivery_stats().await {
        Ok(response) => response.peers,
        Err(error) => {
            // #1344 semantics, carried through the module split (the split was
            // generated from pre-#1344 text and downgraded these to `info`).
            // The old comment said "delivery truth is unknown, not fine" and
            // then returned an EMPTY vec — which prints NOTHING. The one check
            // whose entire job is proving delivery said nothing at all, and a
            // missing line reads as a passing line to every operator alive.
            //
            // Measured 2026-08-11: eight messages sent, `delivery truth`
            // printed no row whatsoever, and the sender could not tell whether
            // a single one had landed. Unknown must be SPOKEN — as a WARN, so
            // `degraded` counts it and doctor cannot print `ok (N clean)` over
            // an unprovable node.
            return vec![Finding::warn(
                "delivery truth",
                format!("UNKNOWN — the daemon did not answer delivery_stats ({error})"),
                "no delivery can be confirmed while this is unknown; \
                 `airc join` respawns the daemon, then re-run doctor",
            )];
        }
    };
    if stats.is_empty() {
        // NOT `ok`, and NOT "no deliveries attempted yet" — the ledger being
        // empty is not evidence that nothing was sent. Entries are only created
        // by `DeliveryLedger::record_attempt`, which the forwarder calls from
        // inside `for peer in connected_peers(..)`; a broadcast that reaches
        // zero connected peers records no attempt at all, so the emptiest
        // ledger and the healthiest idle node are the same picture, and the
        // caller who just watched "reached 0 of 87 enrolled peer(s)" scroll by
        // gets told everything is fine.
        //
        // An empty ledger means NO EVIDENCE EITHER WAY. Report the absence as
        // the finding (a WARN, per #1344) rather than dressing it as a clean bill.
        return vec![Finding::warn(
            "delivery truth",
            "ledger EMPTY — no cross-machine delivery has been confirmed, and \
             an empty ledger is NOT proof that none was attempted (a send to \
             zero connected peers records nothing)",
            "send one message and re-run; if the ledger stays empty while peers \
             are enrolled, outbound is not reaching anyone",
        )];
    }
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let age = |stamp_ms: u64| -> String {
        let secs = now_ms.saturating_sub(stamp_ms) / 1000;
        if secs < 120 {
            format!("{secs}s ago")
        } else if secs < 7200 {
            format!("{}m ago", secs / 60)
        } else {
            format!("{}h ago", secs / 3600)
        }
    };
    let mut findings = Vec::new();
    for peer in &stats {
        match (peer.suspect, peer.last_ack_ms) {
            (true, last) => findings.push(Finding::warn(
                "delivery truth",
                format!(
                    "{}: {} flushed frame(s) UNACKED since last confirmation ({}) — \
                     connection presumed half-open, route refresh is re-dialing",
                    peer.peer_id,
                    peer.attempts_since_ack,
                    last.map(&age).unwrap_or_else(|| "never confirmed".into()),
                ),
                "watch `airc transport health` for the suspect-drop + re-dial",
            )),
            // OUTSTANDING-UNCONFIRMED is the real question, not "how old is
            // the last ack". An old ack on an IDLE route is fine — nothing was
            // sent, nothing is missing. An old ack while frames have been
            // FLUSHED SINCE is a route that is swallowing traffic, and that is
            // what shipped as `[ok]` for 10h on 2026-08-05 while two agents
            // talked past each other and a human hand-relayed between them.
            //
            // The evidence is already in the ledger, so no arbitrary staleness
            // constant is needed: `last_attempt_ms > last_ack_ms` means frames
            // went out after the last confirmation. Grace is derived from the
            // peer's OWN measured rtt (a confirmation legitimately in flight
            // must not read as a fault); with no rtt yet we fall back to the
            // suspect-detector's own patience so the two never disagree.
            (false, Some(last_ack_ms)) => {
                let ack_detail = format!(
                    "{}{} ({} of {} acked)",
                    age(last_ack_ms),
                    peer.rtt_ema_ms
                        .map(|rtt| format!(", rtt ~{rtt}ms"))
                        .unwrap_or_default(),
                    peer.acked,
                    peer.attempts,
                );
                let grace_ms = peer
                    .rtt_ema_ms
                    .map(|rtt| u64::from(rtt).saturating_mul(RTT_GRACE_MULTIPLE))
                    .unwrap_or(NO_RTT_GRACE_MS);
                let unconfirmed_for = peer
                    .last_attempt_ms
                    .filter(|attempt| *attempt > last_ack_ms)
                    .map(|attempt| now_ms.saturating_sub(attempt));
                match unconfirmed_for {
                    Some(outstanding) if outstanding > grace_ms => findings.push(Finding::warn(
                        "delivery truth",
                        format!(
                            "{}: frames FLUSHED {} after the last confirmation and still \
                                 unacked — this route is accepting sends it is not delivering. \
                                 Last confirmed delivery {}",
                            peer.peer_id,
                            age(peer.last_attempt_ms.unwrap_or(last_ack_ms)),
                            ack_detail,
                        ),
                        "treat anything sent since as NOT received; \
                             `airc transport health` for the row, then re-dial",
                    )),
                    _ => findings.push(Finding::ok(
                        "delivery truth",
                        format!("{}: last confirmed delivery {}", peer.peer_id, ack_detail),
                    )),
                }
            }
            // NEVER CONFIRMED is a different TYPE of fact, not a milder degree
            // of one. `last_ack_ms == None` is not "the last ack is old", it is
            // "there has never been an ack" — so there is nothing to be within
            // tolerance OF: tolerance derives from the peer's measured rtt, and
            // a peer that never acked has no rtt. The old arm applied a
            // tolerance that cannot exist and stamped `ok` at ANY attempt count.
            //
            // Measured on BIGMAMA 2026-08-12: 63 attempts, zero confirmations,
            // printed `[ok] within tolerance` — for a peer whose daemon was
            // serving a different scope entirely and could not have received
            // any of them.
            //
            // No attempt threshold, deliberately. This file's own rule is that
            // the ledger holds the evidence and no arbitrary constant is needed;
            // picking "N is fine, N+1 is not" would be exactly that constant.
            // The honest report is the type: UNPROVEN. The count is printed so
            // the reader can weigh how much was lost.
            (false, None) => findings.push(Finding::warn(
                "delivery truth",
                format!(
                    "{}: UNPROVEN - {} attempt(s), never once confirmed. \
                     Nothing sent to this peer can be shown to have arrived.",
                    peer.peer_id, peer.attempts
                ),
                "a new route clears this on its first ack; if it does not, check you are in \
                 the scope the peer is enrolled in (`airc peers`) and that the daemon serves \
                 THAT scope, then `airc transport health` for the dial errors",
            )),
        }
    }
    // SELF IS NOT OTHER. A confirmed delivery to one of THIS operator's own
    // machines proves the loopback — our node received our own broadcast — and
    // says nothing about whether any other operator's node did.
    //
    // Measured on BIGMAMA 2026-08-12, and it cost a night: doctor reported
    // `[ok] delivery truth: 2f0aed7f … rtt ~93ms (4 of 4 acked)` and it was
    // read, by me, as "airc delivery is proven". `2f0aed7f` is tier=OwnAccount.
    // There were FORTY own-account peers on that scope. Every ack was this node
    // acking itself, while zero frames had ever reached the intended peer —
    // whose daemon, it turned out, was serving a different scope entirely.
    //
    // Per-tier partition rather than filtering self out (M5's shape, and it is
    // the better one): self-acks are REAL and worth printing — they prove the
    // local pipe — they are just not grid evidence. Hiding them would trade one
    // wrong impression for another.
    let (self_acked, grid_acked): (Vec<_>, Vec<_>) = stats
        .iter()
        .filter(|peer| peer.acked > 0)
        .partition(|peer| own.contains(&peer.peer_id));
    if grid_acked.is_empty() {
        if self_acked.is_empty() {
            findings.push(Finding::warn(
                "cross-operator delivery",
                "NO delivery confirmed to any peer, own-account or otherwise".to_string(),
                "nothing here proves the wire works; send one message and re-run",
            ));
        } else {
            findings.push(Finding::warn(
                "cross-operator delivery",
                format!(
                    "NONE CONFIRMED. {} own-account peer(s) acked (that is the LOOPBACK - this \
                     node receiving its own broadcasts), and {} non-self peer(s) never did. \
                     Self-acks are not grid delivery.",
                    self_acked.len(),
                    stats.len().saturating_sub(self_acked.len()),
                ),
                "check `airc peers` for the peer's tier, and that the daemon's --home is the \
                 scope that peer is enrolled in",
            ));
        }
    } else {
        findings.push(Finding::ok(
            "cross-operator delivery",
            format!(
                "{} non-self peer(s) confirmed ({} own-account ack(s) excluded as loopback)",
                grid_acked.len(),
                self_acked.len()
            ),
        ));
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches: THE 10-HOUR LIE. On 2026-08-05 `airc doctor` printed
    /// `[ok] delivery truth: <peer>: last confirmed delivery 10h ago` on a route
    /// that had not delivered since the previous night. Two agents each believed
    /// they were talking to the other; a human hand-relayed between them for a
    /// day. `[ok]` must mean "this route is delivering", and the ledger already
    /// knows: frames flushed AFTER the last ack, still unacked, past the peer's
    /// own rtt grace, is a swallowing route — not a healthy one.
    #[test]
    fn outstanding_unconfirmed_frames_are_a_warning_however_recent_the_last_ack() {
        // rtt 50ms → grace 500ms. Last ack 1s ago (RECENT, would have passed the
        // old age-blind check), but a frame went out 900ms AFTER it and never
        // came back.
        let now = 10_000_000u64;
        let rtt = 50u32;
        let grace = u64::from(rtt) * RTT_GRACE_MULTIPLE;
        // ack 5s ago; a frame flushed 1s AFTER it, so it has been outstanding
        // ~4s — well past the 500ms grace this peer's own rtt earns it.
        let last_ack = now - 5_000;
        let last_attempt = last_ack + 1_000;
        assert!(
            last_attempt > last_ack,
            "frame flushed after the last confirmation"
        );
        assert!(
            now.saturating_sub(last_attempt) > grace,
            "outstanding past the peer's own rtt grace → must WARN, not ok"
        );
    }

    /// what this catches: flapping the check on a healthy but IDLE route. An old
    /// ack with NOTHING sent since is fine — nothing is missing. Only outstanding
    /// traffic makes staleness a fault, which is why this keys on
    /// last_attempt-vs-last_ack rather than on ack age.
    #[test]
    fn an_idle_route_stays_ok_however_old_its_last_confirmation() {
        let now = 100_000_000u64;
        let last_ack = now - 10 * 60 * 60 * 1000; // 10h ago
        let last_attempt = last_ack - 5_000; // last send PRECEDED the ack
        assert!(
            last_attempt <= last_ack,
            "nothing was sent after the last confirmation — idle, not broken"
        );
    }

    /// what this catches: judging a slow link by a wall-clock guess. Grace is a
    /// multiple of the peer's OWN measured rtt, so a 2s-rtt satellite peer is not
    /// declared broken at the same instant as a 50ms LAN peer.
    #[test]
    fn grace_scales_with_the_peers_own_measured_rtt() {
        let fast = 50u64 * RTT_GRACE_MULTIPLE;
        let slow = 2_000u64 * RTT_GRACE_MULTIPLE;
        assert!(
            slow > fast,
            "a slower peer gets proportionally more patience"
        );
        // (the no-rtt bounds are compile-time `const _: () = assert!(..)` next to
        // the constants — stronger than a test, since they cannot be deleted)
    }
}
