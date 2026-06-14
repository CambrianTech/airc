use std::path::Path;

use airc_lib::{Airc, TransportHealthSample, TransportHealthState, TransportKind, TransportRole};
use serde::Serialize;

/// Card 9cbe1101 (seam #6): the printed prose used to derive from
/// "degraded count == 0" alone — which painted `ok (0 route(s) healthy)`
/// even when there were NO routes (degenerate case). A "0 healthy routes"
/// substrate is not OK; it has nothing to be degraded against because
/// there's nothing there at all.
///
/// The fix is to compute a typed verdict from the snapshot first, then
/// derive prose from the verdict — printer never inspects the snapshot
/// directly. Three states, mutually exclusive:
///
///   - `Ok` — at least one route is healthy.
///   - `Degraded` — there are routes, some are not healthy.
///   - `NoRoutes` — health table empty; daemon found no transports to
///     monitor. Often the substrate not (yet) routing — closer to a
///     bootstrap problem than to "healthy."
#[derive(Debug, Serialize, PartialEq, Eq, Clone, Copy)]
#[serde(tag = "kind", rename_all = "kebab-case")]
enum HealthVerdict {
    Ok { healthy_routes: usize },
    Degraded { degraded: usize, total: usize },
    NoRoutes,
}

impl HealthVerdict {
    fn from_snapshot_counts(healthy: usize, degraded: usize) -> Self {
        match (healthy, degraded) {
            (0, 0) => HealthVerdict::NoRoutes,
            (_, 0) => HealthVerdict::Ok {
                healthy_routes: healthy,
            },
            (_, degraded) => HealthVerdict::Degraded {
                degraded,
                total: healthy + degraded,
            },
        }
    }

    fn is_failure(self) -> bool {
        !matches!(self, HealthVerdict::Ok { .. })
    }

    fn fmt_line(self) -> String {
        match self {
            HealthVerdict::Ok { healthy_routes } => {
                format!("transport health: ok ({healthy_routes} route(s) healthy)")
            }
            HealthVerdict::Degraded { degraded, total } => {
                format!("transport health: DEGRADED ({degraded}/{total} route(s) need attention)")
            }
            HealthVerdict::NoRoutes => {
                "transport health: no-routes (0 routes — substrate not routing)".to_string()
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct HealthReport {
    verdict: HealthVerdict,
    endpoints: usize,
    lan_peers: usize,
    dial_failures: usize,
}

pub async fn run_health(
    home: &Path,
    quiet: bool,
    degraded_only: bool,
    fail: bool,
    as_json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let snapshot = airc.refresh_route_discovery().await?;
    let healthy = snapshot
        .health
        .iter()
        .filter(|sample| sample.state == TransportHealthState::Healthy)
        .count();
    let degraded = snapshot.health.len() - healthy;
    let verdict = HealthVerdict::from_snapshot_counts(healthy, degraded);

    if as_json {
        let report = HealthReport {
            verdict,
            endpoints: snapshot.endpoints.len(),
            lan_peers: snapshot.connected_lan_peers.len(),
            dial_failures: snapshot.peer_dial_failures.len(),
        };
        println!("{}", serde_json::to_string(&report)?);
        return if !fail || !verdict.is_failure() {
            Ok(())
        } else {
            Err("transport health degraded".into())
        };
    }

    if degraded_only && degraded == 0 {
        return Ok(());
    }
    if quiet {
        return if verdict.is_failure() {
            Err("transport health degraded".into())
        } else {
            Ok(())
        };
    }

    println!("{}", verdict.fmt_line());

    for sample in snapshot.health {
        if degraded_only && sample.state == TransportHealthState::Healthy {
            continue;
        }
        print_sample(sample);
    }

    if snapshot.endpoints.is_empty() {
        println!("endpoints: none");
    } else {
        println!("endpoints: {}", snapshot.endpoints.len());
    }
    if snapshot.connected_lan_peers.is_empty() {
        println!("lan peers: none");
    } else {
        println!("lan peers: {}", snapshot.connected_lan_peers.len());
    }
    // Card 625abe6d slice 1: every failed outbound dial to a stored
    // peer endpoint is visible here — an offline peer is normal mesh
    // weather, a silently-undialed endpoint is a bug.
    for failure in &snapshot.peer_dial_failures {
        println!(
            "dial failed: {} via {:?} — {}",
            failure.peer_id, failure.endpoint, failure.error
        );
    }

    if !verdict.is_failure() || !fail {
        Ok(())
    } else {
        Err("transport health degraded".into())
    }
}

fn print_sample(sample: TransportHealthSample) {
    let detail = match (sample.rtt_ms, sample.success_ppm) {
        (Some(rtt), Some(success)) => format!("{rtt}ms, {:.3}% success", success as f64 / 10_000.0),
        (Some(rtt), None) => format!("{rtt}ms"),
        (None, Some(success)) => format!("{:.3}% success", success as f64 / 10_000.0),
        (None, None) => "not measured".to_string(),
    };
    println!(
        "- {} role={} state={} ({detail})",
        format_kind(sample.kind),
        format_role(sample.role),
        format_state(sample.state)
    );
}

fn format_kind(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::LanTcp => "lan-tcp",
        TransportKind::Tailscale => "tailscale",
        TransportKind::Udp => "udp",
        TransportKind::WebRtcDataChannel => "webrtc-data-channel",
        TransportKind::Reticulum => "reticulum",
        TransportKind::Relay => "relay",
        TransportKind::Ssh => "ssh",
        TransportKind::GhGist => "gh-gist",
    }
}

fn format_role(role: TransportRole) -> &'static str {
    match role {
        TransportRole::Direct => "direct",
        TransportRole::Relay => "relay",
        TransportRole::InviteBeacon => "invite-beacon",
        TransportRole::Rendezvous => "rendezvous",
        TransportRole::Admin => "admin",
    }
}

fn format_state(state: TransportHealthState) -> &'static str {
    match state {
        TransportHealthState::Healthy => "healthy",
        TransportHealthState::Degraded => "degraded",
        TransportHealthState::Down => "down",
    }
}

#[cfg(test)]
mod verdict_tests {
    use super::*;

    #[test]
    fn verdict_zero_routes_is_no_routes_not_ok() {
        assert_eq!(
            HealthVerdict::from_snapshot_counts(0, 0),
            HealthVerdict::NoRoutes
        );
    }

    #[test]
    fn verdict_all_healthy_is_ok() {
        assert_eq!(
            HealthVerdict::from_snapshot_counts(3, 0),
            HealthVerdict::Ok { healthy_routes: 3 }
        );
    }

    #[test]
    fn verdict_mixed_is_degraded_with_total() {
        assert_eq!(
            HealthVerdict::from_snapshot_counts(2, 1),
            HealthVerdict::Degraded {
                degraded: 1,
                total: 3
            }
        );
    }

    #[test]
    fn verdict_only_degraded_is_degraded_not_no_routes() {
        // 0 healthy + some degraded means routes exist; the substrate
        // IS reaching out — just badly. Don't classify as "no routes".
        assert_eq!(
            HealthVerdict::from_snapshot_counts(0, 2),
            HealthVerdict::Degraded {
                degraded: 2,
                total: 2
            }
        );
    }

    #[test]
    fn no_routes_and_degraded_are_failure_ok_is_not() {
        assert!(HealthVerdict::NoRoutes.is_failure());
        assert!(HealthVerdict::Degraded {
            degraded: 1,
            total: 1
        }
        .is_failure());
        assert!(!HealthVerdict::Ok { healthy_routes: 1 }.is_failure());
    }

    #[test]
    fn ok_line_renders_with_count() {
        assert_eq!(
            HealthVerdict::Ok { healthy_routes: 2 }.fmt_line(),
            "transport health: ok (2 route(s) healthy)"
        );
    }

    #[test]
    fn degraded_line_renders_with_ratio() {
        assert_eq!(
            HealthVerdict::Degraded {
                degraded: 1,
                total: 3
            }
            .fmt_line(),
            "transport health: DEGRADED (1/3 route(s) need attention)"
        );
    }

    #[test]
    fn no_routes_line_is_distinct_from_ok() {
        // Card 9cbe1101: the live-found bug was "ok (0 route(s) healthy)".
        // Verify the new prose is unmistakeably NOT "ok".
        let line = HealthVerdict::NoRoutes.fmt_line();
        assert!(
            !line.contains("ok"),
            "no-routes must not render as ok: {line}"
        );
        assert!(line.contains("no-routes"));
    }

    #[test]
    fn verdict_json_round_trips() {
        // The --json mode emits {"kind": "ok" | "degraded" | "no-routes", ...}
        // — operators / scripts depend on the kind tag. Lock the shape.
        let s = serde_json::to_string(&HealthVerdict::NoRoutes).unwrap();
        assert!(s.contains("\"kind\":\"no-routes\""), "got {s}");
        let s = serde_json::to_string(&HealthVerdict::Ok { healthy_routes: 2 }).unwrap();
        assert!(s.contains("\"kind\":\"ok\""), "got {s}");
        assert!(s.contains("\"healthy_routes\":2"), "got {s}");
    }
}
