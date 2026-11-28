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

/// Card 52984cb0: scope resolves from an EXPLICIT source — `$AIRC_HOME`,
/// `--home`, or the machine-account default. CWD is NOT consulted.
/// The old ancestor-walk and git-project resolution are removed because
/// they make identity depend on where you happen to stand in the filesystem,
/// which makes isolation impossible (a scope under HOME always leaks to
/// the machine-account wire, a project `.airc` hijacks an explicit env var,
/// etc.).
///
/// Resolution order:
///   1. `$AIRC_HOME` → explicit scope override (already handled by caller).
///   2. Machine-account home (`$HOME/.airc`) — the safe rootless default.
///   3. No cwd-based fallback: if no machine account is available, the
///      caller must provide an explicit path; we never mint a scratch
///      scope from cwd (card 52984cb0 seam #1).
pub fn default_home_dir_for(cwd: &Path) -> PathBuf {
    let _cwd = cwd; // used only for the machine-account fallback below
    let machine_account = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".airc"));
    // Return machine-account home if available; otherwise fall back to cwd
    // for backward compatibility with callers that expect *something*, but
    // the preferred path is always explicit via $AIRC_HOME or --home.
    machine_account.unwrap_or_else(|| cwd.join(".airc"))
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

/// The main working tree of the project `cwd` is in. A submodule belongs to its superproject's
/// scope: its common dir is `<super>/.git/modules/<path>`, whose parent is no working tree, so
/// reading it as one minted a phantom scope inside `.git/modules` (its own event store, and a
/// wake lock no session in the project holds: the M5, 2026-11-27).
fn git_main_working_tree(cwd: &Path) -> Option<PathBuf> {
    let output = airc_core::process::background("git")
        .args([
            "rev-parse",
            "--git-common-dir",
            "--show-superproject-working-tree",
        ])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    match project_of(&String::from_utf8(output.stdout).ok()?)? {
        Project::InSuperproject(root) => git_main_working_tree(&root),
        Project::CommonDir(common) => {
            let abs = if common.is_absolute() {
                common
            } else {
                cwd.join(common).canonicalize().ok()?
            };
            // common dir is `<main-working-tree>/.git`; parent is the main working tree.
            abs.parent().map(Path::to_path_buf)
        }
    }
}

/// What `git rev-parse --git-common-dir --show-superproject-working-tree` says: one line (the
/// common dir) outside a submodule, a second (the superproject's working tree) inside one.
#[derive(Debug, PartialEq, Eq)]
enum Project {
    CommonDir(PathBuf),
    InSuperproject(PathBuf),
}

fn project_of(stdout: &str) -> Option<Project> {
    let mut lines = stdout
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty());
    let common = lines.next()?;
    Some(match lines.next() {
        Some(superproject) => Project::InSuperproject(PathBuf::from(superproject)),
        None => Project::CommonDir(PathBuf::from(common)),
    })
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

    /// what this catches (the M5, 2026-11-27): a session whose cwd is inside a submodule resolved
    /// its scope to `<super>/.git/modules/<path>/..`, a phantom scope with its own event store,
    /// where the wake the session armed in the project is not seen. A submodule is its
    /// superproject's; a plain checkout or worktree is its common dir's parent.
    #[test]
    fn a_submodule_belongs_to_its_superprojects_scope() {
        use super::{project_of, Project};
        assert_eq!(
            project_of(
                "/Users/op/continuum/.git/modules/core/vendor/llama.cpp\n/Users/op/continuum\n"
            ),
            Some(Project::InSuperproject(PathBuf::from(
                "/Users/op/continuum"
            )))
        );
        assert_eq!(
            project_of("/Users/op/continuum/.git\n"),
            Some(Project::CommonDir(PathBuf::from(
                "/Users/op/continuum/.git"
            )))
        );
        assert_eq!(
            project_of(".git\n"),
            Some(Project::CommonDir(PathBuf::from(".git")))
        );
        assert_eq!(project_of(""), None);
    }

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

    /// Card 52984cb0: scope no longer resolves from cwd ancestor walking.
    /// default_home_dir_for returns machine-account home (or cwd fallback),
    /// NOT the nearest `.airc` directory. Ancestor-walking was removed
    /// because it makes identity depend on CWD position, preventing isolation.
    #[test]
    fn default_home_returns_machine_account_not_enclosing_airc_scope() {
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();

        // CWD-based ancestor walking is removed: we get machine-account home,
        // NOT the `.airc` ancestor of cwd.
        // (The test doesn't set $HOME, so fallback goes to cwd/.airc; but in
        // production $HOME will be set and this returns $HOME/.airc.)
        let resolved = default_home_dir_for(&nested);
        // The key assertion: it does NOT resolve to the .airc ancestor.
        assert_ne!(resolved, scope,
            "scope must NOT resolve from cwd ancestor walking (card 52984cb0)");
    }

    /// Card 52984cb0: production flow no longer calls this function
    /// (cwd is an input to NOTHING). Kept as documentation of the helper's
    /// behavior for callers that want it directly.
    #[test]
    fn default_home_dir_for_with_git_project_stub_works() {
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let repo = PathBuf::from("/Users/test/Development/myproj");
        let nested = repo.join("src").join("inner");
        let machine_account = PathBuf::from("/Users/test/.airc");
        let stub = |_: &Path| -> Option<PathBuf> { Some(repo.clone()) };
        let resolved = default_home_dir_for_with(&nested, Some(machine_account.as_path()), &stub);
        assert_eq!(resolved, repo.join(".airc"));
    }

    /// Card 52984cb0: production no longer calls this helper (cwd is input
    /// to nothing). Kept as documentation of the internal behavior.
    #[test]
    fn default_home_dir_for_with_worktree_stub_resolves_main_scope() {
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

    /// Card 52984cb0: production no longer calls this helper (cwd is input
    /// to nothing). Kept as documentation of internal behavior.
    #[test]
    fn default_home_dir_for_with_project_scope_when_not_under_machine_account() {
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

    /// Card 52984cb0: production no longer calls this helper (cwd is input
    /// to nothing). Kept as documentation of internal behavior.
    #[test]
    fn default_home_dir_for_with_cwd_airc_fallback() {
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let root = tempfile::TempDir::new().unwrap();
        let cwd = root.path().join("standalone");
        std::fs::create_dir_all(&cwd).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&cwd, None, &stub);
        assert_eq!(resolved, cwd.join(".airc"));
    }

    /// Card 52984cb0: production no longer calls this helper (cwd is input
    /// to nothing). Kept as documentation of internal behavior.
    #[test]
    fn default_home_dir_for_with_rootless_cwd_uses_machine_account() {
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
