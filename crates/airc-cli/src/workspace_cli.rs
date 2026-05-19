//! Clap argument shapes for `airc-rs workspace ...`.

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct WorkspaceArgs {
    #[command(subcommand)]
    pub action: WorkspaceAction,
}

#[derive(Debug, Subcommand)]
pub enum WorkspaceAction {
    /// Request a workspace lease for a claimed work card.
    Request {
        /// Work card UUID.
        card_id: String,
        /// Claim UUID returned by `work claim`.
        claim_id: String,
        /// Repository key, e.g. `CambrianTech/airc`.
        #[arg(long)]
        repo: String,
        /// Workspace branch name.
        #[arg(long)]
        branch: String,
        /// Base branch name.
        #[arg(long, default_value = "rust-rewrite")]
        base: String,
    },
    /// Mark a requested workspace as allocated at a concrete path.
    Allocate {
        /// Workspace UUID returned by `workspace request`.
        workspace_id: String,
        /// Filesystem path allocated for the workspace.
        #[arg(long)]
        path: String,
    },
    /// Heartbeat a workspace lease.
    Heartbeat {
        /// Workspace UUID.
        workspace_id: String,
        /// Optional disk usage in bytes.
        #[arg(long)]
        disk_bytes: Option<u64>,
    },
    /// Release a workspace lease.
    Release {
        /// Workspace UUID.
        workspace_id: String,
    },
    /// Print the current room's projected workspace leases.
    List {
        /// Recent transcript events to replay into the projection.
        #[arg(long, default_value_t = 128)]
        limit: usize,
    },
}
