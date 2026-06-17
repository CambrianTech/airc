//! Binary staleness warning — print a banner when this `airc` binary
//! is older than `origin/canary` tip for the airc crates.
//!
//! Card f10c951e. The recurring failure mode: substrate fixes merge
//! into canary, but every agent's local `airc` binary lags
//! behind because nothing tells them to rebuild. We've hit this
//! repeatedly:
//!
//!   - my own `airc work release` released the WRONG claim because
//!     my binary was pre-#1024 (close-guard refactor)
//!   - Codex's first `airc join` failed because PATH preferred
//!     `~/.local/bin/airc` (stale 5ffac8b) over `~/.cargo/bin/airc`
//!     (current 66a74a7) after `cargo install --force`
//!   - every per-card PR has had to be REST-patched at title/base
//!     because the binary that opened it pre-dated the fix card
//!
//! ## What this prints
//!
//! On every CLI invocation (cheap; cached for 5 minutes), check if
//! the binary's compile-time commit is behind `origin/canary`
//! for paths under `crates/airc-*`. If so, print a banner on stderr:
//!
//!   ⚠ airc binary is N commits behind canary — run `airc update`
//!     to rebuild and reinstall in place.
//!
//! Stderr (not stdout) keeps parseable command output clean. The
//! banner is **always informational** — never blocks command
//! execution. An out-of-date binary still works; the user just sees
//! a nudge to update.
//!
//! ## When it stays silent
//!
//!   - `build_info::is_unknown()` — release tarball / no git at
//!     compile time, can't compare; silent
//!   - neither the install-source marker NOR cwd resolves to an airc
//!     git working tree (e.g. a prebuilt-binary install with no source
//!     checkout) — can't git-compare; silent (a future slice can fall
//!     back to a GitHub-API tip comparison for source-less installs)
//!   - check ran successfully in the last 5 minutes and was already
//!     up-to-date; silent
//!   - `git` shell-out failed; silent (no false positives)
//!
//! ## Future: structured event
//!
//! Card 8864c548 (log-hygiene) wants every stderr write replaced
//! with `DiagnosticEvent`. The banner stays an `eprintln!` for now
//! because it's a user-facing nudge in the conventional "your tool
//! is stale" pattern (npm/brew/cargo also use stderr text). When
//! 8864c548 lands the convention, this converts.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// How long to trust the previous "binary is current" cache.
/// 5 minutes is short enough that a fresh substrate merge surfaces
/// in the next ~5 min, long enough that running `airc` 50 times
/// in a tight test loop doesn't shell out to git 50 times.
const CACHE_TTL: Duration = Duration::from_secs(300);

/// Run the check; print a banner on stderr if stale. Best-effort —
/// any I/O failure is silent so this never breaks the command.
pub fn warn_if_stale() {
    if crate::build_info::is_unknown() {
        return;
    }
    let Some(repo_root) = airc_repo_root() else {
        return;
    };
    if !cache_check_due(&repo_root) {
        return;
    }
    let current_commit = crate::build_info::COMMIT;
    if let Some(behind) = count_commits_behind(&repo_root, current_commit) {
        if behind == 0 {
            update_cache(&repo_root);
            return;
        }
        let label = if behind == 1 { "commit" } else { "commits" };
        // Suggest `airc update`, NOT `cargo install --path` — the latter
        // installs to ~/.cargo/bin/airc, a SECOND binary that shadows the
        // install.sh-managed ~/.local/bin/airc and caused the stale-binary
        // join failure documented above. `airc update` rebuilds and
        // reinstalls IN PLACE (same location, + marker + skills), so it
        // can't create a dual-binary split.
        eprintln!(
            "⚠ airc binary is {behind} {label} behind canary — run `airc update` \
             to rebuild + reinstall in place. If `airc update` hangs on an old \
             build (pre-#1218), do a one-time manual rebuild: \
             cd \"$(cat ~/.airc/install-source)\" && git checkout canary && \
             git pull && ./install.sh"
        );
        // Don't update the cache when stale — every run reminds.
    }
}

/// Find the airc repo's working tree from cwd. Returns None when:
///   - cwd is not in a git repo
///   - cwd's git repo isn't airc (heuristic: a `crates/airc-cli/`
///     directory exists)
fn airc_repo_root() -> Option<PathBuf> {
    // Prefer the recorded install-source checkout so the staleness check
    // works REGARDLESS OF CWD: a peer running `airc` from anywhere (not
    // sitting inside the airc clone) still learns its binary is stale.
    // This is the core of #1225 — the old cwd-only discovery was silent
    // for every peer not in the repo, so stale binaries went unnoticed.
    if let Ok(src) = crate::update_commands::install_source_dir() {
        if src.join("crates/airc-cli").is_dir() {
            return Some(src);
        }
    }
    // Fallback: cwd-based discovery (a dev working inside a clone with no
    // install-source marker set).
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let root = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if root.is_empty() {
        return None;
    }
    let root = PathBuf::from(root);
    if !root.join("crates/airc-cli").is_dir() {
        return None;
    }
    Some(root)
}

/// Count commits on `origin/canary` that landed AFTER
/// `current_commit` and touch any `crates/airc-*` path. Returns None
/// on git failure (silent — better than a false positive).
fn count_commits_behind(repo_root: &Path, current_commit: &str) -> Option<usize> {
    // Best-effort: refresh origin/canary so the count reflects the REAL
    // remote tip, not a stale local ref (the old check compared to
    // whatever origin/canary happened to be locally — useless if never
    // fetched). The whole staleness check is cache-gated (CACHE_TTL), so
    // this fetch runs at most once per TTL. Quiet + ignore failure
    // (offline is fine — we just compare against the last-known tip).
    let _ = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["fetch", "--quiet", "origin", "canary"])
        .output();
    let paths = [
        "crates/airc-cli",
        "crates/airc-lib",
        "crates/airc-core",
        "crates/airc-work",
        "crates/airc-store",
        "crates/airc-daemon",
        "crates/airc-ipc",
        "crates/airc-protocol",
        "crates/airc-transport",
        "crates/airc-identity",
        "crates/airc-trust",
        "crates/airc-diagnostics",
        "crates/airc-bus",
        "crates/airc-wire",
    ];
    let mut args: Vec<String> = vec![
        "rev-list".to_string(),
        "--count".to_string(),
        format!("{current_commit}..origin/canary"),
        "--".to_string(),
    ];
    for p in &paths {
        args.push((*p).to_string());
    }
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(&args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()?
        .trim()
        .parse::<usize>()
        .ok()
}

/// Cache file path for the "binary is current" sentinel. Per-user
/// under tmpdir so multi-user boxes don't collide; per-repo-root
/// hash so a developer with multiple airc clones gets per-clone
/// cache.
fn cache_path(repo_root: &Path) -> Option<PathBuf> {
    let tmp = std::env::temp_dir();
    let hash = simple_hash(repo_root.to_string_lossy().as_bytes());
    Some(tmp.join(format!("airc-staleness-{hash:x}.cache")))
}

fn simple_hash(bytes: &[u8]) -> u64 {
    // FNV-1a — small + stable + deterministic + no deps.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in bytes {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100_0000_01b3);
    }
    h
}

/// True when the cache says we should re-check (no cache, or cache
/// is older than [`CACHE_TTL`]).
fn cache_check_due(repo_root: &Path) -> bool {
    let Some(path) = cache_path(repo_root) else {
        return true;
    };
    let Ok(meta) = std::fs::metadata(&path) else {
        return true;
    };
    let Ok(modified) = meta.modified() else {
        return true;
    };
    let Ok(age) = SystemTime::now().duration_since(modified) else {
        return true;
    };
    age >= CACHE_TTL
}

/// Mark the cache as "checked just now and binary is current."
fn update_cache(repo_root: &Path) {
    let Some(path) = cache_path(repo_root) else {
        return;
    };
    let now_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let _ = std::fs::write(path, format!("{now_ms}"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn simple_hash_is_deterministic() {
        assert_eq!(simple_hash(b"airc"), simple_hash(b"airc"));
    }

    #[test]
    fn simple_hash_distinguishes_inputs() {
        assert_ne!(simple_hash(b"airc"), simple_hash(b"continuum"));
    }

    #[test]
    fn cache_path_differs_for_distinct_repo_roots() {
        let a = cache_path(Path::new("/Users/joel/Development/airc")).unwrap();
        let b = cache_path(Path::new("/Users/joel/Development/continuum")).unwrap();
        assert_ne!(a, b, "different repos get different cache files");
    }

    #[test]
    fn cache_check_due_when_no_cache_exists() {
        // A path that definitely has no cache file
        let unique = std::env::temp_dir().join(format!(
            "airc-staleness-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        assert!(
            cache_check_due(&unique),
            "no cache → check is due (first run)"
        );
    }

    #[test]
    fn cache_check_due_false_immediately_after_update() {
        let unique = std::env::temp_dir().join(format!(
            "airc-staleness-update-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        update_cache(&unique);
        assert!(
            !cache_check_due(&unique),
            "fresh cache → not yet time to recheck"
        );
        // Cleanup
        if let Some(p) = cache_path(&unique) {
            let _ = std::fs::remove_file(p);
        }
    }
}
