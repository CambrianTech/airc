//! Repository integration policy shared by PR and worktree allocation consumers.

/// Card 70e87d33 — resolve the integration branch a card's PR opens
/// against, PER REPO. Returns `Some(branch)` for repos whose
/// integration branch is pinned (and is NOT their GitHub default), or
/// `None` to signal "use the repo's GitHub default branch".
///
/// History: card 28f1440c hardcoded a single global `rust-rewrite`
/// base because airc's substrate work lived on rust-rewrite, not its
/// GitHub default (`main`). Correct for airc then, wrong for every other
/// repo — continuum has no `rust-rewrite` branch, so `gh pr create
/// --base rust-rewrite` failed, `PullRequestLinked` never fired, and
/// the merger never saw the PR. The fix is per-repo, NOT a blanket
/// default-branch switch (which would land PRs on main — the exact
/// fallback 28f1440c set out to refuse).
///
/// Card 5e04ab56: airc's integration branch is now `canary`, and
/// `rust-rewrite` no longer exists on the remote — so the airc pin
/// itself had gone stale and `airc work state <card> review` could not
/// open a PR for the airc repo at all ("No commits between rust-rewrite
/// and <branch>, Base ref must be a branch"), i.e. the substrate's own
/// disciplined review path was broken for its own repo. The invariant
/// 28f1440c protects is unchanged: airc pins its integration branch and
/// never falls through to `main`; only the branch's name moved.
///
/// `AIRC_PR_BASE` overrides everything (tests / one-offs). The carded
/// end state surfaces this as `.airc/work.toml` per-repo config; for
/// now the two known consumer repos are pinned inline.
pub fn configured_base_branch(repo: &airc_work::RepoId) -> Option<String> {
    if let Ok(base) = std::env::var("AIRC_PR_BASE") {
        let base = base.trim();
        if !base.is_empty() {
            return Some(base.to_string());
        }
    }
    match repo.as_str() {
        "CambrianTech/airc" => Some("canary".to_string()),
        "CambrianTech/continuum" => Some("canary".to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_work::RepoId;

    fn repo(key: &str) -> RepoId {
        RepoId::new(key).expect("test repo key valid")
    }

    /// Card 70e87d33: the per-repo base resolver. Kept in ONE test so
    /// the process-global `AIRC_PR_BASE` env var can't race a sibling
    /// test running in parallel in the same binary.
    #[test]
    fn configured_base_branch_is_per_repo_with_env_override_and_default_fallback() {
        temp_env::with_var("AIRC_PR_BASE", None::<&str>, || {
            // No override env: pinned repos resolve to their integration
            // branch (NOT their GitHub default of `main`).
            std::env::remove_var("AIRC_PR_BASE");
            assert_eq!(
                configured_base_branch(&repo("CambrianTech/airc")).as_deref(),
                Some("canary"),
                "airc must pin its integration branch (canary — card 5e04ab56 \
             retired rust-rewrite), never fall through to main"
            );
            assert_eq!(
                configured_base_branch(&repo("CambrianTech/continuum")).as_deref(),
                Some("canary"),
                "continuum must pin canary — the bug this card fixes"
            );

            // Unknown repo: None signals the caller to use the repo's real
            // GitHub default branch (the safe, correct generic fallback).
            assert_eq!(
                configured_base_branch(&repo("SomeOrg/unknown-repo")),
                None,
                "unknown repos defer to their GitHub default, not a hardcoded base"
            );

            // Env override wins for every repo (tests / one-off retarget).
            std::env::set_var("AIRC_PR_BASE", "release/x");
            assert_eq!(
                configured_base_branch(&repo("CambrianTech/airc")).as_deref(),
                Some("release/x"),
                "AIRC_PR_BASE must override even a pinned repo"
            );
            assert_eq!(
                configured_base_branch(&repo("SomeOrg/unknown-repo")).as_deref(),
                Some("release/x"),
                "AIRC_PR_BASE must override the default-branch fallback too"
            );
            // Blank/whitespace env is ignored (not treated as a real base).
            std::env::set_var("AIRC_PR_BASE", "   ");
            assert_eq!(
                configured_base_branch(&repo("CambrianTech/airc")).as_deref(),
                Some("canary"),
                "blank AIRC_PR_BASE must not shadow the pinned base"
            );
            std::env::remove_var("AIRC_PR_BASE");
        });
    }
}
