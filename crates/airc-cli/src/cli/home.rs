//! Which scope (`<home>`) a command uses: `--home`/`--here`, `$AIRC_HOME`, the
//! enclosing `.airc`, the git project root, then the machine-account home.

use std::path::{Path, PathBuf};

/// Default home directory for persisted identity + IPC state.
///
/// Resolution order:
///   1. `$AIRC_HOME` → explicit scope override.
///   2. First `.airc` ancestor when cwd is inside a scope.
///   3. Git project root `.airc` when cwd is inside a worktree.
///   4. Canonical machine-account home (`$HOME/.airc`) — a rootless cwd
///      uses the user's real identity, never a throwaway `./.airc`
///      scratch scope (seam #1). `./.airc` only as a last resort when no
///      `$HOME`/`$USERPROFILE` exists.
///
/// Account-wide state still lives under the canonical machine account
/// home (`$HOME/.airc`) inside `airc-lib`; this default is the
/// consumer/project scope. That preserves the original public contract:
/// running `airc join` in a repo uses that repo's `.airc`.
pub fn default_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("AIRC_HOME") {
        return PathBuf::from(home);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    default_home_dir_for(&cwd)
}

/// Resolve the explicit home override from the global `--home` / `--here`
/// flags. `--home <path>` wins (and clap forbids passing both). `--here`
/// is the ergonomic shortcut for `AIRC_HOME=$PWD/.airc` (seam #1): the
/// intentional opt-in to a cwd-LOCAL scope. Returns `None` when neither
/// is set — the caller then falls back to [`default_home_dir`] (which
/// itself honours `$AIRC_HOME`, then the machine-account / git-project
/// resolution). Pure: `cwd` is injected so tests need no real `$PWD`.
pub fn explicit_home_override(
    home_flag: Option<PathBuf>,
    here_flag: bool,
    cwd: &Path,
) -> Option<PathBuf> {
    if let Some(home) = home_flag {
        return Some(home);
    }
    if here_flag {
        return Some(cwd.join(".airc"));
    }
    None
}

fn default_home_dir_for(cwd: &Path) -> PathBuf {
    let machine_account = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".airc"));
    default_home_dir_for_with(cwd, machine_account.as_deref(), &git_main_working_tree)
}

/// Card a1b4552a — pure-function half of the resolution so tests can
/// drive it with synthetic paths instead of needing a real `$HOME`
/// and git repo. `machine_account_home` is the path we MUST NOT
/// resolve to (typically `$HOME/.airc`); `git_main_working_tree_fn`
/// resolves the main working tree of a git checkout from any of its
/// worktrees (production: shells `git rev-parse --git-common-dir`).
fn default_home_dir_for_with(
    cwd: &Path,
    machine_account_home: Option<&Path>,
    git_main_working_tree_fn: &dyn Fn(&Path) -> Option<PathBuf>,
) -> PathBuf {
    for ancestor in cwd.ancestors() {
        let matches_dotairc = ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".airc");
        if !matches_dotairc {
            continue;
        }
        // Card a1b4552a: SKIP the machine-account home itself when
        // walking ancestors. `~/.airc` is a SYSTEM-LEVEL airc state
        // directory holding the singleton machine-account identity +
        // the daemon socket — not a per-project agent scope. Agents
        // who `cd ~/.airc/worktrees/<short>/` (the d1b2798d auto-spawn
        // workflow) would otherwise resolve home here and silently
        // borrow the machine-account's identity, attributing their
        // actions to whoever owns the singleton row. Reproducibly
        // observed today: peer 9bb24964 claiming cards from a
        // spawned worktree → board shows the claims under peer
        // cdff6a9d (machine-account-owner) instead.
        if Some(ancestor) == machine_account_home {
            continue;
        }
        return ancestor.to_path_buf();
    }
    // Card a1b4552a: when cwd is inside a git worktree spawned by
    // `airc work claim` (worktrees live under `~/.airc/worktrees/`),
    // the canonical project scope is the MAIN repo's `.airc`, not
    // the worktree's own (worktrees rarely contain their own `.airc`).
    // `git rev-parse --git-common-dir` points at the MAIN repo's
    // `.git/` for any worktree; its parent is the main working tree.
    // Seam #1 (solidification doc): a cwd that is neither inside a git
    // project NOR under an existing `.airc` scope must NOT mint a
    // throwaway `cwd/.airc` scope. That scratch home gets its own
    // identity, which then publishes a phantom beacon to the mesh — the
    // observed `/tmp` `57059a56` ghost. Fall back to the canonical
    // MACHINE-ACCOUNT home (the user's real identity) instead. Explicit
    // cwd-scoping stays available via `$AIRC_HOME` (a future `--here`
    // flag is the ergonomic shortcut; tracked as a follow-up). Only the
    // truly-rootless case (no `$HOME`/`$USERPROFILE`) keeps the old
    // cwd-local behavior as a last resort.
    git_main_working_tree_fn(cwd)
        .map(|root| root.join(".airc"))
        .or_else(|| git_toplevel(cwd).map(|root| root.join(".airc")))
        .or_else(|| machine_account_home.map(|h| h.to_path_buf()))
        .unwrap_or_else(|| cwd.join(".airc"))
}

fn git_main_working_tree(cwd: &Path) -> Option<PathBuf> {
    let output = airc_core::process::background("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let common_dir = text.trim();
    if common_dir.is_empty() {
        return None;
    }
    let common_path = PathBuf::from(common_dir);
    let abs = if common_path.is_absolute() {
        common_path
    } else {
        cwd.join(common_path).canonicalize().ok()?
    };
    // common dir is `<main-working-tree>/.git`; parent is the main
    // working tree.
    abs.parent().map(|p| p.to_path_buf())
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = airc_core::process::background("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let root = text.trim();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

#[cfg(test)]
mod tests {
    use super::{default_home_dir_for, explicit_home_override};
    use std::path::{Path, PathBuf};

    /// what this catches (seam #1 `--here`): the override precedence.
    /// `--home` wins; `--here` resolves to the cwd-LOCAL `.airc` (the
    /// `AIRC_HOME=$PWD/.airc` shortcut); neither set returns None so the
    /// caller falls through to the machine-account / git-project default.
    /// If `--here` ever resolved to the machine-account home instead of
    /// `$PWD/.airc`, the whole point of the flag (an explicit local
    /// scope) would be lost — this pins it.
    #[test]
    fn explicit_home_override_precedence() {
        let cwd = Path::new("/work/project");

        // --home wins outright.
        assert_eq!(
            explicit_home_override(Some(PathBuf::from("/custom/home")), false, cwd),
            Some(PathBuf::from("/custom/home"))
        );
        // (and still wins even if --here were somehow also set; clap
        // forbids the combination, but the resolver is unambiguous.)
        assert_eq!(
            explicit_home_override(Some(PathBuf::from("/custom/home")), true, cwd),
            Some(PathBuf::from("/custom/home"))
        );
        // --here → cwd-local .airc, NOT the machine-account home.
        assert_eq!(
            explicit_home_override(None, true, cwd),
            Some(PathBuf::from("/work/project/.airc"))
        );
        // neither → None (caller falls back to default_home_dir).
        assert_eq!(explicit_home_override(None, false, cwd), None);
    }

    /// what this catches: `--home` and `--here` are mutually exclusive at
    /// the CLAP layer (`conflicts_with = "home"`). The resolver test above
    /// proves the value precedence, but not the parse-time guard — a
    /// future arg-id rename could silently drop the `conflicts_with` and
    /// let both be passed, with the resolver then quietly preferring
    /// `--home` while the user thinks `--here` took effect. This pins the
    /// guard: passing both must be a hard parse error.
    #[test]
    fn default_home_uses_enclosing_airc_scope() {
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(default_home_dir_for(&nested), scope);
    }

    #[test]
    fn default_home_uses_git_project_root_scope() {
        // Hermetic: STUB the git-main-working-tree resolver instead of
        // shelling out to real `git` in a temp repo. The earlier version
        // called the real `default_home_dir_for` (which shells `git
        // rev-parse`), and that is environment-fragile under parallel
        // unit tests + CI runners: git's dubious-ownership / safe.directory
        // check can intermittently REFUSE a freshly-init'd temp repo (and
        // a sibling test mutating the process-global `$HOME`/cwd can race
        // git's config read), so `git_main_working_tree` returns None and
        // `default_home_dir_for` falls back to the machine-account home —
        // a non-deterministic red that blocked unrelated PRs (the #1230
        // diagnostic assertion is what surfaced exactly this fallback).
        //
        // The mapping logic under test — "a resolved git main working
        // tree ⇒ <tree>/.airc, NOT the machine-account home" — is what
        // this pins, deterministically, via the same `..._with` stub seam
        // the sibling default_home tests use. The git-shell integration
        // itself is exercised in production + the daemon integration
        // tests, not in a flaky parallel unit test.
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let repo = PathBuf::from("/Users/test/Development/myproj");
        let nested = repo.join("src").join("inner");
        let machine_account = PathBuf::from("/Users/test/.airc");
        // No `.airc` ancestor of `nested`; the resolver must fall through
        // to the git main working tree (stubbed) and scope to <repo>/.airc.
        let stub = |_: &Path| -> Option<PathBuf> { Some(repo.clone()) };
        let resolved = default_home_dir_for_with(&nested, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved,
            repo.join(".airc"),
            "a resolved git main working tree must scope home to <tree>/.airc, \
             not fall back to the machine-account home"
        );
    }

    #[test]
    fn default_home_skips_machine_account_home_when_inside_worktrees_subdir() {
        // Card a1b4552a — the leak we caught live today. Without this
        // guard an agent who `cd ~/.airc/worktrees/<short>/` per the
        // d1b2798d auto-spawn workflow would resolve home to ~/.airc
        // and borrow the machine-account identity. Test pins that
        // ancestor walk SKIPS the machine-account ~/.airc and falls
        // through to git_main_working_tree (the main repo's .airc).
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let machine_account = PathBuf::from("/Users/test/.airc");
        let cwd = machine_account.join("worktrees").join("abc12345");
        // Stub git_main_working_tree_fn to a known main repo working tree.
        let main_repo = PathBuf::from("/Users/test/Development/airc");
        let stub = |_: &Path| -> Option<PathBuf> { Some(main_repo.clone()) };
        let resolved = default_home_dir_for_with(&cwd, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved,
            main_repo.join(".airc"),
            "must NOT resolve to machine-account ~/.airc when cwd is under its worktrees dir",
        );
    }

    #[test]
    fn default_home_still_resolves_project_scope_when_not_under_machine_account() {
        // Sanity: a nested cwd that has an enclosing project .airc that
        // is NOT the machine-account home resolves correctly (regression
        // guard — we don't want the fix above to skip legitimate
        // project scopes too).
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let machine_account = PathBuf::from("/Users/test/.airc");
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&nested, Some(machine_account.as_path()), &stub);
        assert_eq!(resolved, scope);
    }

    #[test]
    fn default_home_falls_through_to_cwd_airc_when_nothing_else_resolves() {
        // Final fallback: no ancestor .airc, no git context — cwd's
        // own .airc is the answer. Pre-existing behaviour, pinned so
        // the refactor preserves it.
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let root = tempfile::TempDir::new().unwrap();
        let cwd = root.path().join("standalone");
        std::fs::create_dir_all(&cwd).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&cwd, None, &stub);
        assert_eq!(resolved, cwd.join(".airc"));
    }

    // what this catches: seam #1 — a rootless cwd (no enclosing `.airc`,
    // no git context) with a known machine-account home must resolve to
    // that canonical home, NOT mint a throwaway `cwd/.airc` scratch scope
    // (the `/tmp` `57059a56` phantom-beacon ghost). Mutation check:
    // reverting the machine-account fallback resolves to cwd/.airc and
    // this fails.
    #[test]
    fn default_home_rootless_cwd_uses_machine_account_not_throwaway() {
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let root = tempfile::TempDir::new().unwrap();
        let cwd = root.path().join("nowhere"); // no .airc, no git
        std::fs::create_dir_all(&cwd).unwrap();
        let machine_account = PathBuf::from("/Users/test/.airc");
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&cwd, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved, machine_account,
            "rootless cwd must use the canonical machine-account home, not a throwaway cwd/.airc"
        );
        assert_ne!(resolved, cwd.join(".airc"), "must NOT mint a scratch scope");
    }
}
