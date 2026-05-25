use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub struct RouteArgs {
    #[command(subcommand)]
    pub action: RouteAction,
}

#[derive(Debug, Subcommand)]
pub enum RouteAction {
    /// Show route candidates and policy decisions.
    Status(RouteStatusArgs),
    /// Execute a route proof and print a machine-readable JSON report.
    Proof(RouteProofArgs),
}

#[derive(Debug, Args)]
pub struct RouteStatusArgs {
    /// Add a healthy direct transport candidate.
    #[arg(long = "direct", value_enum)]
    pub direct: Vec<RouteTransport>,

    /// Add a healthy relay transport candidate.
    #[arg(long = "relay", value_enum)]
    pub relay: Vec<RouteTransport>,

    /// Add a healthy peer rendezvous transport candidate.
    #[arg(long = "rendezvous", value_enum)]
    pub rendezvous: Vec<RouteTransport>,

    /// Add a healthy invite beacon transport candidate.
    #[arg(long = "invite", value_enum)]
    pub invite: Vec<RouteTransport>,

    /// Mark a transport down. Format: `<transport>:<role>`, e.g.
    /// `lan-tcp:direct` or `gh-gist:rendezvous`.
    #[arg(long = "down")]
    pub down: Vec<RouteHealthOverride>,
}

#[derive(Debug, Args)]
pub struct RouteProofArgs {
    /// Which proof to run.
    #[arg(long, value_enum, default_value_t = RouteProofKind::RelayLoopback)]
    pub kind: RouteProofKind,

    /// Deadline for the command-bus round trip.
    #[arg(long, default_value_t = 3000)]
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RouteProofKind {
    /// Two independent homes exchange a request/reply over TLS LAN-TCP on loopback.
    LanLoopback,
    /// Two independent homes exchange a request/reply through a local `airc-relay`.
    RelayLoopback,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RouteTransport {
    LocalFs,
    LanTcp,
    Tailscale,
    Udp,
    WebRtcDataChannel,
    Reticulum,
    Relay,
    Ssh,
    GhGist,
}

#[derive(Debug, Clone)]
pub struct RouteHealthOverride {
    pub transport: RouteTransport,
    pub role: RouteRole,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RouteRole {
    Direct,
    Relay,
    InviteBeacon,
    Rendezvous,
    Admin,
}

impl std::str::FromStr for RouteHealthOverride {
    type Err = String;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let Some((transport, role)) = value.split_once(':') else {
            return Err("expected <transport>:<role>".to_string());
        };

        Ok(Self {
            transport: RouteTransport::from_str(transport, true)
                .map_err(|_| format!("unknown transport {transport:?}"))?,
            role: RouteRole::from_str(role, true).map_err(|_| format!("unknown role {role:?}"))?,
        })
    }
}
