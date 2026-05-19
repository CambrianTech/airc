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
}

#[derive(Debug, Args)]
pub struct RouteStatusArgs {
    /// Add a healthy direct transport candidate.
    #[arg(long = "direct", value_enum)]
    pub direct: Vec<RouteTransport>,

    /// Add a healthy relay transport candidate.
    #[arg(long = "relay", value_enum)]
    pub relay: Vec<RouteTransport>,

    /// Add a healthy bootstrap-only transport candidate.
    #[arg(long = "bootstrap", value_enum)]
    pub bootstrap: Vec<RouteTransport>,

    /// Mark a transport down. Format: `<transport>:<role>`, e.g.
    /// `lan-tcp:direct` or `gh-gist:bootstrap-only`.
    #[arg(long = "down")]
    pub down: Vec<RouteHealthOverride>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum RouteTransport {
    LocalFs,
    LanTcp,
    Tailscale,
    Reticulum,
    Relay,
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
    BootstrapOnly,
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
