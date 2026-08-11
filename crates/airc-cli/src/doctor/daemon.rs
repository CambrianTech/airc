//! Daemon checks — is a daemon answering this scope's IPC socket, and is
//! the RUNNING daemon the same build as the installed binary?
//!
//! The second question is the one that hides: a stale daemon answers IPC
//! perfectly, so liveness reports `[ok]` and only its reported build
//! reveals the drift. On a live node (BIGMAMA) that went undetected for
//! hours while the fix sat merged and the installed binary was current.

use std::path::Path;

use airc_ipc::DaemonClient;

use super::{short_sha, Check, CheckConfig, CheckContext, Finding};

/// Liveness: does a process answer this scope's IPC socket? One 250ms ping,
/// so it inherits the default [`CheckTier::Always`].
pub(super) struct DaemonLivenessCheck;

#[async_trait::async_trait]
impl Check for DaemonLivenessCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::always("daemon")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_daemon(ctx.home).await
    }
}

/// Build staleness of the RUNNING daemon. Gated to `--health` because it is
/// only meaningful once you care about drift, and it costs a second IPC
/// round trip — the override that [`Check::tier`]'s default exists for.
pub(super) struct DaemonBuildCheck;

#[async_trait::async_trait]
impl Check for DaemonBuildCheck {
    fn config(&self) -> CheckConfig {
        CheckConfig::health("daemon build")
    }

    async fn run(&self, ctx: &CheckContext<'_>) -> Vec<Finding> {
        check_daemon_build(ctx.home).await
    }
}

async fn check_daemon(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let client = DaemonClient::new(socket.clone());
    match client
        .ping_with_timeout(std::time::Duration::from_millis(250))
        .await
    {
        Ok(_) => vec![Finding::ok(
            "daemon",
            format!("responding on {}", socket.display()),
        )],
        Err(_) if socket.exists() => vec![Finding::warn(
            "daemon",
            format!(
                "socket exists at {} but no process answers",
                socket.display()
            ),
            format!(
                "stale socket from prior crash; remove with `rm {}` then `airc join`",
                socket.display()
            ),
        )],
        Err(_) => vec![Finding::info(
            "daemon",
            "not running (`airc join` will spawn it)",
        )],
    }
}

/// Outcome of comparing the RUNNING daemon's reported build against the
/// installed binary's baked-in build. Pure data so the classification is
/// unit-testable without a live daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DaemonBuildState {
    /// Can't compare: the installed binary has no baked-in build sha
    /// (git unavailable at compile time), so we have no truth to check
    /// the daemon against.
    BinaryUnknown,
    /// Can't compare: the daemon didn't report a build (older daemon, or
    /// it was built without git). Surface as info, not a false alarm.
    DaemonUnknown,
    /// Daemon build matches the installed binary — no drift.
    Match,
    /// Daemon is running an OLDER build than the installed binary. The
    /// `(daemon, installed)` short shas for the warning line.
    Stale { daemon: String, installed: String },
}

/// Compare the running daemon's reported build commit against the
/// installed binary's build commit. Both are full git SHAs (or `None` /
/// `"unknown"` when unavailable). Pure: no IO, fully testable.
fn classify_daemon_build(installed: &str, daemon: Option<&str>) -> DaemonBuildState {
    if installed == "unknown" {
        return DaemonBuildState::BinaryUnknown;
    }
    match daemon {
        None => DaemonBuildState::DaemonUnknown,
        Some(daemon) if daemon == installed => DaemonBuildState::Match,
        Some(daemon) => DaemonBuildState::Stale {
            daemon: short_sha(daemon),
            installed: short_sha(installed),
        },
    }
}

/// Stale-daemon check (only under `--health`). `check_binary_freshness`
/// catches "installed binary vs source"; this catches the NEXT link in the
/// chain — the RUNNING daemon still serving an OLD build after `airc
/// update` rebuilt the binary.
pub(super) async fn check_daemon_build(home: &Path) -> Vec<Finding> {
    let socket = crate::cli::default_socket_path_in(home);
    let client = DaemonClient::new(socket);
    let status = match client
        .status_with_timeout(std::time::Duration::from_millis(250))
        .await
    {
        Ok(status) => status,
        // Liveness is already reported by DaemonLivenessCheck, but returning
        // an EMPTY vec here made this check vanish from the report entirely —
        // "can't compare" rendered as nothing at all, which is the same
        // invisibility as the delivery-stats Err arm. Say why it can't answer.
        Err(_) => {
            return vec![Finding::info(
                "daemon build",
                "daemon not reachable, so its build can't be compared to the installed binary",
            )]
        }
    };

    match classify_daemon_build(crate::build_info::COMMIT, status.build_commit.as_deref()) {
        DaemonBuildState::BinaryUnknown => vec![Finding::info(
            "daemon build",
            "installed binary has no build sha (git unavailable at compile time); skipping stale-daemon check",
        )],
        DaemonBuildState::DaemonUnknown => vec![Finding::info(
            "daemon build",
            "running daemon didn't report a build; can't check for staleness",
        )],
        DaemonBuildState::Match => vec![Finding::ok(
            "daemon build",
            format!(
                "running daemon matches installed binary ({})",
                crate::build_info::COMMIT_SHORT
            ),
        )],
        DaemonBuildState::Stale { daemon, installed } => vec![Finding::warn(
            "daemon",
            format!("running stale build {daemon}; installed binary is {installed}"),
            "restart this scope with 'airc join' to pick it up",
        )],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daemon_build_stale_when_daemon_lags_binary() {
        // what this catches: the exact undetected-stale-daemon bug — a
        // running daemon on an OLD build after `airc update` rebuilt the
        // installed binary. Must classify as Stale with both short shas
        // so the operator sees which build to restart away from.
        let state = classify_daemon_build(
            "1111111111111111111111111111111111111111",
            Some("2222222222222222222222222222222222222222"),
        );
        assert_eq!(
            state,
            DaemonBuildState::Stale {
                daemon: "222222222222".to_string(),
                installed: "111111111111".to_string(),
            }
        );
    }

    #[test]
    fn daemon_build_matches_when_equal() {
        // what this catches: identical builds must NOT warn (no false
        // alarm on a freshly-restarted, current daemon).
        let sha = "abcdef1234567890abcdef1234567890abcdef12";
        assert_eq!(
            classify_daemon_build(sha, Some(sha)),
            DaemonBuildState::Match
        );
    }

    #[test]
    fn daemon_build_binary_unknown_skips_check() {
        // what this catches: a release-tarball binary (no baked-in sha)
        // has no truth to compare against, so we skip rather than warn.
        assert_eq!(
            classify_daemon_build("unknown", Some("abc123")),
            DaemonBuildState::BinaryUnknown
        );
    }

    #[test]
    fn daemon_build_daemon_unknown_is_info_not_warn() {
        // what this catches: an older daemon that reports no build must
        // be info (can't compare), never a stale warning.
        assert_eq!(
            classify_daemon_build("abcdef1234567890", None),
            DaemonBuildState::DaemonUnknown
        );
    }
}
