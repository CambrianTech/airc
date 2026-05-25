//! Worktree-lease ergonomics for the airc CLI.
//!
//! Durable rule (Joel, multi-project): `~/.airc/worktrees/` is the
//! lease zone. Agent lanes do their work inside leases so the main
//! checkout stays clean and parallel agents don't clobber each
//! other.
//!
//! Closes flaw #7 from GRID-SUBSTRATE-AUDIT #964 (unenforced
//! worktree leases). This module enforces the rule at the CLI
//! boundary because cwd is process state — keeping it out of the
//! substrate preserves the substrate-of-truth/SDK-composes/CLI-
//! consumes layering.
//!
//! Two checks:
//!
//! 1. **At claim time** — `airc work claim` refuses when cwd is not
//!    under `~/.airc/worktrees/`, unless `--no-lease-required`.
//!    This catches the common mistake of claiming a card while
//!    sitting in the main checkout.
//!
//! 2. **At heartbeat time** — `airc work heartbeat` does NOT refuse
//!    (the claim was already granted; the heartbeat just renews the
//!    lease). Instead it emits a typed `WorkspaceLeaseViolation`
//!    diagnostic so the substrate has a record. Drift away from a
//!    lease mid-task is worth observing.

use std::path::{Path, PathBuf};

/// The lease zone subdirectory under `$HOME`.
const LEASE_ZONE_RELATIVE: &str = ".airc/worktrees";

/// Resolve the lease zone path (`~/.airc/worktrees`). Returns
/// `None` only when neither `$HOME` nor `$USERPROFILE` is set — in
/// practice this never happens in normal terminal usage, but the
/// caller decides how to treat that case.
pub fn lease_root() -> Option<PathBuf> {
    home_dir().map(|home| home.join(LEASE_ZONE_RELATIVE))
}

/// Result of checking whether a path lives under the lease zone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseCheck {
    /// The path the caller asked about, canonicalised when possible.
    pub path: PathBuf,
    /// The lease zone we compared against, canonicalised when it
    /// exists on disk.
    pub lease_root: PathBuf,
    /// True when `path` is under `lease_root`.
    pub under_lease: bool,
}

/// Check whether `path` lives under the configured lease zone.
///
/// Reads `$HOME`/`$USERPROFILE` for the lease zone root. For tests
/// that need a synthetic lease zone, use [`check_path_against`].
pub fn check_path(path: &Path) -> std::io::Result<LeaseCheck> {
    let lease_root = lease_root()
        .ok_or_else(|| std::io::Error::other("HOME/USERPROFILE not set; cannot resolve ~/"))?;
    Ok(check_path_against(path, &lease_root))
}

/// Like [`check_path`] but with an explicit lease root, suitable
/// for tests that build a synthetic lease zone in a tempdir without
/// mutating process environment.
pub fn check_path_against(path: &Path, lease_root: &Path) -> LeaseCheck {
    // Canonicalise both sides so symlinks into the lease zone (and
    // /var → /private/var on macOS) compare correctly. Fall back to
    // the raw paths when canonicalisation fails (e.g. the path
    // doesn't exist yet) — in that case the prefix check still
    // produces a sensible answer for callers passing real cwds.
    let canonical_root = lease_root
        .canonicalize()
        .unwrap_or_else(|_| lease_root.to_path_buf());
    let canonical_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let under_lease = canonical_path.starts_with(&canonical_root);
    LeaseCheck {
        path: canonical_path,
        lease_root: canonical_root,
        under_lease,
    }
}

/// Convenience: check the current working directory.
pub fn check_current_dir() -> std::io::Result<LeaseCheck> {
    let cwd = std::env::current_dir()?;
    check_path(&cwd)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").map(PathBuf::from))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn path_inside_lease_zone_is_under_lease() {
        let tmp = TempDir::new().expect("home");
        let root = tmp.path().join(".airc/worktrees");
        let leased = root.join("my-lane");
        std::fs::create_dir_all(&leased).expect("create lane dir");

        let check = check_path_against(&leased, &root);
        assert!(
            check.under_lease,
            "lane dir {:?} should be under lease root {:?}",
            check.path, check.lease_root
        );
    }

    #[test]
    fn path_outside_lease_zone_is_not_under_lease() {
        let tmp = TempDir::new().expect("home");
        let root = tmp.path().join(".airc/worktrees");
        std::fs::create_dir_all(&root).expect("create lease root");
        let elsewhere = tmp.path().join("not-a-lease");
        std::fs::create_dir_all(&elsewhere).expect("create non-lease dir");

        let check = check_path_against(&elsewhere, &root);
        assert!(
            !check.under_lease,
            "non-lease dir {:?} should NOT be under lease root {:?}",
            check.path, check.lease_root
        );
    }

    #[test]
    fn nonexistent_lease_root_treats_existing_paths_as_not_under_lease() {
        let tmp = TempDir::new().expect("home");
        let root = tmp.path().join(".airc/worktrees");
        // Note: lease root never created.
        let elsewhere = tmp.path().join("anywhere");
        std::fs::create_dir_all(&elsewhere).expect("create dir");

        let check = check_path_against(&elsewhere, &root);
        assert!(!check.under_lease);
    }

    #[test]
    fn nested_lane_subdirectory_still_under_lease() {
        let tmp = TempDir::new().expect("home");
        let root = tmp.path().join(".airc/worktrees");
        let deep = root.join("lane/crates/airc-lib/src");
        std::fs::create_dir_all(&deep).expect("create deep dir");

        let check = check_path_against(&deep, &root);
        assert!(check.under_lease, "deep path inside a lane must count");
    }

    #[cfg(unix)]
    #[test]
    fn symlink_into_lease_zone_resolves_as_under_lease() {
        // Canonicalising both sides means a symlink that points into
        // ~/.airc/worktrees/ still passes — matches user intent
        // (working in the linked-to location).
        let tmp = TempDir::new().expect("home");
        let root = tmp.path().join(".airc/worktrees");
        let leased = root.join("lane");
        std::fs::create_dir_all(&leased).expect("create lane");
        let link = tmp.path().join("shortcut");
        std::os::unix::fs::symlink(&leased, &link).expect("symlink");

        let check = check_path_against(&link, &root);
        assert!(check.under_lease, "symlink into lease zone should resolve");
    }

    #[test]
    fn unrelated_sibling_path_is_not_under_lease_even_with_prefix_overlap() {
        // Guard against a naïve string-prefix check that would let
        // `~/.airc/worktrees-malicious/` match `~/.airc/worktrees/`.
        let tmp = TempDir::new().expect("home");
        let root = tmp.path().join(".airc/worktrees");
        std::fs::create_dir_all(&root).expect("create lease root");
        let sibling = tmp.path().join(".airc/worktrees-malicious");
        std::fs::create_dir_all(&sibling).expect("create sibling");

        let check = check_path_against(&sibling, &root);
        assert!(
            !check.under_lease,
            "sibling dir with overlapping prefix must not count as under lease"
        );
    }
}
