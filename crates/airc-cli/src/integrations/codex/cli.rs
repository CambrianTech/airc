//! Codex integration CLI definitions.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::integrations::agent_hook::{self, HookEvent, HookReadArgs};

#[derive(Debug, Args)]
pub struct CodexHookArgs {
    #[command(subcommand)]
    pub action: CodexHookAction,
}

#[derive(Debug, Subcommand)]
pub enum CodexHookAction {
    /// Update installer-owned configuration through the TOML parser.
    ConfigureInstaller {
        #[arg(long)]
        codex_home: Option<PathBuf>,
        /// Read the GitHub token from stdin, never from command-line arguments.
        #[arg(long)]
        token_stdin: bool,
        /// Preserve any existing rules configuration; install the legacy default only if absent.
        #[arg(long)]
        command_rules: bool,
    },
    /// Install the Rust prompt and post-tool hooks into Codex config.
    InstallHooks {
        /// Codex home directory. Defaults to `$HOME/.codex`.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// Remove AIRC-managed prompt and post-tool hooks from Codex config.
    UninstallHooks {
        /// Codex home directory. Defaults to `$HOME/.codex`.
        #[arg(long)]
        codex_home: Option<PathBuf>,
    },
    /// Emit Codex UserPromptSubmit JSON with unread AIRC context.
    UserPromptSubmit(#[command(flatten)] HookReadArgs),
    /// Emit Codex PostToolUse JSON with unread AIRC context.
    PostToolUse(#[command(flatten)] HookReadArgs),
    /// Print unread AIRC context for an active Codex turn.
    ///
    /// Unlike the UserPromptSubmit hook, this is a normal CLI surface
    /// Codex can call between tool steps. With `--wait-ms`, it briefly
    /// waits for a new subscribed event before returning.
    Poll {
        #[command(flatten)]
        read: HookReadArgs,
        /// Wait this many milliseconds for one new event if no unread
        /// events are immediately available.
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
    },
}

impl CodexHookArgs {
    pub async fn run(self, home: &Path) -> Result<(), Box<dyn std::error::Error>> {
        use super::{hook::CODEX, install};
        match self.action {
            CodexHookAction::ConfigureInstaller {
                codex_home,
                token_stdin,
                command_rules,
            } => install::configure_installer(codex_home, token_stdin, command_rules),
            CodexHookAction::InstallHooks { codex_home } => {
                install::run_install_hooks(codex_home).await
            }
            CodexHookAction::UninstallHooks { codex_home } => {
                install::run_uninstall_hooks(codex_home).await
            }
            CodexHookAction::UserPromptSubmit(read) => {
                agent_hook::run_hook(&CODEX, home, &read.options(), HookEvent::UserPromptSubmit)
                    .await
            }
            CodexHookAction::PostToolUse(read) => {
                agent_hook::run_hook(&CODEX, home, &read.options(), HookEvent::PostToolUse).await
            }
            CodexHookAction::Poll { read, wait_ms } => {
                agent_hook::run_poll(&CODEX, home, &read.options(), wait_ms).await
            }
        }
    }
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
