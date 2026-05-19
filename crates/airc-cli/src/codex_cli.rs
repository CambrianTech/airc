//! Codex integration CLI definitions.

use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct CodexHookArgs {
    #[command(subcommand)]
    pub action: CodexHookAction,
}

#[derive(Debug, Subcommand)]
pub enum CodexHookAction {
    /// Emit Codex UserPromptSubmit JSON with unread AIRC context.
    UserPromptSubmit {
        /// Cursor file. Defaults to `<home>/codex_hook_cursor.json`.
        #[arg(long)]
        cursor_file: Option<PathBuf>,
        /// Maximum unread events to fetch from the transcript store.
        #[arg(long, default_value_t = 50)]
        count: usize,
        /// Maximum events to show in digest mode.
        #[arg(long, default_value_t = 8)]
        max_items: usize,
        /// Emit raw unread lines instead of a compact digest.
        #[arg(long)]
        raw: bool,
        /// Include events from this peer. Default excludes same-peer
        /// self echoes so Codex does not re-inject its own sends.
        #[arg(long)]
        include_self: bool,
    },
}
