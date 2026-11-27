//! Which binary may replace a running daemon whose build differs from it (airc#1692: a worktree
//! build run beside the installed airc stopped the daemon every session of the account used).

use std::path::{Path, PathBuf};

/// Whether this binary may replace a running daemon whose build differs from it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum DaemonReplacement {
    /// The same installed executable, rebuilt: the update path adopts it.
    Adopt,
    Refused(ForeignBuild),
}

/// A different airc executable (a development or worktree build) would have stopped the daemon
/// every session of this account uses. It runs isolated instead.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ForeignBuild {
    running: Option<PathBuf>,
    this: PathBuf,
}

impl std::fmt::Display for ForeignBuild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let running = self.running.as_ref().map_or_else(
            || "an unreported executable".into(),
            |p| p.display().to_string(),
        );
        write!(
            f,
            "the running daemon is {running}, a different build from {this}; a build that is not \
             the installed airc never replaces it. Run this build isolated (`--home <dir>` with \
             its own scope), or install it with `airc update`.",
            this = self.this.display()
        )
    }
}

impl std::error::Error for ForeignBuild {}

/// Only the executable the daemon runs may replace it, once rebuilt; any other is refused.
pub(crate) fn daemon_replacement(running: Option<&Path>, this: &Path) -> DaemonReplacement {
    let same = |a: &Path, b: &Path| match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    };
    match running {
        Some(running) if same(running, this) => DaemonReplacement::Adopt,
        running => DaemonReplacement::Refused(ForeignBuild {
            running: running.map(Path::to_path_buf),
            this: this.to_path_buf(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a development build stopping the daemon every session on the account
    // uses (it restarted the M5's twice on 2026-11-27), or the installed airc, rebuilt by an
    // update, refused its own daemon. Only the daemon's own executable adopts it.
    #[test]
    fn only_the_daemons_own_executable_replaces_it() {
        let dir = tempfile::tempdir().unwrap();
        let installed = dir.path().join("airc");
        let worktree = dir.path().join("target-debug-airc");
        std::fs::write(&installed, b"").unwrap();
        std::fs::write(&worktree, b"").unwrap();
        assert_eq!(
            daemon_replacement(Some(&installed), &installed),
            DaemonReplacement::Adopt
        );
        assert_eq!(
            daemon_replacement(Some(&installed), &worktree),
            DaemonReplacement::Refused(ForeignBuild {
                running: Some(installed.clone()),
                this: worktree.clone(),
            })
        );
        assert!(
            matches!(
                daemon_replacement(None, &installed),
                DaemonReplacement::Refused(_)
            ),
            "a daemon that does not report its executable cannot be proven ours"
        );
    }
}
