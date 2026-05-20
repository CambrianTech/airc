use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct CollaborationArgs {
    #[command(subcommand)]
    pub action: CollaborationAction,
}

#[derive(Debug, Subcommand)]
pub enum CollaborationAction {
    /// Print collaboration health for the current scope.
    Status(CollaborationScopeArgs),
    /// Print doctor-style collaboration findings.
    Doctor(CollaborationScopeArgs),
    /// Warn when sends are likely isolated.
    SendWarning(CollaborationScopeArgs),
    /// Print paired peers plus recent broadcast-only peers.
    Peers(CollaborationScopeArgs),
    /// Remove stale duplicate peer records for the same host.
    PrunePeers(CollaborationScopeArgs),
    /// Print identity evidence observed from signed room traffic.
    ObservedWhois {
        #[command(flatten)]
        scope: CollaborationScopeArgs,
        #[arg(long)]
        peer_name: String,
    },
}

#[derive(Debug, Args)]
pub struct CollaborationScopeArgs {
    #[arg(long)]
    pub home: Option<PathBuf>,
    #[arg(long, default_value = "")]
    pub my_name: String,
    #[arg(long, default_value = "")]
    pub client_id: String,
}
