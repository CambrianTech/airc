//! Local `git` + repo-id helpers — spawn_claim_worktree and friends.
//! Currently shells out via `std::process::Command`; card dec35ec7
//! migrates these to the typed GitClient trait when that PR lands.
//!
//! Card cb8d8990 (phase 3 redo, was c63a1b6e). The same extraction
//! pattern as phases 1 (#1063) and 2 (#1064 in flight).

use crate::lease;

/// Best-effort: allocate `~/.airc/worktrees/<card_short>/` and a
/// branch `<card_short>/<slug>` off the current feature branch HEAD
/// so the agent who just claimed the card can `cd` into a clean,
/// isolated workspace.
///
/// Returns Err on any genuine failure (no git repo, no lease zone,
/// git command failure) so run_claim can surface it as a warning
/// without aborting. Skips silently (Ok) when the worktree already
/// exists, which lets re-claim after release work without surprise.
pub(crate) async fn spawn_claim_worktree(
    airc: &airc_lib::Airc,
    card_id: airc_lib::WorkCardId,
) -> Result<(), Box<dyn std::error::Error>> {
    // Need the card's title for the branch slug — board projection
    // is the source of truth.
    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} not visible in board projection"))?;
    let short: String = card.card_id.to_string().chars().take(8).collect();

    // Card 8a3082c4 + BIGMAMA review fix: skip worktree spawn when
    // card is linked to a LIVE PR (Open/Draft/Ready). A Merged or
    // Closed PR means the work shipped — operator wants a fresh
    // worktree to do follow-up. Look up the merge_state from the
    // projection so the gate is by liveness, not by presence.
    let pr_merge_state = card.pull_request.as_ref().and_then(|pr| {
        board
            .pull_request(&pr.repo, pr.number)
            .and_then(|record| record.merge_state)
    });
    if let Some(reason) = worktree_skip_reason(card, pr_merge_state) {
        println!("worktree:  skipped — {reason}");
        return Ok(());
    }

    let slug = slugify(&card.title, 40);

    let lease_root = lease::lease_root()
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    let worktree_path = lease_root.join(&short);
    if worktree_path.exists() {
        println!("worktree:  {} (existing — reused)", worktree_path.display());
        return Ok(());
    }
    std::fs::create_dir_all(&lease_root)?;

    // Resolve repo root from cwd (the user's checkout). git itself
    // handles the worktree-add — we don't reimplement.
    let repo_root_out = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()?;
    if !repo_root_out.status.success() {
        return Err(format!(
            "git rev-parse --show-toplevel failed: {}",
            String::from_utf8_lossy(&repo_root_out.stderr).trim()
        )
        .into());
    }
    let repo_root = String::from_utf8(repo_root_out.stdout)?.trim().to_string();

    // Card 59243bee: refuse worktree creation when cwd's repo doesn't
    // match card.repo — otherwise we'd create a worktree of the wrong
    // codebase. Honest error beats wrong worktree. The proper
    // cross-repo resolver (~/.airc config mapping RepoId → local
    // clone path) is a richer follow-up; this check at minimum
    // prevents silent damage.
    let cwd_repo_id = cwd_github_repo_id(&repo_root).ok_or_else(|| {
        format!(
            "cannot determine github repo from cwd ({repo_root}); \
                 ensure `git remote get-url origin` points at a github.com URL, \
                 or pass --no-lease-required to skip worktree spawn"
        )
    })?;
    if cwd_repo_id != card.repo.to_string() {
        return Err(format!(
            "card belongs to repo {card_repo}, but cwd is in {cwd_repo}; \
             cd into the {card_repo} checkout and re-claim, or pass \
             --no-lease-required to claim without a worktree spawn",
            card_repo = card.repo,
            cwd_repo = cwd_repo_id,
        )
        .into());
    }

    let branch = format!("{short}/{slug}");
    let add_out = std::process::Command::new("git")
        .args(["-C", &repo_root, "worktree", "add", "-b", &branch])
        .arg(&worktree_path)
        .output()?;
    if !add_out.status.success() {
        return Err(format!(
            "git worktree add failed: {}",
            String::from_utf8_lossy(&add_out.stderr).trim()
        )
        .into());
    }

    println!("worktree:  {}", worktree_path.display());
    println!("branch:    {branch}");
    println!("hint:      cd {}", worktree_path.display());
    Ok(())
}
/// Resolve cwd's github "owner/repo" identity by reading the origin
/// remote URL. Card 59243bee — the check the original d1b2798d
/// shipped without. Returns None when the remote isn't a github URL
/// (or doesn't exist) so callers can decide whether to refuse or
/// degrade. Parses both `https://github.com/owner/repo[.git]` and
/// `git@github.com:owner/repo[.git]` shapes.
pub(crate) fn cwd_github_repo_id(repo_root: &str) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(["-C", repo_root, "remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let url = String::from_utf8(out.stdout).ok()?.trim().to_string();
    parse_github_repo_id(&url)
}
/// Pure-function half of [`cwd_github_repo_id`] — accepts a remote
/// URL and returns `Some("owner/repo")` for github URLs it
/// recognises, `None` otherwise. Kept pure so tests don't need a
/// real git repo.
pub(crate) fn parse_github_repo_id(url: &str) -> Option<String> {
    // SSH: git@github.com:owner/repo[.git]
    // HTTPS: https://github.com/owner/repo[.git]
    // HTTP:  http://github.com/owner/repo[.git]
    let owner_repo = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("https://github.com/"))
        .or_else(|| url.strip_prefix("http://github.com/"))?;
    let owner_repo = owner_repo.trim().trim_end_matches('/');
    let owner_repo = owner_repo.strip_suffix(".git").unwrap_or(owner_repo);
    // Expect exactly one '/' separating owner and repo.
    if owner_repo.matches('/').count() != 1 {
        return None;
    }
    Some(owner_repo.to_string())
}
/// Sanitize a card title into a git-safe branch slug. Lowercase,
/// alphanumeric + '-' only; collapses runs of non-alphanumerics
/// into single dashes; trims leading/trailing dashes; bounds length.
pub(crate) fn slugify(title: &str, max_len: usize) -> String {
    let mut out = String::with_capacity(title.len().min(max_len));
    let mut last_was_dash = false;
    for ch in title.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
        if out.len() >= max_len {
            break;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        out.push_str("work");
    }
    out
}
pub(crate) fn git_rev_parse_branch(worktree: &str) -> Result<String, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("git")
        .args(["-C", worktree, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git rev-parse failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?.trim().to_string())
}
/// `git show -s --format=<format> HEAD` from inside `worktree`. Used
/// to read the HEAD commit's subject (%s) and body (%b) to pass as
/// `gh pr create --title` / `--body`, since `gh pr create --fill`'s
/// heuristic sometimes falls back to a slugified branch name (card
/// 13131f1c). Empty stdout is valid (commits often have an empty
/// body) and propagates as an empty `String`.
pub(crate) fn git_show_format(
    worktree: &str,
    format: &str,
) -> Result<String, Box<dyn std::error::Error>> {
    let out = std::process::Command::new("git")
        .args([
            "-C",
            worktree,
            "show",
            "-s",
            &format!("--format={format}"),
            "HEAD",
        ])
        .output()?;
    if !out.status.success() {
        return Err(format!(
            "git show --format={format} failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        )
        .into());
    }
    Ok(String::from_utf8(out.stdout)?)
}

/// Decide whether [`spawn_claim_worktree`] should skip creating a new
/// worktree for `card`. Returns `Some(reason)` with a human-readable
/// explanation suitable for direct printing, or `None` when the
/// allocator should proceed normally.
///
/// Per Joel `[[every-error-is-an-opportunity-to-battle-harden]]`:
/// fixing the immediate friction (continuum PR #1547 → card 8a3082c4,
/// auto-spawned stray worktree on an already-PR'd card) AND making
/// the rule testable on its own without an IPC daemon round-trip.
///
/// Current rule: skip when `card.pull_request` is linked AND the PR
/// is LIVE (`PrMergeState::{Open,Draft,Ready}`). A linked PR that's
/// already `Merged` or `Closed` is dead work — the operator wants to
/// claim a fresh worktree to start the follow-up, so DON'T skip.
///
/// BIGMAMA review on PR #1199: original rule gated on `is_some()`
/// (PRESENCE), not state, so a card with a stale Merged PR linked
/// would silently refuse the next spawn forever. The fix threads
/// `PullRequestRecord.merge_state` from the projection through to
/// the test, keeping the function pure (caller does the lookup).
///
/// `pr_merge_state == None` means "projection doesn't know" — we
/// fall back to the conservative behavior (skip on presence) to
/// avoid clobbering an unobserved in-flight PR.
pub(crate) fn worktree_skip_reason(
    card: &airc_work::model::WorkCard,
    pr_merge_state: Option<airc_work::model::PrMergeState>,
) -> Option<String> {
    let pr = card.pull_request.as_ref()?;
    use airc_work::model::PrMergeState::*;
    match pr_merge_state {
        Some(Merged) | Some(Closed) => None,
        // Open, Draft, Ready, or unknown → conservative skip.
        _ => Some(format!(
            "card already linked to PR #{number} on branch {head} ({repo}); \
             continue work in your existing checkout/worktree",
            number = pr.number,
            head = pr.head,
            repo = pr.repo,
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::PeerId;
    use airc_work::ids::{RepoId, WorkCardId};
    use airc_work::model::{BranchName, CardState, Priority, PullRequestRef, WorkCard};

    /// Build a minimal `WorkCard` with the optional `pr` already
    /// linked or not. Pure constructor — no clock, no IDs from the
    /// daemon, so the result is reproducible across runs.
    fn card_with_pr(pr: Option<PullRequestRef>) -> WorkCard {
        WorkCard {
            card_id: WorkCardId::new(),
            repo: RepoId::new("acme/widgets").expect("test repo id"),
            title: "fix(scheduler): bound retry backoff".to_string(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            state: CardState::Open,
            owner: None,
            claim_id: None,
            claim_expires_at_ms: None,
            last_heartbeat_at_ms: None,
            pull_request: pr,
            created_by: PeerId::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            reviews: None,
        }
    }

    /// Card 8a3082c4: the load-bearing case for this fix. A PR-linked
    /// card has its work in flight on `pr.head` somewhere else;
    /// allocating a fresh `<short>/<slug>` worktree would be wasted
    /// disk + a misleading hint. The skip reason must be informative
    /// enough that the operator knows where to go instead.
    #[test]
    fn skips_when_pull_request_is_linked() {
        let card = card_with_pr(Some(PullRequestRef {
            repo: RepoId::new("acme/widgets").expect("test repo"),
            number: 1547,
            head: BranchName::new("fix/rolling-log").expect("test head"),
            base: BranchName::new("canary").expect("test base"),
        }));
        let reason = worktree_skip_reason(&card, Some(airc_work::model::PrMergeState::Open))
            .expect("live PR-linked card must skip");
        assert!(
            reason.contains("#1547"),
            "reason must cite PR number: {reason}"
        );
        assert!(
            reason.contains("fix/rolling-log"),
            "reason must cite head branch: {reason}"
        );
        assert!(
            reason.contains("acme/widgets"),
            "reason must cite repo so the operator knows which checkout: {reason}"
        );
    }

    /// BIGMAMA review fix: a Merged or Closed PR is dead work — the
    /// operator wants a fresh worktree to start the follow-up. The
    /// presence-only `is_some()` rule from PR #1199 incorrectly kept
    /// skipping forever; this test pins the liveness gate.
    #[test]
    fn proceeds_when_linked_pr_is_merged() {
        let card = card_with_pr(Some(PullRequestRef {
            repo: RepoId::new("acme/widgets").expect("test repo"),
            number: 1547,
            head: BranchName::new("fix/rolling-log").expect("test head"),
            base: BranchName::new("canary").expect("test base"),
        }));
        assert!(
            worktree_skip_reason(&card, Some(airc_work::model::PrMergeState::Merged)).is_none(),
            "Merged PR is dead work — fresh worktree allowed for follow-up"
        );
    }

    #[test]
    fn proceeds_when_linked_pr_is_closed() {
        let card = card_with_pr(Some(PullRequestRef {
            repo: RepoId::new("acme/widgets").expect("test repo"),
            number: 1547,
            head: BranchName::new("fix/rolling-log").expect("test head"),
            base: BranchName::new("canary").expect("test base"),
        }));
        assert!(
            worktree_skip_reason(&card, Some(airc_work::model::PrMergeState::Closed)).is_none(),
            "Closed PR is dead work — fresh worktree allowed for follow-up"
        );
    }

    /// `None` merge_state means the projection hasn't observed a PR
    /// state yet. Conservative behavior: skip on presence so we don't
    /// clobber an in-flight branch we just haven't indexed yet.
    #[test]
    fn skips_conservatively_when_pr_state_unknown() {
        let card = card_with_pr(Some(PullRequestRef {
            repo: RepoId::new("acme/widgets").expect("test repo"),
            number: 1547,
            head: BranchName::new("fix/rolling-log").expect("test head"),
            base: BranchName::new("canary").expect("test base"),
        }));
        assert!(
            worktree_skip_reason(&card, None).is_some(),
            "unknown PR state must fall back to skip-on-presence"
        );
    }

    /// The normal-claim path. With no PR linked the substrate has no
    /// existing-checkout evidence; the worktree allocator should
    /// proceed (the function returns None so the caller falls
    /// through to its existing logic).
    #[test]
    fn proceeds_when_no_pull_request_linked() {
        let card = card_with_pr(None);
        assert!(
            worktree_skip_reason(&card, None).is_none(),
            "unlinked card must NOT short-circuit worktree spawn",
        );
    }
}
