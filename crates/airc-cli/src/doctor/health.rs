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
    if degraded == 0 {
        vec![Finding::ok(
            "route health",
            format!("{total} route(s) healthy"),
        )]
    } else {
        vec![Finding::warn(
            "route health",
            format!("{degraded} of {total} route(s) degraded"),
            "run `airc transport health` to see the row-level detail",
        )]
    }
}
