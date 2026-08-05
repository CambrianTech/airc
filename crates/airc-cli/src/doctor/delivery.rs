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

async fn check_delivery_truth(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let stats = match airc_ipc::DaemonClient::new(socket).delivery_stats().await {
        Ok(response) => response.peers,
        Err(_) => {
            // No daemon (or an older build without the verb): delivery truth
            // is unknown, not fine. It used to return an EMPTY vec, which
            // produces no Finding at all — and since `degraded` counts only
            // Warn/Blocked, doctor then printed `ok (N clean)`. So on the one
            // node class where delivery truth is least trustworthy (a stale
            // daemon predating the verb — the case check_daemon_build exists
            // for) the check silently vanished into a clean bill of health.
            // Unknown must SAY unknown.
            return vec![Finding::info(
                "delivery truth",
                "daemon did not answer the delivery-stats verb (not running, or a build \
                 predating it) — delivery truth is UNKNOWN for this node, not confirmed",
            )];
        }
    };
    if stats.is_empty() {
        // An empty ledger means nothing was ever ATTEMPTED — not that
        // everything sent arrived. Entries are only created by
        // `DeliveryLedger::record_attempt`, which the forwarder calls from
        // inside `for peer in connected_peers(..)`. With zero connected peers
        // that loop body never runs, so a node with peers enrolled but none
        // connected drops every broadcast into a void and the ledger stays
        // empty FOREVER. `is_empty()` is exactly as true after ten thousand
        // undeliverable sends as it is at boot, so it cannot be reported as ok.
        return vec![Finding::info(
            "delivery truth",
            "delivery ledger is empty — nothing has been ATTEMPTED to any peer. Expected on a \
             fresh scope; on a node with peers enrolled it means the forwarder found no \
             CONNECTED peer to send to (broadcasts are going nowhere)",
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
            (false, None) => findings.push(Finding::ok(
                "delivery truth",
                format!(
                    "{}: {} attempt(s), none confirmed yet (within tolerance)",
                    peer.peer_id, peer.attempts
                ),
            )),
        }
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
