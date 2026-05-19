//! Clap argument shapes for `airc-rs events ...`.

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub struct EventsArgs {
    #[command(subcommand)]
    pub action: EventsAction,
}

#[derive(Debug, Subcommand)]
pub enum EventsAction {
    /// List persisted current-room events matching filters.
    List {
        /// Restrict to transcript kind. Repeatable.
        #[arg(long = "kind", value_enum)]
        kind: Vec<CliTranscriptKind>,
        /// Exact header match as `key=value`. Repeatable.
        #[arg(long = "header", value_name = "KEY=VALUE")]
        header: Vec<String>,
        /// Header prefix match as `key=prefix`. Repeatable.
        #[arg(long = "header-prefix", value_name = "KEY=PREFIX")]
        header_prefix: Vec<String>,
        /// Recent events to scan before filtering.
        #[arg(long, default_value_t = 128)]
        limit: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum CliTranscriptKind {
    Message,
    Attachment,
    Receipt,
    Presence,
    SessionControl,
    System,
}
