//! Route availability from the daemon's delivery snapshot. A new local Airc
//! handle has no daemon ledger and cannot report the running transport's health.

use airc_ipc::DeliveryStatsResponse;

use super::{Check, CheckConfig, CheckContext, Finding};

pub(super) struct RouteHealthCheck;

#[async_trait::async_trait]
impl Check for RouteHealthCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::health("route health")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        vec![route_finding(ctx.delivery_stats().await, now_ms())]
    }
}

pub(super) fn now_ms() -> Option<u64> {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .map(|elapsed| elapsed.as_millis() as u64)
}

/// Allow an in-flight refresh before declaring an observation stale. This is
/// diagnostic freshness, not a new transport timeout or refresh schedule.
pub(super) fn snapshot_age(
    snapshot: &DeliveryStatsResponse,
    now_ms: Option<u64>,
) -> Result<u64, String> {
    let sampled_at = snapshot.sampled_at_ms.ok_or_else(|| {
        "UNAVAILABLE — daemon has not supplied a timestamped delivery snapshot (first refresh pending or older daemon)".to_string()
    })?;
    let now = now_ms
        .ok_or_else(|| "UNKNOWN — local clock cannot date the daemon snapshot".to_string())?;
    let age = now.checked_sub(sampled_at).ok_or_else(|| {
        "UNKNOWN — daemon snapshot timestamp is ahead of the local clock".to_string()
    })?;
    let freshness_ms = airc_daemon::route_refresh::REFRESH_INTERVAL.as_millis() as u64 * 2;
    if age > freshness_ms {
        return Err(format!(
            "STALE — daemon delivery snapshot is {}s old (more than two refresh intervals); current route and delivery state are unknown",
            age / 1000
        ));
    }
    Ok(age)
}

pub(super) fn route_finding(
    snapshot: Result<&DeliveryStatsResponse, &str>,
    now_ms: Option<u64>,
) -> Finding {
    let snapshot = match snapshot {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return Finding::warn(
                "route health",
                format!("UNAVAILABLE — daemon delivery_stats did not answer ({error})"),
                "check daemon status; this is missing evidence, not a measured route failure",
            );
        }
    };
    let age = match snapshot_age(snapshot, now_ms) {
        Ok(age) => age,
        Err(detail) => {
            return Finding::warn(
                "route health",
                detail,
                "check daemon build and route-refresh diagnostics; do not infer delivery from a stale or missing snapshot",
            );
        }
    };
    match snapshot.connected_lan_peers {
        Some(0) => Finding::warn(
            "route health",
            format!("daemon snapshot {}s old: no connected LAN peers", age / 1000),
            "check the intended peer's route; this count does not measure other transports or prove delivery failure",
        ),
        Some(count) => Finding::info(
            "route health",
            format!(
                "daemon snapshot {}s old: {count} connected LAN peer(s); connection count is not delivery confirmation (see delivery truth)",
                age / 1000
            ),
        ),
        None => Finding::warn(
            "route health",
            "UNAVAILABLE — daemon snapshot does not report connected LAN peers",
            "check daemon build; no route count can be inferred from delivery history",
        ),
    }
}
