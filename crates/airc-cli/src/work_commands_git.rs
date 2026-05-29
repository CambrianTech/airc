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
    let board = airc.work_board(usize::MAX).await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} not visible in board projection"))?;
    let short: String = card.card_id.to_string().chars().take(8).collect();
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
