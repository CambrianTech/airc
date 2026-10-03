//! Clap argument shapes for `airc events ...`.

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Args)]
pub struct EventsArgs {
    #[command(subcommand)]
    pub action: EventsAction,
}

#[derive(Debug, Subcommand)]
pub enum EventsAction {
    /// Observe one durable event ID in the machine owner's store without
    /// starting the daemon or modifying the database. Read failures are errors.
    Contains {
        event_id: uuid::Uuid,
        /// Emit schema_version=1 JSON with event_id, database, and present.
        #[arg(long)]
        json: bool,
    },
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
        /// Emit a machine-readable JSON object instead of human text.
        #[arg(long)]
        json: bool,
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
