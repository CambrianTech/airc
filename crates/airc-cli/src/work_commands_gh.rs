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
    let subject = crate::work_commands::git_show_format(&worktree_str, "%s")?;
    let body = crate::work_commands::git_show_format(&worktree_str, "%b")?;
    let create_out = std::process::Command::new("gh")
        .current_dir(&worktree_str)
        .args([
            "pr",
            "create",
            "--title",
            subject.trim(),
            "--body",
            body.trim(),
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

    // Resolve head/base from the worktree's git state.
    let head_branch = crate::work_commands::git_rev_parse_branch(&worktree_str)?;
    let base_branch = gh_default_branch(&worktree_str).unwrap_or_else(|_| "main".to_string());

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
/// Card 28f1440c — the integration branch every per-card PR opens
/// against. Hardcoded to the airc substrate's working branch for
/// MVP; a follow-up surfaces this as configurable (e.g. via
/// `.airc/work.toml`) for consumers whose integration branch is
/// different.
///
/// MUST NOT change to a runtime read of `gh repo view --json defaultBranchRef` —
/// that returns the repo's GitHub `default_branch` (`main` on this
/// repo today), and every PR landing on `main` bypasses
/// `rust-rewrite`'s substrate work. The whole point of this card
/// is to refuse that fallback.
// Dead-code today because `open_pr_and_link` above does not pass
// `--base` to `gh pr create` — substrate kink that opens every
// auto-spawned PR against the github default branch (main) instead
// of rust-rewrite. Wiring this in is carded as a follow-up
// (812b5a1b) so that fix can land independently of fae3c28e's
// review-only close-flow work.
#[allow(dead_code)]
pub(crate) fn pr_create_base_branch() -> &'static str {
    "rust-rewrite"
}

#[allow(dead_code)]
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
