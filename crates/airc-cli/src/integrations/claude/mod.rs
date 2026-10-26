//! Claude Code integration boundary (card b00d2724).
//!
//! A Claude Code session hears AIRC through a Monitor on `airc join`, which
//! expires and is lost at a context compaction. These hooks give the session
//! the same catch-up Codex has on every prompt and after every tool call. Only
//! Claude's lines live here: its verb, its session prefix, its settings file,
//! and how it re-arms a reader. The delivery itself is `integrations::agent_hook`.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::integrations::agent_hook::settings::{self, ManagedHook, ManagedHooks};
use crate::integrations::agent_hook::{self, HookEvent, HookReadArgs, HookRuntime};

pub const CLAUDE: HookRuntime = HookRuntime {
    verb: "claude-hook",
    name: "Claude Code",
    session_prefix: "claude:",
    rearm_reader: Some(
        "Re-arm one: run `airc join` under a Monitor (it lapses after its timeout and at a context compaction).",
    ),
};

/// Claude's hooks in `settings.json`. PostToolUse takes a matcher; `*` is every tool.
const CLAUDE_HOOKS: ManagedHooks = ManagedHooks {
    hooks: &[
        ManagedHook {
            event: "UserPromptSubmit",
            args: "claude-hook user-prompt-submit",
            matcher: None,
        },
        ManagedHook {
            event: "PostToolUse",
            args: "claude-hook post-tool-use",
            matcher: Some("*"),
        },
    ],
    status_message: None,
};

#[derive(Debug, Args)]
pub struct ClaudeHookArgs {
    #[command(subcommand)]
    pub action: ClaudeHookAction,
}

#[derive(Debug, Subcommand)]
pub enum ClaudeHookAction {
    /// Install the AIRC prompt and post-tool hooks into Claude Code settings.
    InstallHooks {
        /// Settings file. Defaults to `$CLAUDE_CONFIG_DIR/settings.json`, else
        /// `$HOME/.claude/settings.json`.
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Remove only the AIRC-managed hooks from Claude Code settings.
    UninstallHooks {
        /// Settings file. Same default as install-hooks.
        #[arg(long)]
        settings: Option<PathBuf>,
    },
    /// Emit Claude Code UserPromptSubmit JSON with unread AIRC context.
    UserPromptSubmit(#[command(flatten)] HookReadArgs),
    /// Emit Claude Code PostToolUse JSON with unread AIRC context.
    PostToolUse(#[command(flatten)] HookReadArgs),
    /// Print unread AIRC context for this Claude Code session.
    Poll {
        #[command(flatten)]
        read: HookReadArgs,
        /// Wait this many milliseconds for one new event if none is unread.
        #[arg(long, default_value_t = 0)]
        wait_ms: u64,
    },
}

impl ClaudeHookArgs {
    pub async fn run(self, home: &Path) -> Result<(), Box<dyn std::error::Error>> {
        match self.action {
            ClaudeHookAction::InstallHooks { settings: path } => {
                let path = path.map_or_else(default_settings, Ok)?;
                if settings::install(&path, &CLAUDE_HOOKS)? {
                    println!(
                        "installed AIRC UserPromptSubmit and PostToolUse hooks in {}",
                        path.display()
                    );
                }
                Ok(())
            }
            ClaudeHookAction::UninstallHooks { settings: path } => {
                let path = path.map_or_else(default_settings, Ok)?;
                if settings::uninstall(&path, &CLAUDE_HOOKS)? {
                    println!(
                        "removed AIRC UserPromptSubmit and PostToolUse hooks from {}",
                        path.display()
                    );
                }
                Ok(())
            }
            ClaudeHookAction::UserPromptSubmit(read) => {
                agent_hook::run_hook(&CLAUDE, home, &read.options(), HookEvent::UserPromptSubmit)
                    .await
            }
            ClaudeHookAction::PostToolUse(read) => {
                agent_hook::run_hook(&CLAUDE, home, &read.options(), HookEvent::PostToolUse).await
            }
            ClaudeHookAction::Poll { read, wait_ms } => {
                agent_hook::run_poll(&CLAUDE, home, &read.options(), wait_ms).await
            }
        }
    }
}

/// Claude Code's user settings file: `$CLAUDE_CONFIG_DIR/settings.json` when the
/// runtime is configured there, else `$HOME/.claude/settings.json`.
fn default_settings() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir).join("settings.json"));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("neither HOME nor USERPROFILE is set; pass --settings")?;
    Ok(PathBuf::from(home).join(".claude").join("settings.json"))
}
