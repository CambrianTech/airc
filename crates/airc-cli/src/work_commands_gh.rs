//! GitHub operations — PR creation + linking + base-branch resolution.
//! Currently shells out via `std::process::Command::new("gh")`; card
//! dec35ec7 will migrate these to the typed `GhClient` trait from
//! `airc-lib::tools` once that lands.
//!
//! Card c0bd865c phase 4 (c7bdf2df). The extracted handlers:
//!
//!   - `open_pr_and_link` — best-effort: from a card's worktree,
//!     run `gh pr create` against the configured base, parse the
//!     created PR's URL/number, and emit `PullRequestLinked` so the
//!     projection picks it up.
//!   - `gh_default_branch` — query github for the repo's GitHub
//!     default branch; kept for back-compat (no longer the source
//!     of truth for the per-card base, see card 28f1440c).
//!   - `extract_pr_number` — pure parser for `gh pr create`'s URL
//!     output.
//!   - `pr_create_base_branch` — the integration branch the merger
//!     opens PRs against (rust-rewrite). Hardcoded per card 28f1440c.

use crate::lease;

/// Best-effort: open a GitHub PR for the work card's worktree branch
/// and link it via `WorkEvent::PullRequestLinked`. Returns Err so
/// run_state can surface as a warning without aborting the state
/// transition.
pub(crate) async fn open_pr_and_link(
    airc: &airc_lib::Airc,
    card_id: airc_lib::WorkCardId,
) -> Result<(), Box<dyn std::error::Error>> {
    use airc_work::model::{BranchName, PullRequestRef};

    let board = airc.work_board(usize::MAX).await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} not visible in board projection"))?;
    if card.pull_request.is_some() {
        // Already linked; nothing to do. Re-running `state review` on
        // a card that's already been reviewed is a no-op for the PR
        // side.
        return Ok(());
    }

    let short: String = card.card_id.to_string().chars().take(8).collect();
    let lease_root = lease::lease_root()
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    let worktree_path = lease_root.join(&short);
    if !worktree_path.exists() {
        return Err(format!(
            "no worktree at {} — claim was made before card d1b2798d shipped, or with --no-lease-required (re-claim from a worktree to get auto-PR on review)",
            worktree_path.display()
        )
        .into());
    }
    let worktree_str = worktree_path.to_string_lossy().to_string();

    // gh pr create — pass --title + --body explicitly from the HEAD
    // commit's metadata. `--fill` SOUNDS right but its heuristic
    // sometimes falls back to a slugified branch name as the title
    // (observed live on cards 53698eb9 and ef168afe — branch
    // `53698eb9/agents-md-encode-engineering-staff-not-a` became
    // PR title `53698eb9/agents md encode engineering staff not a`,
    // which stripped the `docs(agents):` prefix the canary-gate's
    // bypass regex needs). Reading the commit subject directly is
    // deterministic and matches what the author actually wrote.
    //
    // Card a4fe899f: `gh` does NOT accept `-C` (that's git's flag).
    // The cwd has to be set via `Command::current_dir(...)` so gh's
    // own repo-resolution (which scans `git remote get-url origin`
    // from cwd) picks the worktree's branch. Without this, gh
    // silently ran against whatever the shell's cwd was at invoke
    // time — usually the wrong repo — and the whole Review-state →
    // PR-link pipeline failed (best-effort warning was swallowed).
    let subject = crate::work_commands_git::git_show_format(&worktree_str, "%s")?;
    let body = crate::work_commands_git::git_show_format(&worktree_str, "%b")?;
    // Card 70e87d33: resolve --base PER REPO. Card 812b5a1b pinned a
    // single global base (rust-rewrite) — correct for airc, but it
    // BROKE every other repo: continuum has no rust-rewrite branch, so
    // `gh pr create --base rust-rewrite` failed, no PullRequestLinked
    // fired, and the merger never saw the PR (flywheel stall on
    // #1469/#1470/#1471). configured_base_branch() returns the repo's
    // pinned integration branch (airc→rust-rewrite, continuum→canary);
    // repos with no override fall back to their actual GitHub default
    // branch. This is deliberately NOT a blanket default-branch switch
    // — that would re-break airc by landing PRs on main.
    let base_branch = match configured_base_branch(&card.repo) {
        Some(base) => base,
        None => gh_default_branch(&worktree_str)?,
    };
    let create_out = std::process::Command::new("gh")
        .current_dir(&worktree_str)
        .args([
            "pr",
            "create",
            "--title",
            subject.trim(),
            "--body",
            body.trim(),
            "--base",
            base_branch.as_str(),
        ])
        .output()?;
    if !create_out.status.success() {
        return Err(format!(
            "gh pr create failed: {}",
            String::from_utf8_lossy(&create_out.stderr).trim()
        )
        .into());
    }
    let stdout = String::from_utf8(create_out.stdout)?;
    let pr_url = stdout.trim().lines().last().unwrap_or("").trim();
    let pr_number = extract_pr_number(pr_url)
        .ok_or_else(|| format!("could not parse PR number from gh output: {pr_url}"))?;

    // Resolve head from the worktree's git state. Base is the same
    // pinned value we passed to gh — the projection must record what
    // we actually opened the PR against, not gh's repo default.
    let head_branch = crate::work_commands_git::git_rev_parse_branch(&worktree_str)?;

    let pull_request = PullRequestRef {
        repo: card.repo.clone(),
        number: pr_number,
        head: BranchName::new(head_branch)?,
        base: BranchName::new(base_branch)?,
    };
    airc.link_card_pull_request(airc_lib::LinkCardPullRequest {
        card_id,
        pull_request,
    })
    .await?;

    println!("pull_request: {pr_url}");

    // Card ad7e100b Sub-C: with PR linked, spawn a sibling review
    // card so any peer (other than the author) can claim it and
    // review the diff. Best-effort and idempotent — a spawn failure
    // here must not undo the state transition or the PR link, and
    // re-running `state review` on a card whose review card already
    // exists is a no-op.
    if let Err(error) = crate::work_commands::auto_spawn_review_card(airc, card_id, pr_url).await {
        eprintln!("airc: review card auto-spawn skipped — {error}");
    }

    Ok(())
}
/// Extract the PR number from a gh pr create URL line like
/// `https://github.com/owner/repo/pull/123`. Returns None for any
/// shape we don't recognise (so callers can degrade gracefully).
pub(crate) fn extract_pr_number(url: &str) -> Option<u64> {
    let trimmed = url.trim();
    let tail = trimmed.rsplit('/').next()?;
    tail.parse::<u64>().ok()
}
/// Card 70e87d33 — resolve the integration branch a card's PR opens
/// against, PER REPO. Returns `Some(branch)` for repos whose
/// integration branch is pinned (and is NOT their GitHub default), or
/// `None` to signal "use the repo's GitHub default branch".
///
/// History: card 28f1440c hardcoded a single global `rust-rewrite`
/// base because airc's substrate work lives on rust-rewrite, not its
/// GitHub default (`main`). Correct for airc, wrong for every other
/// repo — continuum has no `rust-rewrite` branch, so `gh pr create
/// --base rust-rewrite` failed, `PullRequestLinked` never fired, and
/// the merger never saw the PR. The fix is per-repo, NOT a blanket
/// default-branch switch (which would re-break airc by landing PRs on
/// main — the exact fallback 28f1440c set out to refuse).
///
/// `AIRC_PR_BASE` overrides everything (tests / one-offs). The carded
/// end state surfaces this as `.airc/work.toml` per-repo config; for
/// now the two known consumer repos are pinned inline.
pub(crate) fn configured_base_branch(repo: &airc_work::RepoId) -> Option<String> {
    if let Ok(base) = std::env::var("AIRC_PR_BASE") {
        let base = base.trim();
        if !base.is_empty() {
            return Some(base.to_string());
        }
    }
    match repo.as_str() {
        "CambrianTech/airc" => Some("rust-rewrite".to_string()),
        "CambrianTech/continuum" => Some("canary".to_string()),
        _ => None,
    }
}

/// Card 70e87d33 (part b) — retroactively link an ALREADY-OPEN PR to a
/// card by emitting `PullRequestLinked`. The auto-link in
/// `open_pr_and_link` only fires when `airc work state review` itself
/// creates the PR; a PR opened manually (e.g. while the base-default
/// bug blocked the auto path, as on #1471/#1472) has no link, so the
/// merger never picks it up. This closes that gap: `airc work link
/// <card> --pr <n>` reads the PR's real head/base from `gh` and links
/// it, after which the merger's gate sees a linked PR and can merge.
///
/// Idempotent: re-linking a card that already has a PR is a no-op.
pub(crate) async fn link_existing_pr(
    airc: &airc_lib::Airc,
    card_id: airc_lib::WorkCardId,
    pr_number: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use airc_work::model::{BranchName, PullRequestRef};

    let board = airc.work_board(usize::MAX).await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} not visible in board projection"))?;
    if let Some(existing) = &card.pull_request {
        println!(
            "pull_request already linked: card={card_id} pr=#{} ({})",
            existing.number,
            existing.repo.as_str()
        );
        return Ok(());
    }
    let repo = card.repo.clone();

    // Read the PR's actual head/base/state from GitHub. `--repo` is
    // explicit (not cwd-derived) so this works from anywhere, including
    // a card whose worktree was already cleaned up.
    let out = std::process::Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_number.to_string(),
            "--repo",
            repo.as_str(),
            "--json",
            "state,headRefName,baseRefName",
            "--jq",
            ".state + \"\\t\" + .headRefName + \"\\t\" + .baseRefName",
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "gh pr view #{pr_number} ({}) failed: {}",
            repo.as_str(),
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    let line = String::from_utf8(out.stdout)?;
    let mut fields = line.trim().split('\t');
    let state = fields.next().unwrap_or("").trim().to_string();
    let head = fields.next().unwrap_or("").trim().to_string();
    let base = fields.next().unwrap_or("").trim().to_string();
    if head.is_empty() || base.is_empty() {
        return Err(format!(
            "could not parse head/base for PR #{pr_number} ({}) — got state={state:?}",
            repo.as_str()
        )
        .into());
    }
    if state != "OPEN" {
        // Linking a closed/merged PR is allowed (records history) but
        // the merger won't act on it — flag so the caller isn't
        // surprised the flywheel stays put.
        eprintln!("airc: warning — PR #{pr_number} state is {state}, not OPEN");
    }

    let pull_request = PullRequestRef {
        repo,
        number: pr_number,
        head: BranchName::new(head)?,
        base: BranchName::new(base)?,
    };
    airc.link_card_pull_request(airc_lib::LinkCardPullRequest {
        card_id,
        pull_request,
    })
    .await?;
    println!("pull_request_linked: card={card_id} pr=#{pr_number}");
    Ok(())
}

pub(crate) fn gh_default_branch(worktree: &str) -> Result<String, Box<dyn std::error::Error>> {
    // Card a4fe899f: `gh` does NOT accept `-C`; set cwd via
    // `Command::current_dir(...)` so `gh repo view` resolves the
    // worktree's origin remote, not the shell cwd's.
    let out = std::process::Command::new("gh")
        .current_dir(worktree)
        .args([
            "repo",
            "view",
            "--json",
            "defaultBranchRef",
            "--jq",
            ".defaultBranchRef.name",
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "gh repo view failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
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
        // No override env: pinned repos resolve to their integration
        // branch (NOT their GitHub default of `main`).
        std::env::remove_var("AIRC_PR_BASE");
        assert_eq!(
            configured_base_branch(&repo("CambrianTech/airc")).as_deref(),
            Some("rust-rewrite"),
            "airc must pin rust-rewrite, never fall through to main"
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
            Some("rust-rewrite"),
            "blank AIRC_PR_BASE must not shadow the pinned base"
        );
        std::env::remove_var("AIRC_PR_BASE");
    }
}
