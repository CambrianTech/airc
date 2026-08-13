//! Route / transport health (under `--health`) — measures the PIPES.
//!
//! Pairs with [`super::delivery`], which measures whether anything
//! actually arrives through them. Both are needed: healthy pipes with no
//! confirmed deliveries is precisely the failure shape that hid for 10
//! hours on 2026-08-05.

use std::path::Path;

use super::{Check, CheckConfig, CheckContext, Finding};

/// Route/transport health. Re-runs discovery and dials peers — expensive, so
/// `--health` only.
pub(super) struct RouteHealthCheck;

#[async_trait::async_trait]
impl Check for RouteHealthCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::health("route health")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_health(ctx.home).await
    }
}

async fn check_health(home: &Path) -> Vec<Finding> {
    use airc_lib::{Airc, TransportHealthState};

    let airc = match Airc::open(home).await {
        Ok(airc) => airc,
        Err(error) => {
            return vec![Finding::blocked(
                "route health",
                format!("can't open substrate: {error}"),
                "address the identity/store errors above first",
            )];
        }
    };
    let snapshot = match airc.refresh_route_discovery().await {
        Ok(s) => s,
        Err(error) => {
            return vec![Finding::warn(
                "route health",
                format!("route refresh failed: {error}"),
                "run `airc transport health` for the underlying detail",
            )];
        }
    };
    let total = snapshot.health.len();
    // #267: zero routes is NOT vacuously healthy. `degraded == 0` was true
    // for an EMPTY health list, so doctor stamped "[ok] 0 route(s) healthy"
    // while every remote peer was unreachable — the exact lie that hid a
    // dead mesh behind a green check. With remote peers enrolled, no routes
    // means beyond-this-machine delivery is DOWN: say so, loudly.
    if total == 0 {
        let enrolled = airc.peers().await.map(|peers| peers.len()).unwrap_or(0);
        if enrolled > 0 {
            return vec![Finding::warn(
                "route health",
                format!("0 routes with {enrolled} enrolled peer(s) — remote delivery is DOWN"),
                "run `airc transport health` for the dial errors; `airc join` re-runs discovery",
            )];
        }
        return vec![Finding::ok(
            "route health",
            "0 routes (no remote peers enrolled — nothing to route to)",
        )];
    }
    let degraded = snapshot
        .health
        .iter()
        .filter(|sample| sample.state != TransportHealthState::Healthy)
        .count();
    // UNMEASURED IS NOT HEALTHY.
    //
    // `TransportHealthState::Healthy` with `rtt_ms: None` and
    // `success_ppm: None` means "optimistically marked usable, never actually
    // exercised" — `airc transport health` prints it honestly as
    // `state=healthy (not measured)`. Doctor counted those rows toward
    // "N route(s) healthy", so a route that had never carried a single frame
    // reported as proof the wire works.
    //
    // Measured on BIGMAMA 2026-08-12: `[ok] route health: 1 route(s) healthy`
    // on a lan-tcp row with no rtt and no success rate, while every frame this
    // node sent went to a peer it could not reach. The count was true and the
    // conclusion it invited was false.
    //
    // Same family as the delivery-truth self-ack fix in this branch, one layer
    // up: there, SELF was rendered as OTHER; here, UNMEASURED is rendered as
    // MEASURED. Both let a green board describe a dead wire.
    //
    // Not downgraded to a warning on its own — an unmeasured route is not a
    // fault, it is an unknown, and a fresh route legitimately starts here. It
    // is simply not evidence, so it is counted and named separately.
    let unmeasured = snapshot
        .health
        .iter()
        .filter(|sample| {
            sample.state == TransportHealthState::Healthy
                && sample.rtt_ms.is_none()
                && sample.success_ppm.is_none()
        })
        .count();
    if degraded == 0 && unmeasured == total {
        vec![Finding::warn(
            "route health",
            format!(
                "{total} route(s) present but NONE MEASURED - no rtt, no success rate, \
                 so nothing here shows a frame has ever crossed"
            ),
            "send one message and re-run; a route that stays unmeasured while peers are \
             enrolled is not carrying traffic",
        )]
    } else if degraded == 0 {
        vec![Finding::ok(
            "route health",
            if unmeasured > 0 {
                format!("{total} route(s) healthy ({unmeasured} not yet measured)")
            } else {
                format!("{total} route(s) healthy")
            },
        )]
    } else {
        vec![Finding::warn(
            "route health",
            format!("{degraded} of {total} route(s) degraded"),
            "run `airc transport health` to see the row-level detail",
        )]
    }
}
