use std::path::Path;

use airc_lib::{Airc, TransportHealthSample, TransportHealthState, TransportKind, TransportRole};

pub async fn run_health(
    home: &Path,
    quiet: bool,
    degraded_only: bool,
    fail: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let snapshot = airc.refresh_route_discovery().await?;
    let degraded = snapshot
        .health
        .iter()
        .filter(|sample| sample.state != TransportHealthState::Healthy)
        .count();

    if degraded_only && degraded == 0 {
        return Ok(());
    }
    if quiet {
        return if degraded == 0 {
            Ok(())
        } else {
            Err("transport health degraded".into())
        };
    }

    if degraded == 0 {
        println!(
            "transport health: ok ({} route(s) healthy)",
            snapshot.health.len()
        );
    } else {
        println!(
            "transport health: DEGRADED ({degraded}/{} route(s) need attention)",
            snapshot.health.len()
        );
    }

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

    if degraded == 0 || !fail {
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
