//! A card clone with no allocation evidence: re-cloned when nothing in it is hers, kept and
//! refused by name otherwise.
//!
//! Kimi on the Windows node, 2026-11-08 (Codex, read-only): her card clone was clean, at
//! e5fe473d47 (#4205, weeks old), with no allocation evidence and no creation anchor. Such a
//! clone was refused ("preserved for recovery") with no recovery, and continuum handed it back
//! to her as-is, so her hands sat on old code and `work/submit` could never name a base. A fresh
//! allocation is safe exactly when the old tree holds nothing anyone could lose; then it is
//! moved aside whole, never deleted. A base is never inferred from its HEAD.

use super::git_in;
use std::path::{Path, PathBuf};

/// What became of an unanchored card clone.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Unanchored {
    /// Moved whole to this path; the caller makes a fresh allocation in its place.
    Retired(PathBuf),
    /// Kept untouched: this is what in it is not on the remote.
    Kept(String),
}

/// Move `path` aside when nothing in it is unpublished: a clean tree (untracked files
/// included), no stash, and every local branch tip present in `mirror` (the node's mirror of
/// the real remote). Anything else keeps it byte-identical and names why.
pub(super) fn retire_if_nothing_hers(mirror: &Path, path: &Path) -> Result<Unanchored, String> {
    let changed = git_in(path, &["status", "--porcelain", "--untracked-files=all"])?;
    if !changed.is_empty() {
        return Ok(Unanchored::Kept(format!(
            "uncommitted or untracked files ({} paths)",
            changed.lines().count()
        )));
    }
    if !git_in(path, &["stash", "list"])?.is_empty() {
        return Ok(Unanchored::Kept("a stash".into()));
    }
    let tips = git_in(
        path,
        &[
            "for-each-ref",
            "--format=%(refname:short) %(objectname)",
            "refs/heads",
        ],
    )?;
    for (branch, tip) in tips.lines().filter_map(|line| line.split_once(' ')) {
        // The mirror answers only for commits it has; a missing object is not on the remote.
        let on_remote = git_in(
            mirror,
            &[
                "for-each-ref",
                "--count=1",
                "--contains",
                tip,
                "refs/heads",
                "refs/remotes",
            ],
        )
        .is_ok_and(|found| !found.is_empty());
        if !on_remote {
            return Ok(Unanchored::Kept(format!(
                "branch {branch} at {tip}, which is not on the remote"
            )));
        }
    }
    let (zone, name) = match (path.parent(), path.file_name()) {
        (Some(zone), Some(name)) => (zone, name.to_string_lossy().into_owned()),
        _ => return Err(format!("checkout path {} has no parent", path.display())),
    };
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|e| format!("system clock before 1970: {e}"))?
        .as_secs();
    let retired = zone.join(".retired").join(format!("{name}-{stamp}"));
    std::fs::create_dir_all(zone.join(".retired"))
        .map_err(|e| format!("create {}: {e}", zone.join(".retired").display()))?;
    std::fs::rename(path, &retired).map_err(|e| {
        format!(
            "move {} aside to {}: {e}",
            path.display(),
            retired.display()
        )
    })?;
    Ok(Unanchored::Retired(retired))
}
