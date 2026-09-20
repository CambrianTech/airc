//! Clap argument shapes for `airc workspace ...`.

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
        /// Base branch name. Defaults to the repo's configured
        /// integration branch (card 5e04ab56: ONE source of truth —
        /// `configured_base_branch`, the same resolver PR creation
        /// uses — instead of a second hardcoded name that goes stale
        /// when the branch is renamed).
        #[arg(long)]
        base: Option<String>,
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
        /// Deprecated no-op (kept so existing invocations still parse):
        /// the projection is always complete now — continuum #154.
        #[arg(long, default_value_t = 128)]
        limit: usize,
    },
}
