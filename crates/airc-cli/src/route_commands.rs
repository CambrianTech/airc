use airc_lib::{
    RouteDecision, RoutePurpose, TransportHealthSample, TransportHealthState, TransportKind,
    TransportResolver, TransportRole,
};

use crate::route_cli::{RouteHealthOverride, RouteRole, RouteStatusArgs, RouteTransport};

pub fn run_status(args: RouteStatusArgs) -> Result<(), Box<dyn std::error::Error>> {
    let samples = health_samples(args);
    let resolver = TransportResolver::from_health(samples.iter().copied());

    println!("route candidates:");
    for sample in &samples {
        println!(
            "- {} role={} state={}",
            format_kind(sample.kind),
            format_role(sample.role),
            format_state(sample.state)
        );
    }

    println!("route decisions:");
    for purpose in [
        RoutePurpose::Data,
        RoutePurpose::LiveEvent,
        RoutePurpose::Bootstrap,
    ] {
        match resolver.resolve(purpose) {
            Ok(route) => println!(
                "- {} -> {}",
                format_purpose(purpose),
                format_kind(route.kind)
            ),
            Err(RouteDecision::NoRoute { .. }) => {
                println!("- {} -> no-route", format_purpose(purpose));
            }
            Err(RouteDecision::Selected(_)) => unreachable!("selected routes are returned as Ok"),
        }
    }

    Ok(())
}

fn health_samples(args: RouteStatusArgs) -> Vec<TransportHealthSample> {
    let mut samples = Vec::new();

    if args.direct.is_empty()
        && args.relay.is_empty()
        && args.bootstrap.is_empty()
        && args.down.is_empty()
    {
        samples.push(TransportHealthSample::healthy_direct(
            TransportKind::LocalFs,
        ));
        return samples;
    }

    samples.extend(
        args.direct
            .into_iter()
            .map(|transport| healthy(transport, RouteRole::Direct)),
    );
    samples.extend(
        args.relay
            .into_iter()
            .map(|transport| healthy(transport, RouteRole::Relay)),
    );
    samples.extend(
        args.bootstrap
            .into_iter()
            .map(|transport| healthy(transport, RouteRole::BootstrapOnly)),
    );
    samples.extend(args.down.into_iter().map(down));
    samples
}

fn healthy(transport: RouteTransport, role: RouteRole) -> TransportHealthSample {
    TransportHealthSample {
        kind: transport.into(),
        role: role.into(),
        state: TransportHealthState::Healthy,
        rtt_ms: None,
        success_ppm: None,
    }
}

fn down(override_: RouteHealthOverride) -> TransportHealthSample {
    TransportHealthSample::down(override_.transport.into(), override_.role.into())
}

fn format_purpose(purpose: RoutePurpose) -> &'static str {
    match purpose {
        RoutePurpose::Data => "data",
        RoutePurpose::LiveEvent => "live-event",
        RoutePurpose::Bootstrap => "bootstrap",
    }
}

fn format_kind(kind: TransportKind) -> &'static str {
    match kind {
        TransportKind::LocalFs => "local-fs",
        TransportKind::LanTcp => "lan-tcp",
        TransportKind::Tailscale => "tailscale",
        TransportKind::Reticulum => "reticulum",
        TransportKind::Relay => "relay",
        TransportKind::GhGist => "gh-gist",
    }
}

fn format_role(role: TransportRole) -> &'static str {
    match role {
        TransportRole::Direct => "direct",
        TransportRole::Relay => "relay",
        TransportRole::BootstrapOnly => "bootstrap-only",
    }
}

fn format_state(state: TransportHealthState) -> &'static str {
    match state {
        TransportHealthState::Healthy => "healthy",
        TransportHealthState::Degraded => "degraded",
        TransportHealthState::Down => "down",
    }
}

impl From<RouteTransport> for TransportKind {
    fn from(value: RouteTransport) -> Self {
        match value {
            RouteTransport::LocalFs => Self::LocalFs,
            RouteTransport::LanTcp => Self::LanTcp,
            RouteTransport::Tailscale => Self::Tailscale,
            RouteTransport::Reticulum => Self::Reticulum,
            RouteTransport::Relay => Self::Relay,
            RouteTransport::GhGist => Self::GhGist,
        }
    }
}

impl From<RouteRole> for TransportRole {
    fn from(value: RouteRole) -> Self {
        match value {
            RouteRole::Direct => Self::Direct,
            RouteRole::Relay => Self::Relay,
            RouteRole::BootstrapOnly => Self::BootstrapOnly,
        }
    }
}
