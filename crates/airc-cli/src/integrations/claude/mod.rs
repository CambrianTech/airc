//! Claude Code integration boundary (card b00d2724).
//!
//! A Claude Code session hears AIRC through these hooks and its wake (docs/AGENT-DELIVERY.md): unread peer traffic on
//! every prompt and after every tool call, presence, a Stop hook that hands over what is addressed before the session idles, and `wake`, whose exit wakes an idle session. No
//! Monitor or other hand-held reader is ever asked for; `airc join` is a plain
//! command that ensures the daemon and the room subscriptions. Only Claude's
//! lines live here: its verb, its session prefix and its settings file. The
//! delivery itself is `integrations::agent_hook`.

use std::path::{Path, PathBuf};

use clap::{Args, Subcommand};

use crate::integrations::agent_hook::settings::{self, ManagedHook, ManagedHooks};
use crate::integrations::agent_hook::{self, HookEvent, HookReadArgs, HookRuntime};

pub const CLAUDE: HookRuntime = HookRuntime {
    verb: "claude-hook",
    name: "Claude Code",
    session_prefix: "claude:",
    presence: "claude",
    wakes: true,
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
        ManagedHook {
            event: "Stop",
            args: "claude-hook stop",
            matcher: None,
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
    /// Emit Claude Code Stop JSON: hold the stop while messages addressed to this session are
    /// undelivered, and ask once for a wake when none is armed.
    Stop(#[command(flatten)] HookReadArgs),
    /// Wait for the next event addressed to this session, then exit: run as a background command,
    /// its exit wakes an idle session.
    Wake,
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
                        "installed AIRC UserPromptSubmit, PostToolUse and Stop hooks in {}",
                        path.display()
                    );
                }
                Ok(())
            }
            ClaudeHookAction::UninstallHooks { settings: path } => {
                let path = path.map_or_else(default_settings, Ok)?;
                if settings::uninstall(&path, &CLAUDE_HOOKS)? {
                    println!(
                        "removed AIRC UserPromptSubmit, PostToolUse and Stop hooks from {}",
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
            ClaudeHookAction::Stop(read) => {
                agent_hook::run_stop(&CLAUDE, home, &read.options()).await
            }
            ClaudeHookAction::Wake => agent_hook::run_wake(&CLAUDE, home).await,
            ClaudeHookAction::Poll { read, wait_ms } => {
                agent_hook::run_poll(&CLAUDE, home, &read.options(), wait_ms).await
            }
        }
    }
}

/// Claude Code's user settings file: `$CLAUDE_CONFIG_DIR/settings.json` when the
/// runtime is configured there, else `$HOME/.claude/settings.json`.
fn default_settings() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(default_config_dir()?.join("settings.json"))
}

/// Claude Code's config directory: `$CLAUDE_CONFIG_DIR`, else `$HOME/.claude`.
fn default_config_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|dir| !dir.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or("neither HOME nor USERPROFILE is set; pass --settings")?;
    Ok(PathBuf::from(home).join(".claude"))
}

/// What `airc join` does for Claude Code, as it does for Codex
/// (`codex::install::install_hooks_for_default_home_if_present`): when Claude
/// Code's config directory exists, install (or refresh) the AIRC hooks in its
/// `settings.json`. Idempotent; `Some(path)` only when the file changed.
pub fn install_hooks_for_default_config_if_present(
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    install_hooks_if_present(&default_config_dir()?)
}

fn install_hooks_if_present(
    config_dir: &Path,
) -> Result<Option<PathBuf>, Box<dyn std::error::Error>> {
    if !config_dir.is_dir() {
        return Ok(None);
    }
    let path = config_dir.join("settings.json");
    Ok(settings::install(&path, &CLAUDE_HOOKS)?.then_some(path))
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: `airc join` installs Claude Code's hooks exactly once. A
    // second install changes nothing and leaves one entry per hook; a machine
    // without Claude Code (no config directory) gets no file.
    #[test]
    fn join_installs_claude_hooks_once_and_only_where_claude_is_present() {
        let dir = tempfile::tempdir().unwrap();
        let absent = dir.path().join("no-claude");
        assert_eq!(install_hooks_if_present(&absent).unwrap(), None);
        assert!(!absent.exists());

        let config = dir.path().join(".claude");
        std::fs::create_dir(&config).unwrap();
        let settings = config.join("settings.json");
        assert_eq!(
            install_hooks_if_present(&config).unwrap(),
            Some(settings.clone())
        );
        let first = std::fs::read_to_string(&settings).unwrap();
        assert_eq!(
            install_hooks_if_present(&config).unwrap(),
            None,
            "idempotent"
        );
        assert_eq!(std::fs::read_to_string(&settings).unwrap(), first);
        for hook in CLAUDE_HOOKS.hooks {
            assert_eq!(
                first.matches(hook.args).count(),
                1,
                "{}: {first}",
                hook.args
            );
        }
    }
}
