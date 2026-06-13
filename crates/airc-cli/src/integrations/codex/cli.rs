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
    /// Install the Rust UserPromptSubmit hook into Codex config.
    InstallHooks {
        /// Codex home directory. Defaults to `$HOME/.codex`.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// Remove AIRC-managed UserPromptSubmit hooks from Codex config.
    UninstallHooks {
        /// Codex home directory. Defaults to `$HOME/.codex`.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// Emit Codex UserPromptSubmit JSON with unread AIRC context.
    UserPromptSubmit {
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
    /// Print unread AIRC context for an active Codex turn.
    ///
    /// Unlike the UserPromptSubmit hook, this is a normal CLI surface
    /// Codex can call between tool steps. With `--wait-ms`, it briefly
    /// waits for a new subscribed event before returning.
    Poll {
        /// Maximum unread events to fetch from the transcript store.
        #[arg(long, default_value_t = 50)]
        count: usize,
        /// Maximum events to show in digest mode.
        #[arg(long, default_value_t = 8)]
        max_items: usize,
        /// Emit raw unread lines instead of a compact digest.
        #[arg(long)]
        raw: bool,
        /// Include events from this runtime client. Default excludes
        /// self echoes while still advancing the cursor.
        #[arg(long)]
        include_self: bool,
        /// Wait this many milliseconds for one new event if no unread
        /// events are immediately available.
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
    },
}

#[derive(Debug, Args)]
pub struct CodexStartArgs {
    /// Path to the airc executable the detached child should run.
    #[arg(long)]
    pub airc: PathBuf,
    /// AIRC_HOME/scope directory for the detached child.
    #[arg(long)]
    pub home: PathBuf,
    /// Log file for detached stdout/stderr.
    #[arg(long)]
    pub log: PathBuf,
    /// Arguments forwarded after `airc join`.
    #[arg(last = true)]
    pub join_args: Vec<String>,
}
