//! Clap argument shapes for `airc-rs log ...`.

use std::path::PathBuf;

use clap::{Args, Subcommand};

pub const DEFAULT_MAX_LINES: usize = 5_000;
pub const DEFAULT_KEEP_LINES: usize = 2_500;

#[derive(Debug, Args)]
pub struct LogArgs {
    #[command(subcommand)]
    pub action: LogAction,
}

#[derive(Debug, Subcommand)]
pub enum LogAction {
    /// Append one stdin frame unless its JSON `sig` exists in the recent tail.
    Append {
        /// messages.jsonl path to append.
        #[arg(long)]
        path: PathBuf,
    },

    /// Trim a messages.jsonl file to its recent tail when over threshold.
    Rotate {
        /// messages.jsonl path to rotate.
        #[arg(long)]
        path: PathBuf,
        /// Rotate only when the file has more than this many lines.
        #[arg(long, default_value_t = DEFAULT_MAX_LINES)]
        max_lines: usize,
        /// Number of tail lines to keep after rotation.
        #[arg(long, default_value_t = DEFAULT_KEEP_LINES)]
        keep_lines: usize,
    },

    /// Render messages.jsonl lines from stdin.
    Render {
        /// Filter to messages newer than this ISO timestamp or relative window.
        #[arg(long, default_value = "")]
        since: String,
        /// Number of raw tail lines read by the caller.
        #[arg(long)]
        count: usize,
        /// Emit machine-readable JSON instead of human text.
        #[arg(long)]
        json: bool,
    },

    /// Read unread messages from messages.jsonl using a byte cursor.
    InboxRead {
        /// Scope home containing messages.jsonl.
        #[arg(long)]
        home: PathBuf,
        /// Cursor file to read/update.
        #[arg(long)]
        cursor_file: PathBuf,
        /// Filter to messages newer than this ISO timestamp or relative window.
        #[arg(long, default_value = "")]
        since: String,
        /// Maximum messages to print.
        #[arg(long, default_value_t = 500)]
        count: usize,
        /// Do not advance the cursor.
        #[arg(long)]
        peek: bool,
        /// Suppress the empty inbox line.
        #[arg(long)]
        quiet_empty: bool,
        /// Hide messages from this identity.
        #[arg(long)]
        exclude_self: bool,
        #[arg(long, default_value = "")]
        my_name: String,
        #[arg(long, default_value = "")]
        client_id: String,
    },

    /// Move the inbox byte cursor to the current end of messages.jsonl.
    InboxReset {
        /// Scope home containing messages.jsonl.
        #[arg(long)]
        home: PathBuf,
        /// Cursor file to write.
        #[arg(long)]
        cursor_file: PathBuf,
    },
}
