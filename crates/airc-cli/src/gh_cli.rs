use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct GhArgs {
    #[command(subcommand)]
    pub action: GhAction,
}

#[derive(Debug, Subcommand)]
pub enum GhAction {
    /// Run `gh` through the shared AIRC request governor.
    Run {
        /// Arguments passed to gh. Use `--` before gh flags.
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        gh_args: Vec<String>,
    },

    /// Print current shared backoff wait in seconds.
    WaitSeconds,

    /// Inspect or clear the local gh governor audit state.
    Audit {
        #[arg(long, default_value_t = 50)]
        count: usize,
        #[arg(long)]
        summary: bool,
        #[arg(long)]
        reset: bool,
        #[arg(long)]
        clear_audit: bool,
    },

    /// Health report for the local gh governor.
    Doctor {
        #[arg(long, default_value_t = 80)]
        count: usize,
    },
}
