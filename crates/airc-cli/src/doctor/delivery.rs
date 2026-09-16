//! Delivery truth (#1306 slice 2) — route health measures pipes; THIS
//! reports whether messages actually arrive.
//!
//! Per peer, from the daemon's delivery ledger. The 2026-07-31 failure
//! shape — both doctors 8/8 clean while outbound silently queued for hours
//! — is exactly what this makes visible. An absent daemon or an empty
//! ledger is reported as a warning, never vacuously ok: neither
//! establishes that anything was delivered.

use std::path::Path;

use airc_ipc::IpcPeerDeliveryStats;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Delivery truth. Queries the daemon's ledger — `--health` only.
///
/// Registered as its own check rather than being called from the tail of
/// [`super::health`]. It used to be, and that coupling meant delivery truth
/// disappeared whenever route health took an early return. Route availability
/// and the delivery ledger are separate observations; neither check should
/// suppress the other's evidence.
pub(super) struct DeliveryTruthCheck;

#[async_trait::async_trait]
impl Check for DeliveryTruthCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::health("delivery truth")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_delivery_truth(ctx).await
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
/// `OwnAccount` describes account ownership, not physical placement. Another
/// machine on the same account can acknowledge delivery; do not call it loopback.
async fn own_account_peers(
    home: &Path,
) -> Result<std::collections::HashSet<airc_core::PeerId>, String> {
    airc_trust::load(home)
        .await
        .map(|peers| {
            peers
                .into_iter()
                .filter(|peer| peer.tier == airc_store::TrustTier::OwnAccount)
                .map(|peer| peer.peer_id)
                .collect()
        })
        .map_err(|error| error.to_string())
}

async fn check_delivery_truth(ctx: &CheckContext<'_>) -> Vec<Finding> {
    let snapshot = match ctx.delivery_stats().await {
        Ok(response) => response,
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
                format!("UNAVAILABLE — the daemon did not answer delivery_stats ({error})"),
                "check daemon status; no delivery can be confirmed from this observation",
            )];
        }
    };
    let Some(now) = super::health::now_ms() else {
        return vec![Finding::warn(
            "delivery truth",
            "UNKNOWN — local clock cannot date the daemon snapshot",
            "check the local clock before interpreting delivery timestamps",
        )];
    };
    if let Err(detail) = super::health::snapshot_age(snapshot, Some(now)) {
        return vec![Finding::warn(
            "delivery truth",
            detail,
            "check daemon build and route-refresh diagnostics; historical acknowledgements cannot establish current delivery",
        )];
    }
    let own = own_account_peers(ctx.home).await;
    delivery_findings(&snapshot.peers, own.as_ref().map_err(String::as_str), now)
}

fn delivery_findings(
    stats: &[IpcPeerDeliveryStats],
    own: Result<&std::collections::HashSet<airc_core::PeerId>, &str>,
    now_ms: u64,
) -> Vec<Finding> {
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
            "ledger EMPTY — no peer delivery has been confirmed in this snapshot, and \
             an empty ledger is NOT proof that none was attempted (a send to \
             zero connected peers records nothing)",
            "check the intended room and peer acknowledgement; an empty snapshot does not prove a transport failure",
        )];
    }
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
    for peer in stats {
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
    // Account ownership is not a physical topology test. Keep every peer's
    // receipt above, and describe the trust partition without inventing locality.
    match own {
        Ok(own) => {
            let (same_account, not_tagged) = stats.iter().filter(|peer| peer.acked > 0).fold(
                (0, 0),
                |(same, other), peer| {
                    if own.contains(&peer.peer_id) {
                        (same + 1, other)
                    } else {
                        (same, other + 1)
                    }
                },
            );
            findings.push(Finding::info(
                "delivery account scope",
                format!(
                    "{same_account} OwnAccount peer(s) and {not_tagged} peer(s) not tagged OwnAccount in this scope have acknowledged; OwnAccount does not imply physical loopback, and a daemon ACK does not prove the intended reader consumed a message"
                ),
            ));
        }
        Err(error) => findings.push(Finding::warn(
            "delivery account scope",
            format!("UNKNOWN — trust store could not classify peer acknowledgements ({error})"),
            "inspect the trust-store read error; peer receipts remain valid but their account scope is unknown",
        )),
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
        let now = 10_000_000u64;
        let peer = IpcPeerDeliveryStats {
            peer_id: airc_core::PeerId::from_u128(1),
            attempts: 2,
            acked: 1,
            attempts_since_ack: 1,
            last_attempt_ms: Some(now - 4_000),
            last_ack_ms: Some(now - 5_000),
            rtt_ema_ms: Some(50),
            suspect: false,
        };
        let findings = delivery_findings(&[peer], Ok(&Default::default()), now);
        assert_eq!(findings[0].status, super::super::Status::Warn);
        assert!(findings[0].detail.contains("still unacked"));
    }

    /// what this catches: flapping the check on a healthy but IDLE route. An old
    /// ack with NOTHING sent since is fine — nothing is missing. Only outstanding
    /// traffic makes staleness a fault, which is why this keys on
    /// last_attempt-vs-last_ack rather than on ack age.
    #[test]
    fn an_idle_route_stays_ok_however_old_its_last_confirmation() {
        let now = 100_000_000u64;
        let last_ack = now - 10 * 60 * 60 * 1000; // 10h ago
        let peer = IpcPeerDeliveryStats {
            peer_id: airc_core::PeerId::from_u128(1),
            attempts: 1,
            acked: 1,
            attempts_since_ack: 0,
            last_attempt_ms: Some(last_ack - 5_000),
            last_ack_ms: Some(last_ack),
            rtt_ema_ms: Some(50),
            suspect: false,
        };
        let findings = delivery_findings(&[peer], Ok(&Default::default()), now);
        assert_eq!(findings[0].status, super::super::Status::Ok);
        assert!(findings[0].detail.contains("last confirmed delivery"));
    }

    /// what this catches: judging a slow link by a wall-clock guess. Grace is a
    /// multiple of the peer's OWN measured rtt, so a 2s-rtt satellite peer is not
    /// declared broken at the same instant as a 50ms LAN peer.
    #[test]
    fn grace_scales_with_the_peers_own_measured_rtt() {
        let now = 10_000;
        let mut peer = IpcPeerDeliveryStats {
            peer_id: airc_core::PeerId::from_u128(1),
            attempts: 2,
            acked: 1,
            attempts_since_ack: 1,
            last_attempt_ms: Some(now - 1_000),
            last_ack_ms: Some(now - 2_000),
            rtt_ema_ms: Some(50),
            suspect: false,
        };
        let fast = delivery_findings(&[peer.clone()], Ok(&Default::default()), now);
        peer.rtt_ema_ms = Some(2_000);
        let slow = delivery_findings(&[peer], Ok(&Default::default()), now);
        assert_eq!(fast[0].status, super::super::Status::Warn);
        assert_eq!(slow[0].status, super::super::Status::Ok);
    }

    // c7873cba: an OwnAccount ACK is retained; a trust-store read failure must
    // not reclassify it as another operator or physical loopback.
    #[test]
    fn account_scope_does_not_invent_physical_loopback() {
        let peer = IpcPeerDeliveryStats {
            peer_id: airc_core::PeerId::from_u128(1),
            attempts: 1,
            acked: 1,
            attempts_since_ack: 0,
            last_attempt_ms: Some(900),
            last_ack_ms: Some(950),
            rtt_ema_ms: Some(50),
            suspect: false,
        };
        let own = std::collections::HashSet::from([peer.peer_id]);
        let findings = delivery_findings(std::slice::from_ref(&peer), Ok(&own), 1_000);
        assert_eq!(findings[0].status, super::super::Status::Ok);
        assert_eq!(findings[1].status, super::super::Status::Info);
        assert!(findings[1].detail.contains("1 OwnAccount peer(s)"));
        assert!(findings[1]
            .detail
            .contains("does not imply physical loopback"));

        let findings = delivery_findings(&[peer], Err("trust read failed"), 1_000);
        assert_eq!(findings[0].status, super::super::Status::Ok);
        assert_eq!(findings[1].status, super::super::Status::Warn);
        assert!(findings[1].detail.contains("trust read failed"));
    }
}
