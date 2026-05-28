//! `airc work ...` handlers.
//!
//! The CLI stays intentionally thin: it parses human input, calls the
//! consumer-facing `airc_lib::Airc` work API, and renders terminal
//! output for humans. Integrations should call `airc-lib` / daemon IPC
//! / ORM projections directly rather than parsing CLI output.
//! Work-domain validation and event construction live in `airc-lib` /
//! `airc-work`.

use std::path::Path;

use airc_diagnostics::{DiagnosticCode, DiagnosticComponent, DiagnosticEvent};
use uuid::Uuid;

use airc_lib::{
    AgentAvailabilityState, CardState, ChangeWorkCardState, ClaimId, ClaimWorkCard,
    CreateWorkCard, LaneId, Priority, ReleaseWorkClaim, RepoId, UpdateWorkCard,
    WorkBacklogSeedCandidate, WorkBacklogSeedOutcome, WorkBoardProjection, WorkCardId,
    WorkManagerRecommendation, WorkManagerRecommendationKind, WorkManagerStatus,
    WorkQueueStatus, WorkRosterStatus,
};

use crate::lease;
use crate::work_cli::{CliAvailabilityState, CliCardState, CliPriority};

pub async fn run_create(
    home: &Path,
    repo: String,
    title: String,
    body: Option<String>,
    lane_id: Option<String>,
    priority: CliPriority,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_id = airc
        .create_work_card(CreateWorkCard {
            repo: RepoId::new(repo)?,
            title,
            body,
            priority: priority.into(),
            lane_id: parse_optional_lane_id(lane_id.as_deref())?,
            reviews: None,
        })
        .await?;
    println!("card_id: {card_id}");
    Ok(())
}

pub async fn run_seed(
    home: &Path,
    repo: String,
    title: String,
    body: Option<String>,
    lane_id: Option<String>,
    priority: CliPriority,
    evidence_key: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let result = airc
        .seed_work_backlog(vec![WorkBacklogSeedCandidate {
            repo: RepoId::new(repo)?,
            title,
            body,
            priority: priority.into(),
            lane_id: parse_optional_lane_id(lane_id.as_deref())?,
            evidence_key,
        }])
        .await?;
    for item in result.items {
        let outcome = match item.outcome {
            WorkBacklogSeedOutcome::Created => "created",
            WorkBacklogSeedOutcome::AlreadyRepresented => "already_represented",
            WorkBacklogSeedOutcome::AlreadyCompleted => "already_completed",
        };
        println!(
            "seeded: outcome={outcome} card_id={card_id} repo={repo} title={title}",
            card_id = item.card_id,
            repo = item.candidate.repo,
            title = item.candidate.title,
        );
    }
    Ok(())
}

/// `airc work review <PARENT> [--pr URL] [--priority P] [--body B]` —
/// spawn a sibling review card with a typed `reviews` link back to
/// the parent. Card ad7e100b Sub-B (the CLI half of the peer-agent
/// review loop); Sub-A shipped the typed substrate.
///
/// Lookup rules:
///   * Parent must exist in the current room's board projection. The
///     review card is created in the SAME repo as the parent (cross-
///     repo reviews aren't a thing — reviews live where the work
///     lives).
///   * Priority defaults to the parent's priority. The review of a
///     P0 is P0-eligible work; the default keeps that visible
///     without the caller having to spell it out.
///   * Title is generated as `review: <parent.title>` (truncated at
///     a reasonable bound to stay board-renderable).
///   * Body prepends the parent card id and any `--pr` URL so a
///     reviewer who pulls just this card has the navigation anchors;
///     `--body` content (if any) follows.
pub async fn run_review(
    home: &Path,
    parent_id: String,
    pr: Option<String>,
    priority: Option<CliPriority>,
    body: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let parent_card_id = parse_work_card_id(&parent_id)?;
    let airc = crate::commands::attached_airc(home).await?;

    // Resolve the parent off the current room's board. Refusing on
    // "no parent" is more useful than spawning an orphan review.
    let board = airc.work_board(usize::MAX).await?;
    let parent = board.card(parent_card_id).ok_or_else(|| {
        format!(
            "parent card {parent_card_id} not found in the current room's board; \
             switch to the room that owns it, or pass the correct id"
        )
    })?;

    // Title — same convention an auto-spawn (Sub-C) would use, so
    // manual and auto-created review cards render identically.
    let title = format_review_title(&parent.title);

    // Body — the navigation anchors a reviewer needs, then the
    // caller's prose. The parent id is a UUID so reviewers can
    // `airc work board` and find the parent fast; `--pr` URL gives
    // them the diff directly.
    let mut body_buf = String::new();
    body_buf.push_str(&format!("review of card {parent_card_id}"));
    if let Some(ref url) = pr {
        body_buf.push_str("\nPR: ");
        body_buf.push_str(url);
    }
    if let Some(extra) = body {
        body_buf.push_str("\n\n");
        body_buf.push_str(&extra);
    }

    // Priority — inherits the parent's unless overridden. Reviews
    // of high-priority work are themselves high-priority.
    let final_priority = priority
        .map(Into::into)
        .unwrap_or(parent.priority);

    // Construct via the airc-lib request type with the typed link
    // populated. Sub-A added `.reviewing(parent)` precisely so this
    // call doesn't have to spell out `reviews: Some(parent)` inline.
    let request = CreateWorkCard::new(parent.repo.clone(), title, final_priority)
        .reviewing(parent_card_id);
    let request = CreateWorkCard {
        body: Some(body_buf),
        ..request
    };

    let review_card_id = airc.create_work_card(request).await?;
    println!("review_card_id: {review_card_id} parent_card_id: {parent_card_id}");
    Ok(())
}

/// Generate the review card's title from the parent's. Format is
/// stable so Sub-C (auto-spawn) and Sub-B (CLI) produce identical
/// titles — observers that filter on `title.starts_with("review:")`
/// pick up both paths.
fn format_review_title(parent_title: &str) -> String {
    // 80-char body keeps board rendering tidy without aggressively
    // truncating informative parent titles.
    const MAX_PARENT_LEN: usize = 80;
    let parent_short: String = parent_title.chars().take(MAX_PARENT_LEN).collect();
    if parent_short.chars().count() < parent_title.chars().count() {
        format!("review: {parent_short}…")
    } else {
        format!("review: {parent_short}")
    }
}

pub async fn run_claim(
    home: &Path,
    card_id: String,
    ttl_ms: u64,
    no_lease_required: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    if !no_lease_required {
        let check = lease::check_current_dir()?;
        if !check.under_lease {
            return Err(format!(
                "refusing to claim work card from {cwd}: not under lease zone {root}.\n\
                 Allocate a worktree under ~/.airc/worktrees/ first, or pass \
                 --no-lease-required to override.",
                cwd = check.path.display(),
                root = check.lease_root.display(),
            )
            .into());
        }
    }
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    let claim_id = airc
        .claim_work_card(ClaimWorkCard {
            card_id: card_uuid,
            ttl_ms,
        })
        .await?;
    println!("claim_id: {claim_id}");

    // Card d1b2798d: auto-spawn a worktree + branch on successful
    // claim. Eliminates the shared-checkout friction two agents on
    // the same machine hit (`--no-lease-required` everywhere is the
    // tell). Best-effort: a git failure does NOT undo the claim or
    // the lease — the claim is the authoritative record, the
    // worktree is convenience around it.
    if let Err(error) = spawn_claim_worktree(&airc, card_uuid).await {
        eprintln!("airc: worktree spawn skipped — {error}");
    }
    Ok(())
}

/// Best-effort: allocate `~/.airc/worktrees/<card_short>/` and a
/// branch `<card_short>/<slug>` off the current feature branch HEAD
/// so the agent who just claimed the card can `cd` into a clean,
/// isolated workspace.
///
/// Returns Err on any genuine failure (no git repo, no lease zone,
/// git command failure) so run_claim can surface it as a warning
/// without aborting. Skips silently (Ok) when the worktree already
/// exists, which lets re-claim after release work without surprise.
async fn spawn_claim_worktree(
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
    let cwd_repo_id = cwd_github_repo_id(&repo_root)
        .ok_or_else(|| {
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
        .args([
            "-C",
            &repo_root,
            "worktree",
            "add",
            "-b",
            &branch,
        ])
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
fn cwd_github_repo_id(repo_root: &str) -> Option<String> {
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
fn parse_github_repo_id(url: &str) -> Option<String> {
    // SSH: git@github.com:owner/repo[.git]
    let owner_repo = if let Some(rest) = url.strip_prefix("git@github.com:") {
        rest
    } else if let Some(rest) = url.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = url.strip_prefix("http://github.com/") {
        rest
    } else {
        return None;
    };
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
fn slugify(title: &str, max_len: usize) -> String {
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

pub async fn run_release(
    home: &Path,
    card_id: String,
    claim_id: Option<String>,
    reason: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    // Default: resolve THIS peer's active claim from the board so
    // callers don't have to track claim_ids the system already knows
    // (kink card acb8bfcd: release ergonomics).
    let claim_uuid = match claim_id {
        Some(raw) => parse_claim_id(&raw)?,
        None => resolve_my_active_claim(&airc, card_uuid).await?,
    };
    airc.release_work_claim(ReleaseWorkClaim {
        card_id: card_uuid,
        claim_id: claim_uuid,
        reason,
    })
    .await?;
    println!("released: card_id={card_id} claim_id={claim_uuid}");
    Ok(())
}

/// Look up the active claim on `card_id` held by this peer via the
/// board projection. Surfaces clear errors when there is no claim, or
/// the claim is held by another peer — so the default never silently
/// releases someone else's work.
async fn resolve_my_active_claim(
    airc: &airc_lib::Airc,
    card_id: airc_lib::WorkCardId,
) -> Result<airc_lib::ClaimId, Box<dyn std::error::Error>> {
    let board = airc.work_board(usize::MAX).await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} not present in the board projection"))?;
    let me = airc.peer_id();
    match (card.owner, card.claim_id) {
        (Some(owner), Some(claim_id)) if owner == me => Ok(claim_id),
        (Some(owner), _) => Err(format!(
            "card {card_id} is currently claimed by {owner}, not this peer ({me}); \
             pass CLAIM_ID explicitly to release a claim you don't own"
        )
        .into()),
        (None, _) => Err(format!("card {card_id} has no active claim to release").into()),
    }
}

pub async fn run_heartbeat(
    home: &Path,
    card_id: String,
    claim_id: String,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    let claim_uuid = parse_claim_id(&claim_id)?;
    airc.heartbeat_work_claim(airc_lib::HeartbeatWorkClaim {
        card_id: card_uuid,
        claim_id: claim_uuid,
        ttl_ms,
    })
    .await?;
    println!("claim_heartbeat: card_id={card_id} claim_id={claim_id} ttl_ms={ttl_ms}");

    // Heartbeat doesn't refuse on lease drift — the claim was
    // already granted, and refusing here would just orphan it. But
    // we DO record the drift as a typed diagnostic so a substrate
    // observer can see when an agent has wandered out of its lease.
    if let Ok(check) = lease::check_current_dir() {
        if !check.under_lease {
            let diag = DiagnosticEvent::warn(
                DiagnosticComponent::Work,
                DiagnosticCode::WorkspaceLeaseViolation,
                "work heartbeat fired from a path outside the lease zone",
            )
            .with_field("card_id", &card_id)
            .with_field("claim_id", &claim_id)
            .with_field("cwd", check.path.display())
            .with_field("lease_root", check.lease_root.display());
            // Best-effort: a publish failure shouldn't break the
            // heartbeat from the user's perspective. Surface to
            // stderr so it isn't silently swallowed.
            if let Err(error) = airc.publish_diagnostic_event(&diag).await {
                eprintln!("airc: failed to publish lease-violation diagnostic: {error}");
            }
        }
    }
    Ok(())
}

/// Card 5ac0a359 — `airc work update <CARD_ID> [--title T] [--body B]
/// [--priority P]`. Amend a card's editable fields post-creation.
/// Omitted flags leave the projection's existing values alone;
/// `--body ""` clears (empty string is the markdown "no body"
/// idiom).
pub async fn run_update(
    home: &Path,
    card_id: String,
    title: Option<String>,
    body: Option<String>,
    priority: Option<CliPriority>,
) -> Result<(), Box<dyn std::error::Error>> {
    let card_uuid = parse_work_card_id(&card_id)?;
    let airc = crate::commands::attached_airc(home).await?;

    let mut request = UpdateWorkCard::amend(card_uuid);
    if let Some(title) = title {
        request = request.with_title(title);
    }
    if let Some(body) = body {
        request = request.with_body(body);
    }
    if let Some(priority) = priority {
        request = request.with_priority(priority.into());
    }

    airc.update_work_card(request).await?;
    println!("card_updated: card_id={card_uuid}");
    Ok(())
}

pub async fn run_state(
    home: &Path,
    card_id: String,
    state: CliCardState,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    let card_state = CardState::from(state);

    // Card a1bc62b3 — substrate-only target states refused from the CLI.
    // Symmetric to 9656a836's close-side guard, which refuses
    // self-attested closure. Without this gate, 9656a836 has a clean
    // bypass:
    //
    //   airc work state X review        # legitimate
    //   airc work state X merged        # ← agent self-attests merged
    //   airc work close X               # passes 9656a836 (state == Merged)
    //
    // Closing that hole is what makes the close-guard's
    // "shipped = PR merged" promise real. When the gh observer ships,
    // it routes through `Airc::change_work_card_state` directly,
    // bypassing this CLI path — that's the correct architectural
    // boundary (agent input through the CLI guard, substrate
    // mechanisms write events directly).
    if !cli_can_set_state_directly(card_state) {
        return Err(refusal_message(card_uuid, card_state).into());
    }

    airc.change_work_card_state(ChangeWorkCardState {
        card_id: card_uuid,
        state: card_state,
    })
    .await?;
    println!("card_state_changed: card_id={card_uuid} state={card_state:?}");

    // Card 820629e9: on transition to Review, open a PR via `gh` from
    // the card's worktree and link it to the card. Best-effort — a gh
    // failure (no commits, no remote, gh not installed) prints a
    // warning but does not undo the state transition. The link is
    // recorded as a separate WorkEvent::PullRequestLinked, whose
    // projection re-sets state=Review idempotently and populates
    // card.pull_request — so downstream consumers (ad7e100b Sub-C
    // auto-spawn review card, board renderers) read one source of
    // truth.
    if card_state == CardState::Review {
        if let Err(error) = open_pr_and_link(&airc, card_uuid).await {
            eprintln!("airc: gh pr create skipped — {error}");
        }
    }
    Ok(())
}

/// Best-effort: open a GitHub PR for the work card's worktree branch
/// and link it via `WorkEvent::PullRequestLinked`. Returns Err so
/// run_state can surface as a warning without aborting the state
/// transition.
async fn open_pr_and_link(
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

    // gh pr create — --fill takes title/body from commits, so a clean
    // workflow (claim → commit → state review) produces a PR titled
    // from the last commit. Head is the worktree's current branch.
    //
    // Card a4fe899f: `gh` does NOT accept `-C` (that's git's flag).
    // The cwd has to be set via `Command::current_dir(...)` so gh's
    // own repo-resolution (which scans `git remote get-url origin`
    // from cwd) picks the worktree's branch. Without this, gh
    // silently ran against whatever the shell's cwd was at invoke
    // time — usually the wrong repo — and the whole Review-state →
    // PR-link pipeline failed (best-effort warning was swallowed).
    let create_out = std::process::Command::new("gh")
        .current_dir(&worktree_str)
        .args(["pr", "create", "--fill"])
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
    let head_branch = git_rev_parse_branch(&worktree_str)?;
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
    if let Err(error) = auto_spawn_review_card(airc, card_id, pr_url).await {
        eprintln!("airc: review card auto-spawn skipped — {error}");
    }

    Ok(())
}

/// Spawn a sibling review card for `parent_id` if one doesn't already
/// exist. Card ad7e100b Sub-C — the auto-spawn side of the
/// peer-agent review loop. Manual spawning still works via
/// `airc work review` (Sub-B); both paths produce structurally
/// identical cards (same title format, same typed `reviews` link)
/// so observers cannot tell them apart and observer filters built on
/// `title.starts_with("review:")` pick up both.
///
/// Idempotency: `WorkBoardProjection::review_cards_for(parent_id)`
/// (added in Sub-A) is the canonical "does a review already exist?"
/// query. Skipping on a non-empty match means re-running
/// `airc work state X review` is a no-op for the review-spawn side,
/// matching the no-op semantics of the PR link.
///
/// Architectural note: auto-spawn lives in the CLI rather than the
/// projection-apply layer on purpose. Projections must stay pure
/// (replay determinism); emitting a new event inside `apply_*` would
/// couple the projection to a side-effectful publish path. The CLI
/// is the right place: it's the orchestration layer that already
/// composes other side-effects (gh pr create, link). A future
/// command-bus subscriber on `PullRequestLinked` could hoist this
/// out of the CLI for consumers that bypass it (Continuum, OpenClaw),
/// but the wire shape is stable — that move doesn't break this
/// emit.
async fn auto_spawn_review_card(
    airc: &airc_lib::Airc,
    parent_id: airc_lib::WorkCardId,
    pr_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let board = airc.work_board(usize::MAX).await?;
    let parent = board
        .card(parent_id)
        .ok_or_else(|| format!("parent card {parent_id} no longer in board projection"))?;

    // Idempotency guard: any pre-existing review for this parent
    // (manual via Sub-B, or a prior auto-spawn) wins. We never spawn
    // a second.
    if board.review_cards_for(parent_id).next().is_some() {
        return Ok(());
    }

    // Title — `format_review_title` is the shared formatter Sub-B
    // (manual CLI) also uses, so observers can't tell auto-spawned
    // from manual cards apart.
    let title = format_review_title(&parent.title);

    let mut body = format!("review of card {parent_id}");
    if !pr_url.is_empty() {
        body.push_str("\nPR: ");
        body.push_str(pr_url);
    }
    body.push_str("\n\nAuto-spawned on Review-state transition (card ad7e100b Sub-C).");

    let request = airc_lib::CreateWorkCard::new(parent.repo.clone(), title, parent.priority)
        .reviewing(parent_id);
    let request = airc_lib::CreateWorkCard {
        body: Some(body),
        ..request
    };
    let review_card_id = airc.create_work_card(request).await?;
    println!("review_card_id: {review_card_id} parent_card_id: {parent_id} (auto-spawned)");
    Ok(())
}

/// Extract the PR number from a gh pr create URL line like
/// `https://github.com/owner/repo/pull/123`. Returns None for any
/// shape we don't recognise (so callers can degrade gracefully).
fn extract_pr_number(url: &str) -> Option<u64> {
    let trimmed = url.trim();
    let tail = trimmed.rsplit('/').next()?;
    tail.parse::<u64>().ok()
}

fn git_rev_parse_branch(worktree: &str) -> Result<String, Box<dyn std::error::Error>> {
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

fn gh_default_branch(worktree: &str) -> Result<String, Box<dyn std::error::Error>> {
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

/// Card a1bc62b3 — gate on direct CLI state writes.
///
/// Returns `true` when the CLI's `airc work state <id> <target>` is
/// allowed to drive a card into `target` by direct event emission.
///
/// `false` for substrate-owned target states:
///   * [`CardState::Merged`] — only the gh observer's
///     `WorkEvent::PullRequestMerged` should set Merged; an agent
///     self-attesting Merged from the CLI defeats 9656a836's
///     close-guard.
///
/// `true` for everything else the workflow currently surfaces
/// (Open / Claimed / InProgress / Blocked / Review / Closed). 9656a836's
/// `close_transition_allowed_from` already gates Closed by FROM-state.
/// This gate is its sibling on the TO-state axis.
///
/// "Workflow is workflow" — for non-coding recipes, swap PR-merged
/// for the recipe-specific verified-done signal; the substrate gate
/// is the same shape.
pub(crate) fn cli_can_set_state_directly(target: CardState) -> bool {
    !matches!(target, CardState::Merged)
}

/// Per-state actionable refusal message for the gate above. Lesser-
/// capable agents read this verbatim, so the corrective steps for
/// each refused state are explicit.
fn refusal_message(card_uuid: airc_lib::WorkCardId, target: CardState) -> String {
    match target {
        CardState::Merged => format!(
            "refusing to set card {card_uuid} to Merged from the CLI: Merged is \
             reserved for the PullRequestMerged event from the gh observer, \
             which fires automatically when the linked PR merges on github.\n\n\
             What you almost certainly want instead:\n  \
             - If the PR has been merged on github: wait for the gh observer \
             event to land (or check the card's pull_request field on the \
             board — if it shows merged_at populated, the observer event is \
             in flight).\n  \
             - If you want to mark the card done without going through \
             github (e.g. cancellation): `airc work close {card_uuid}` from \
             a pre-work state (Open / Claimed / Blocked) succeeds directly \
             per card 9656a836's close-guard.\n  \
             - If you want to override for testing / recovery: this CLI \
             surface is deliberately disabled (card a1bc62b3); patch the \
             substrate or use a substrate-internal recovery tool."
        ),
        // Future substrate-owned states extend this match. The
        // compiler enforces exhaustiveness on the gate function;
        // the refusal message follows it.
        other => format!(
            "refusing to set card {card_uuid} to {other:?} from the CLI \
             (no actionable next step encoded yet — this is a bug in \
             airc-cli's refusal_message)"
        ),
    }
}

pub async fn run_close(home: &Path, card_id: String) -> Result<(), Box<dyn std::error::Error>> {
    run_state(home, card_id, CliCardState::Closed).await
}

/// First 8 chars of a UUID-style id — enough to disambiguate at the
/// board's typical scale, much easier on the eye than 36-char UUIDs.
/// We deliberately do NOT shorten card_id in the board output because
/// callers copy-paste it into `claim` / `state` / `close`; it's the
/// API key, not a display field.
fn short_id<T: std::fmt::Display>(id: T) -> String {
    id.to_string().chars().take(8).collect()
}

/// Render a peer-id for the board: 'me' for self, the published alias
/// when known (kink 6f111211 / card c397567a — looked up via
/// Airc::peer_alias and pre-fetched into the map by run_board), else
/// short-uuid fallback. Honest "unknown" rendering keeps the board
/// readable even when a peer hasn't published an identity card to
/// this room yet.
fn format_peer(
    peer: airc_lib::PeerId,
    me: airc_lib::PeerId,
    aliases: &std::collections::HashMap<airc_lib::PeerId, String>,
) -> String {
    if peer == me {
        "me".to_string()
    } else if let Some(alias) = aliases.get(&peer) {
        alias.clone()
    } else {
        short_id(peer)
    }
}

/// Filter for `airc work board` output — clap enforces mutual exclusion
/// on the underlying flags, but the runtime collapses them into one
/// value so the rest of the code doesn't carry three booleans.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoardFilter {
    All,
    /// No active claim, or claim's lease has expired (eligible for
    /// reclaim per the flywheel-continuity doctrine). Closed/Merged
    /// terminal cards are excluded.
    Available,
    /// Owned by this peer right now.
    Mine,
    /// Owned by another peer (active claim).
    Others,
}

impl BoardFilter {
    pub fn from_flags(available: bool, mine: bool, others: bool) -> Self {
        match (available, mine, others) {
            (true, _, _) => Self::Available,
            (_, true, _) => Self::Mine,
            (_, _, true) => Self::Others,
            _ => Self::All,
        }
    }

    fn matches(self, card: &airc_work::WorkCard, me: airc_lib::PeerId, now_ms: u64) -> bool {
        match self {
            Self::All => true,
            Self::Available => {
                use airc_work::model::CardState;
                if matches!(card.state, CardState::Closed | CardState::Merged) {
                    return false;
                }
                match (card.claim_id, card.claim_expires_at_ms) {
                    (None, _) => true,
                    (Some(_), Some(exp)) if exp <= now_ms => true,
                    _ => false,
                }
            }
            Self::Mine => card.owner == Some(me),
            Self::Others => card.owner.is_some_and(|owner| owner != me),
        }
    }
}

/// Render claim-lease liveness inline on the board (kink ac6affc7):
/// '-' = no claim, '<STALE>' = lease expired (eligible for reclaim per
/// the flywheel-continuity doctrine), otherwise time remaining as
/// 'Mm SS s'. Lets at-a-glance scanning of the board surface stale
/// claims without having to cross-reference 'work roster' + ttl.
fn format_lease(expires_at_ms: Option<u64>, now_ms: u64) -> String {
    match expires_at_ms {
        None => "-".to_string(),
        Some(expires_at_ms) if expires_at_ms <= now_ms => "<STALE>".to_string(),
        Some(expires_at_ms) => {
            let ms_left = expires_at_ms - now_ms;
            let s_left = ms_left / 1000;
            let m = s_left / 60;
            let s = s_left % 60;
            format!("{m}m{s:02}s")
        }
    }
}

pub async fn run_board(
    home: &Path,
    limit: usize,
    filter: BoardFilter,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let board = airc.work_board(limit).await?;
    let me = airc.peer_id();

    // Pre-fetch published aliases for every distinct non-self owner on
    // the board (kink 6f111211 / card c397567a). One scan per peer for
    // now — N is small, and peer_alias is page_recent-backed; if this
    // becomes a hot path the follow-up is one shared scan + a
    // local index.
    let snapshot = board.snapshot();
    let now = now_ms();
    let mut owner_peers: std::collections::HashSet<airc_lib::PeerId> =
        std::collections::HashSet::new();
    for card in &snapshot.cards {
        if let Some(owner) = card.owner {
            if owner != me {
                owner_peers.insert(owner);
            }
        }
    }
    for claim in board.stale_claims(now) {
        if claim.owner != me {
            owner_peers.insert(claim.owner);
        }
    }
    let mut aliases: std::collections::HashMap<airc_lib::PeerId, String> =
        std::collections::HashMap::new();
    for peer in owner_peers {
        if let Ok(Some(alias)) = airc.peer_alias(peer).await {
            aliases.insert(peer, alias);
        }
    }

    print_board(&board, me, filter, &aliases);
    Ok(())
}

pub async fn run_next(
    home: &Path,
    repo: Option<String>,
    max_priority: CliPriority,
    include_stale: bool,
    limit: usize,
    event_limit: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let query = airc_lib::WorkQueueStatusQuery {
        repo: repo.map(RepoId::new).transpose()?,
        max_priority: max_priority.into(),
        include_stale_claims: include_stale,
        event_limit,
        limit,
    };
    let status = airc.work_queue_status(query).await?;
    if status.claimable.is_empty() {
        println!("(no claimable work)");
    } else {
        println!("claimable work: {}", status.claimable.len());
        for item in &status.claimable {
            let stale = item
                .stale_claim
                .as_ref()
                .map(|claim| format!("stale_claim={} owner={}", claim.claim_id, claim.owner))
                .unwrap_or_else(|| "open".to_string());
            println!(
                "{card_id}  {priority:?}  repo={repo}  {stale}  title={title}",
                card_id = item.card.card_id,
                priority = item.card.priority,
                repo = item.card.repo,
                title = item.card.title,
            );
        }
    }

    print_work_queue_availability(&status);
    if !status.active_claims_for_peer.is_empty() {
        println!();
        println!(
            "your active claims: {}",
            status.active_claims_for_peer.len()
        );
        for card in &status.active_claims_for_peer {
            println!(
                "{card_id}  {priority:?}  repo={repo}  title={title}",
                card_id = card.card_id,
                priority = card.priority,
                repo = card.repo,
                title = card.title,
            );
        }
    }
    Ok(())
}

pub async fn run_roster(
    home: &Path,
    repo: Option<String>,
    event_limit: usize,
    active_within_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let status = airc
        .work_roster_status(airc_lib::WorkRosterQuery {
            repo: repo.map(RepoId::new).transpose()?,
            event_limit,
            active_within_ms,
        })
        .await?;
    print_work_roster(&status);
    Ok(())
}

pub async fn run_manage(
    home: &Path,
    repo: Option<String>,
    max_priority: CliPriority,
    include_stale: bool,
    limit: usize,
    event_limit: usize,
    active_within_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let status = airc
        .work_manager_status(airc_lib::WorkManagerQuery {
            repo: repo.map(RepoId::new).transpose()?,
            max_priority: max_priority.into(),
            include_stale_claims: include_stale,
            event_limit,
            limit,
            active_within_ms,
        })
        .await?;
    print_work_manager(&status);
    Ok(())
}

pub async fn run_availability(
    home: &Path,
    repo: String,
    state: CliAvailabilityState,
    note: Option<String>,
    ttl_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let repo = RepoId::new(repo)?;
    airc.report_agent_availability(airc_lib::ReportAgentAvailability {
        repo: repo.clone(),
        state: state.into(),
        note,
        ttl_ms,
    })
    .await?;
    println!("agent_availability: repo={repo} state={state:?} ttl_ms={ttl_ms}");
    Ok(())
}

fn print_board(
    board: &WorkBoardProjection,
    me: airc_lib::PeerId,
    filter: BoardFilter,
    aliases: &std::collections::HashMap<airc_lib::PeerId, String>,
) {
    let snapshot = board.snapshot();
    if snapshot.cards.is_empty() && snapshot.agent_availability.is_empty() {
        println!("(no work cards)");
        return;
    }
    let stale_claims = board.stale_claims(now_ms());

    let now = now_ms();
    let visible: Vec<&airc_work::WorkCard> = snapshot
        .cards
        .iter()
        .filter(|card| filter.matches(card, me, now))
        .collect();
    if !visible.is_empty() {
        if matches!(filter, BoardFilter::All) {
            println!("work cards: {}", visible.len());
        } else {
            println!(
                "work cards: {} (filter={:?}, hidden={})",
                visible.len(),
                filter,
                snapshot.cards.len() - visible.len(),
            );
        }
    } else if !matches!(filter, BoardFilter::All) {
        println!("(no cards match filter {:?})", filter);
    }
    for card in &visible {
        let owner = card
            .owner
            .map(|peer| format_peer(peer, me, aliases))
            .unwrap_or_else(|| "-".to_string());
        let claim = card
            .claim_id
            .map(short_id)
            .unwrap_or_else(|| "-".to_string());
        let lease = format_lease(card.claim_expires_at_ms, now);
        println!(
            "{card_id}  {priority:?}  {state:?}  owner={owner}  claim={claim}  lease={lease}  repo={repo}  title={title}",
            card_id = card.card_id,
            priority = card.priority,
            state = card.state,
            repo = card.repo,
            title = card.title,
        );
    }
    if !stale_claims.is_empty() {
        println!();
        println!("stale claims: {}", stale_claims.len());
        for claim in stale_claims {
            println!(
                "{card_id}  owner={owner}  claim={claim_id}  expired_at_ms={expired_at_ms}",
                card_id = claim.card_id,
                owner = format_peer(claim.owner, me, aliases),
                claim_id = short_id(claim.claim_id),
                expired_at_ms = claim.expired_at_ms,
            );
        }
    }
    if !snapshot.agent_availability.is_empty() {
        println!();
        println!("agent availability: {}", snapshot.agent_availability.len());
        for availability in snapshot.agent_availability {
            let stale = availability.expires_at_ms <= now_ms();
            let note = availability.report.note.as_deref().unwrap_or("-");
            println!(
                "{repo}  peer={peer}  state={state:?}  stale={stale}  expires_at_ms={expires_at_ms}  note={note}",
                repo = availability.report.repo,
                peer = availability.report.peer,
                state = availability.report.state,
                expires_at_ms = availability.expires_at_ms,
            );
        }
    }
}

fn print_work_queue_availability(status: &WorkQueueStatus) {
    if status.agent_availability.is_empty() {
        return;
    }

    let now_ms = now_ms();
    println!();
    println!(
        "agent availability: ready={} busy={} away={} stale={}",
        status.ready_count(now_ms),
        status.busy_count(now_ms),
        status.away_count(now_ms),
        status.stale_availability_count(now_ms)
    );
    for availability in &status.agent_availability {
        let stale = availability.expires_at_ms <= now_ms;
        let note = availability.report.note.as_deref().unwrap_or("-");
        println!(
            "{repo}  peer={peer}  state={state:?}  stale={stale}  note={note}",
            repo = availability.report.repo,
            peer = availability.report.peer,
            state = availability.report.state,
        );
    }
}

fn print_work_roster(status: &WorkRosterStatus) {
    let now_ms = now_ms();
    if status.rows.is_empty() {
        println!("work roster: 0 agent(s)");
        println!("claimable work: {}", status.claimable_count);
        return;
    }

    println!(
        "work roster: {} agent(s) live={} ready={} busy={} away={} stale_availability={} claimable={}",
        status.rows.len(),
        status.alive_count(),
        status.ready_count(now_ms),
        status.busy_count(now_ms),
        status.away_count(now_ms),
        status.stale_availability_count(now_ms),
        status.claimable_count
    );
    for row in &status.rows {
        let live = row
            .liveness
            .as_ref()
            .map(|liveness| {
                let client = liveness.client_id.as_deref().unwrap_or("-");
                let build = liveness.build.as_deref().unwrap_or("-");
                format!(
                    "live runtime={} client={} scope={} build={} last_seen_ms={}",
                    liveness.runtime,
                    client,
                    liveness.scope.as_deref().unwrap_or("-"),
                    build,
                    liveness.last_seen_ms
                )
            })
            .unwrap_or_else(|| "live=false".to_string());
        let availability = row
            .availability
            .as_ref()
            .map(|availability| {
                format!(
                    "availability={:?} repo={} stale={} note={}",
                    availability.report.state,
                    availability.report.repo,
                    availability.expires_at_ms <= now_ms,
                    availability.report.note.as_deref().unwrap_or("-")
                )
            })
            .unwrap_or_else(|| "availability=unknown".to_string());
        println!(
            "peer={peer}  {live}  {availability}  claims={claims}",
            peer = row.peer,
            claims = row.active_claims.len(),
        );
        for card in &row.active_claims {
            println!(
                "  claim {card_id}  {priority:?}  {state:?}  repo={repo}  title={title}",
                card_id = card.card_id,
                priority = card.priority,
                state = card.state,
                repo = card.repo,
                title = card.title,
            );
        }
    }
}

fn print_work_manager(status: &WorkManagerStatus) {
    let now_ms = now_ms();
    println!(
        "work manager: recommendations={} claimable={} agents={} live={} ready={} busy={} away={}",
        status.recommendations.len(),
        status.queue.claimable.len(),
        status.roster.rows.len(),
        status.roster.alive_count(),
        status.roster.ready_count(now_ms),
        status.roster.busy_count(now_ms),
        status.roster.away_count(now_ms),
    );
    if status.recommendations.is_empty() {
        println!("action: none");
        return;
    }
    for recommendation in &status.recommendations {
        print_manager_recommendation(recommendation);
    }
}

fn print_manager_recommendation(recommendation: &WorkManagerRecommendation) {
    let action = match recommendation.kind {
        WorkManagerRecommendationKind::ClaimWork => "claim-work",
        WorkManagerRecommendationKind::RecoverStaleClaim => "recover-stale-claim",
        WorkManagerRecommendationKind::PublishAvailability => "publish-availability",
        WorkManagerRecommendationKind::SeedBacklog => "seed-backlog",
        WorkManagerRecommendationKind::Wait => "wait",
    };
    let card = recommendation
        .card
        .as_ref()
        .map(|card| {
            format!(
                " card={} priority={:?} repo={} title={}",
                card.card_id, card.priority, card.repo, card.title
            )
        })
        .unwrap_or_default();
    let stale = recommendation
        .stale_claim
        .as_ref()
        .map(|claim| {
            format!(
                " stale_claim={} stale_owner={}",
                claim.claim_id, claim.owner
            )
        })
        .unwrap_or_default();
    let agent = recommendation
        .agent
        .as_ref()
        .map(|agent| {
            format!(
                " agent={} client={} runtime={} scope={}",
                agent.peer,
                agent.client_id.as_deref().unwrap_or("-"),
                agent.runtime.as_deref().unwrap_or("-"),
                agent.scope.as_deref().unwrap_or("-"),
            )
        })
        .unwrap_or_default();
    println!(
        "action={action} reason={reason:?}{card}{stale}{agent}",
        reason = recommendation.reason
    );
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

fn parse_work_card_id(input: &str) -> Result<WorkCardId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("work card id {input:?} is not a valid UUID: {error}"))?;
    Ok(WorkCardId::from_uuid(uuid))
}

fn parse_claim_id(input: &str) -> Result<ClaimId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("claim id {input:?} is not a valid UUID: {error}"))?;
    Ok(ClaimId::from_uuid(uuid))
}

fn parse_optional_lane_id(
    input: Option<&str>,
) -> Result<Option<LaneId>, Box<dyn std::error::Error>> {
    input.map(parse_lane_id).transpose()
}

fn parse_lane_id(input: &str) -> Result<LaneId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("lane id {input:?} is not a valid UUID: {error}"))?;
    Ok(LaneId::from_uuid(uuid))
}

impl From<CliPriority> for Priority {
    fn from(value: CliPriority) -> Self {
        match value {
            CliPriority::P0 => Self::P0,
            CliPriority::P1 => Self::P1,
            CliPriority::P2 => Self::P2,
            CliPriority::P3 => Self::P3,
        }
    }
}

impl From<CliAvailabilityState> for AgentAvailabilityState {
    fn from(value: CliAvailabilityState) -> Self {
        match value {
            CliAvailabilityState::Ready => Self::Ready,
            CliAvailabilityState::Busy => Self::Busy,
            CliAvailabilityState::Away => Self::Away,
        }
    }
}

impl From<CliCardState> for CardState {
    fn from(value: CliCardState) -> Self {
        match value {
            CliCardState::Open => Self::Open,
            CliCardState::Claimed => Self::Claimed,
            CliCardState::InProgress => Self::InProgress,
            CliCardState::Blocked => Self::Blocked,
            CliCardState::Review => Self::Review,
            CliCardState::Merged => Self::Merged,
            CliCardState::Closed => Self::Closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_renders_dash_when_no_claim() {
        assert_eq!(format_lease(None, 1_000), "-");
    }

    #[test]
    fn lease_renders_stale_marker_when_expired() {
        assert_eq!(format_lease(Some(500), 1_000), "<STALE>");
        // Exact-equal counts as expired — eligible for reclaim now.
        assert_eq!(format_lease(Some(1_000), 1_000), "<STALE>");
    }

    #[test]
    fn lease_renders_remaining_as_minutes_seconds() {
        // 8 minutes 12 seconds remaining.
        assert_eq!(format_lease(Some(1_000 + 8 * 60_000 + 12_000), 1_000), "8m12s");
        // Sub-minute pads seconds with leading zero.
        assert_eq!(format_lease(Some(1_000 + 5_000), 1_000), "0m05s");
    }

    #[test]
    fn short_id_truncates_to_8_chars() {
        assert_eq!(short_id("cdff6a9d-e995-4b4a-a119-10bc1faf1747"), "cdff6a9d");
        assert_eq!(short_id("short"), "short");
    }

    use airc_core::PeerId;
    use airc_work::model::CardState;
    use airc_work::{ClaimId, Priority, RepoId, WorkCard, WorkCardId};

    fn make_card(
        state: CardState,
        owner: Option<PeerId>,
        claim_id: Option<ClaimId>,
        claim_expires_at_ms: Option<u64>,
    ) -> WorkCard {
        WorkCard {
            card_id: WorkCardId::from_u128(1),
            repo: RepoId::new("test/test").unwrap(),
            title: "t".into(),
            body: None,
            priority: Priority::P2,
            lane_id: None,
            state,
            owner,
            claim_id,
            claim_expires_at_ms,
            last_heartbeat_at_ms: None,
            pull_request: None,
            created_by: PeerId::from_u128(99),
            created_at_ms: 0,
            updated_at_ms: 0,
            reviews: None,
        }
    }

    #[test]
    fn from_flags_picks_filter_or_defaults_to_all() {
        assert_eq!(BoardFilter::from_flags(false, false, false), BoardFilter::All);
        assert_eq!(BoardFilter::from_flags(true, false, false), BoardFilter::Available);
        assert_eq!(BoardFilter::from_flags(false, true, false), BoardFilter::Mine);
        assert_eq!(BoardFilter::from_flags(false, false, true), BoardFilter::Others);
    }

    #[test]
    fn available_filter_includes_open_unclaimed_and_stale_claims() {
        let me = PeerId::from_u128(10);
        let other = PeerId::from_u128(20);
        let claim = ClaimId::from_u128(30);

        // Open, no claim → available
        let card = make_card(CardState::Open, None, None, None);
        assert!(BoardFilter::Available.matches(&card, me, 1_000));

        // Open with stale claim (expired) → available for reclaim
        let card = make_card(CardState::Claimed, Some(other), Some(claim), Some(500));
        assert!(BoardFilter::Available.matches(&card, me, 1_000));

        // Open with active claim → NOT available
        let card = make_card(CardState::InProgress, Some(other), Some(claim), Some(5_000));
        assert!(!BoardFilter::Available.matches(&card, me, 1_000));

        // Closed terminal → never available (even with no claim)
        let card = make_card(CardState::Closed, None, None, None);
        assert!(!BoardFilter::Available.matches(&card, me, 1_000));
    }

    #[test]
    fn mine_and_others_split_ownership_cleanly() {
        let me = PeerId::from_u128(10);
        let other = PeerId::from_u128(20);
        let claim = ClaimId::from_u128(30);

        let mine = make_card(CardState::InProgress, Some(me), Some(claim), Some(5_000));
        let theirs = make_card(CardState::InProgress, Some(other), Some(claim), Some(5_000));
        let unclaimed = make_card(CardState::Open, None, None, None);

        assert!(BoardFilter::Mine.matches(&mine, me, 1_000));
        assert!(!BoardFilter::Mine.matches(&theirs, me, 1_000));
        assert!(!BoardFilter::Mine.matches(&unclaimed, me, 1_000));

        assert!(BoardFilter::Others.matches(&theirs, me, 1_000));
        assert!(!BoardFilter::Others.matches(&mine, me, 1_000));
        assert!(!BoardFilter::Others.matches(&unclaimed, me, 1_000));
    }

    #[test]
    fn extract_pr_number_parses_gh_url_or_returns_none() {
        assert_eq!(
            extract_pr_number("https://github.com/CambrianTech/airc/pull/123"),
            Some(123),
        );
        assert_eq!(
            extract_pr_number("  https://github.com/owner/repo/pull/1  "),
            Some(1),
        );
        // Non-numeric tail → None (degrade gracefully).
        assert_eq!(extract_pr_number("https://example.com/no-pr-here"), None);
        assert_eq!(extract_pr_number(""), None);
    }

    #[test]
    fn parse_github_repo_id_handles_https_ssh_and_trailing_git() {
        // HTTPS — both with and without .git suffix.
        assert_eq!(
            parse_github_repo_id("https://github.com/CambrianTech/airc.git"),
            Some("CambrianTech/airc".into()),
        );
        assert_eq!(
            parse_github_repo_id("https://github.com/CambrianTech/airc"),
            Some("CambrianTech/airc".into()),
        );
        // SSH (git@github.com:owner/repo).
        assert_eq!(
            parse_github_repo_id("git@github.com:CambrianTech/continuum.git"),
            Some("CambrianTech/continuum".into()),
        );
        // Trailing slash gets normalized.
        assert_eq!(
            parse_github_repo_id("https://github.com/CambrianTech/airc/"),
            Some("CambrianTech/airc".into()),
        );
        // Non-github remotes return None so the caller can refuse
        // worktree creation with a clear error.
        assert_eq!(
            parse_github_repo_id("https://gitlab.com/owner/repo.git"),
            None,
        );
        assert_eq!(parse_github_repo_id(""), None);
        // Github URL but with extra path segments isn't owner/repo.
        assert_eq!(
            parse_github_repo_id("https://github.com/owner/repo/pull/123"),
            None,
        );
    }

    #[test]
    fn slugify_lowercases_alphanumeric_collapses_separators_and_bounds_length() {
        assert_eq!(
            slugify("airc work claim: auto-spawn worktree + branch", 40),
            "airc-work-claim-auto-spawn-worktree-bran",
        );
        assert_eq!(slugify("simple", 40), "simple");
        // Trailing dashes from cut runs of non-alphanum are trimmed.
        assert_eq!(slugify("a !! b", 40), "a-b");
        // Empty / fully non-alphanum -> sensible fallback so branch
        // creation never produces "<short>/" with empty suffix.
        assert_eq!(slugify("!!!", 40), "work");
        assert_eq!(slugify("", 40), "work");
        // Length bound respected.
        let bounded = slugify(&"x".repeat(200), 10);
        assert!(bounded.len() <= 10, "got: {bounded}");
    }

    #[test]
    fn format_peer_renders_alias_when_known_short_uuid_otherwise() {
        let me = PeerId::from_u128(10);
        let alice = PeerId::from_u128(20);
        let bob = PeerId::from_u128(30);
        let mut aliases = std::collections::HashMap::new();
        aliases.insert(alice, "alice".to_string());

        // Self is always "me", even if a stale alias exists for self.
        assert_eq!(format_peer(me, me, &aliases), "me");
        // Known peer renders by alias.
        assert_eq!(format_peer(alice, me, &aliases), "alice");
        // Unknown peer falls back to short-uuid — never empty string.
        let bob_short = format_peer(bob, me, &aliases);
        assert_eq!(bob_short.len(), 8);
        assert_ne!(bob_short, "me");
    }

    #[test]
    fn all_filter_admits_everything() {
        let me = PeerId::from_u128(10);
        let claim = ClaimId::from_u128(30);
        let cases = [
            make_card(CardState::Open, None, None, None),
            make_card(CardState::Closed, Some(me), Some(claim), Some(5_000)),
            make_card(CardState::Merged, None, None, None),
        ];
        for card in &cases {
            assert!(BoardFilter::All.matches(card, me, 1_000));
        }
    }

    // -----------------------------------------------------------------
    // Card ad7e100b Sub-B — review-card title formatting.
    //
    // Sub-C (auto-spawn on Review state) and Sub-B (this CLI) MUST
    // produce identical titles so observers filtering on
    // `title.starts_with("review:")` pick up both paths. Pin the
    // shared formatter here so a future tweak to either path can't
    // diverge them silently.
    // -----------------------------------------------------------------

    #[test]
    fn review_title_preserves_short_parent_title_verbatim() {
        let title = format_review_title("typed reviews link on WorkCard");
        assert_eq!(title, "review: typed reviews link on WorkCard");
        assert!(
            title.starts_with("review: "),
            "starts_with(\"review:\") observer filter must hold"
        );
    }

    #[test]
    fn review_title_truncates_long_parent_with_ellipsis_marker() {
        // Eighty Xs is exactly at the limit and must NOT truncate;
        // adding the 81st character must add the ellipsis.
        let parent_at_limit: String = std::iter::repeat('x').take(80).collect();
        let title_at_limit = format_review_title(&parent_at_limit);
        assert_eq!(title_at_limit, format!("review: {parent_at_limit}"));
        assert!(!title_at_limit.ends_with('…'));

        let parent_too_long: String = std::iter::repeat('x').take(120).collect();
        let title_too_long = format_review_title(&parent_too_long);
        assert!(title_too_long.ends_with('…'));
        // The visible portion + the "review: " prefix should sum to
        // the 80-char limit + the ellipsis suffix, so reviewers see
        // a recognizable parent title without the board renderer
        // wrapping.
        let expected_prefix: String = std::iter::repeat('x').take(80).collect();
        assert_eq!(title_too_long, format!("review: {expected_prefix}…"));
    }

    #[test]
    fn review_title_counts_chars_not_bytes_for_unicode_parents() {
        // Multi-byte chars are 1 char but >1 byte. The truncation
        // bound MUST be character-based; otherwise we'd slice in the
        // middle of a UTF-8 sequence and panic. Use a 3-byte glyph
        // ('日') 90 times — well past the 80-char limit but under
        // any byte-based slice.
        let parent: String = std::iter::repeat('日').take(90).collect();
        let title = format_review_title(&parent);
        // No panic = the truncation respects char boundaries. The
        // truncated portion must be exactly 80 chars of '日' + the
        // ellipsis.
        let expected_visible: String = std::iter::repeat('日').take(80).collect();
        assert_eq!(title, format!("review: {expected_visible}…"));
    }

    // ---------------------------------------------------------------------
    // Card a1bc62b3 — substrate-only target-state gate
    // ---------------------------------------------------------------------

    /// The gate refuses ONLY [`CardState::Merged`] today. Every other
    /// `CardState` variant is allowed through, because:
    ///   - Open / Claimed / Blocked: pre-work states the agent
    ///     legitimately drives.
    ///   - InProgress: agent's "I'm actively working" signal.
    ///   - Review: opens a PR (card 820629e9).
    ///   - Closed: 9656a836's close-guard handles the FROM-state
    ///     gate; this gate is the TO-state sibling.
    ///
    /// Future substrate-owned target states extend the match in
    /// `cli_can_set_state_directly`. This test catches any new
    /// CardState variant that's added without an explicit
    /// allow/refuse decision (the test fails closed: every variant
    /// is asserted on, so a new one needs an arm added here).
    #[test]
    fn cli_can_set_state_directly_only_refuses_merged() {
        // Refused targets:
        assert!(
            !cli_can_set_state_directly(CardState::Merged),
            "Merged is substrate-owned (gh PullRequestMerged event); \
             CLI must not self-attest"
        );

        // Allowed targets — every other current CardState variant.
        // Listed explicitly (not via a default arm) so a new variant
        // added to CardState surfaces here at test-time, forcing the
        // author to classify it.
        assert!(cli_can_set_state_directly(CardState::Open));
        assert!(cli_can_set_state_directly(CardState::Claimed));
        assert!(cli_can_set_state_directly(CardState::InProgress));
        assert!(cli_can_set_state_directly(CardState::Blocked));
        assert!(cli_can_set_state_directly(CardState::Review));
        assert!(cli_can_set_state_directly(CardState::Closed));
    }

    /// Refusal message is what a lesser-capable agent reads
    /// verbatim to figure out the corrective action. Pin the
    /// substantive guidance phrases so a future tweak that drops
    /// them surfaces here.
    #[test]
    fn refusal_message_for_merged_carries_actionable_guidance() {
        let card_id = airc_lib::WorkCardId::new();
        let msg = refusal_message(card_id, CardState::Merged);
        assert!(msg.contains("PullRequestMerged"), "names the substrate event");
        assert!(msg.contains("gh observer"), "points at the event source");
        assert!(msg.contains("airc work close"), "names the cancellation alternative");
        assert!(msg.contains("9656a836"), "cross-references the close-guard card");
        assert!(msg.contains("a1bc62b3"), "cross-references THIS card");
        // The id must appear so the agent can copy-paste it into
        // the corrective command.
        assert!(msg.contains(&card_id.to_string()), "carries the card UUID");
    }
}
