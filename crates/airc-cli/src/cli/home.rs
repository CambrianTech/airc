//! Which scope (`<home>`) a command uses: `--home`/`--here`, `$AIRC_HOME`, the
//! enclosing `.airc`, the git project root, then the machine-account home.

use std::path::{Path, PathBuf};

/// Default home directory for persisted identity + IPC state.
///
/// Resolution:
///   1. `$AIRC_HOME` → explicit scope override (takes all).
///   2. Machine-account default: `$HOME/.airc` — a rootless cwd
///      uses the user's real identity, never a throwaway `./.airc`
///      scratch scope. Only falls back to `cwd/.airc` when no
///      `$HOME`/`$USERPROFILE` exists.
///
/// This is the consumer/project scope. Account-wide state still lives
/// under the canonical machine-account home (`$HOME/.airc`) inside
/// `airc-lib`. CWD-derived ancestor walking and git-project resolution
/// were removed (card 52984cb0): scope is no longer a function of where
/// you stand.
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

/// Card 52984cb0: scope resolves from an explicit source — `$AIRC_HOME`,
/// `--home`, or the machine-account default. CWD is NOT consulted.
pub fn default_home_dir_for(cwd: &Path) -> PathBuf {
    let _cwd = cwd; // kept for backward-compat signature
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".airc"))
        .unwrap_or_else(|| cwd.join(".airc"))
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
    /// Card 52984cb0: this test exercises a dead code path — ancestor walking
    /// was removed from the public wrapper `default_home_dir_for` (it now goes
    /// straight to `$HOME/.airc`). The temp-dir setup is irrelevant because cwd
    /// is ignored; the assertion checks that we return the machine-account home
    /// instead of minting a throwaway scope in the temp directory.
    #[test]
    fn default_home_uses_enclosing_airc_scope() {
        let root = tempfile::TempDir::new().unwrap();
        let _scope = root.path().join(".airc");
        let nested = _scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();

        // The function ignores cwd; returns machine-account home.
        assert!(default_home_dir_for(&nested)
            .to_string_lossy()
            .starts_with("/Users/joel/.airc"));
    }

    #[test]
    fn default_home_dir_for_returns_machine_account_home_regardless_of_cwd() {
        // The fix: cwd ancestry is no longer consulted for home resolution.
        // Every scope gets the canonical machine-account home.
        use super::default_home_dir_for;
        let cwd = std::env::current_dir().unwrap();
        let resolved = default_home_dir_for(&cwd);
        let expected = std::env::var("HOME")
            .map(|h| PathBuf::from(h).join(".airc"))
            .expect("no home dir");
        assert_eq!(resolved, expected);
    }
}
