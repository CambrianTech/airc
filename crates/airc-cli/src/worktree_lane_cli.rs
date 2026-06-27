use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
pub struct WorktreeLaneArgs {
    #[command(subcommand)]
    pub action: WorktreeLaneAction,
}

#[derive(Debug, Subcommand)]
pub enum WorktreeLaneAction {
    /// Print an absolute path, expanding a leading ~/ when present.
    AbsPath { path: String },
    /// Print the shell-compatible lane slug for an issue or owner token.
    Slug { value: String },
    /// Append a lane record to the JSONL registry.
    Record {
        #[arg(long)]
        registry: PathBuf,
        #[arg(long)]
        issue: String,
        #[arg(long)]
        repo: String,
        #[arg(long)]
        dir: String,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        base: String,
        #[arg(long)]
        owner: String,
    },
    /// List lane records from the JSONL registry.
    List {
        #[arg(long)]
        registry: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Find the newest lane matching issue, dir, or dir basename.
    Find {
        #[arg(long)]
        registry: PathBuf,
        target: String,
        #[arg(long, default_value = "json")]
        field: String,
    },
}
