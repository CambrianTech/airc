use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct MonitorArgs {
    #[command(subcommand)]
    pub action: MonitorAction,
}

#[derive(Debug, Subcommand)]
pub enum MonitorAction {
    /// Read legacy JSONL monitor frames from stdin and render stdout notifications.
    Format {
        /// Legacy peers directory containing <peer>.json records.
        #[arg(long)]
        peers_dir: PathBuf,
        /// Current local display name.
        #[arg(long)]
        my_name: String,
    },

    /// Attach to the Rust event stream without owning transport.
    Attach {
        /// Current local display name.
        #[arg(long)]
        my_name: String,
        /// **Card 7d5b6a65.** Subscribe from the LIVE EDGE — only
        /// events published strictly after the attach call return.
        ///
        /// Default. The agent-Monitor live-tail shape: no transcript
        /// replay, no per-historical-event notification flood. Pass
        /// `--include-backlog` for the legacy "give me everything"
        /// behaviour.
        #[arg(long, default_value_t = true, overrides_with = "include_backlog")]
        from_now: bool,
        /// **Card 7d5b6a65.** Opt INTO backlog replay. When set, the
        /// daemon replays historical events on attach. With
        /// `--coalesce-backlog` (default ON when this is set), the
        /// catch-up is collapsed to ONE `caught up: skipped N events`
        /// summary line; without it, each historical event is rendered
        /// individually.
        #[arg(long, default_value_t = false, conflicts_with = "from_now")]
        include_backlog: bool,
        /// **Card 7d5b6a65.** When backlog is replayed, collapse the
        /// catch-up phase to ONE summary line instead of N
        /// per-historical-event lines. Default ON whenever backlog is
        /// included — pass `--no-coalesce-backlog` for the legacy
        /// event-by-event replay (audit / replay tooling that needs
        /// every historical envelope on stdout).
        #[arg(long, default_value_t = true)]
        coalesce_backlog: bool,
    },
}
