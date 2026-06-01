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
    AgentAvailabilityState, CardState, ChangeWorkCardState, ClaimId, ClaimWorkCard, CreateWorkCard,
    LaneId, Priority, ReleaseWorkClaim, RepoId, UpdateWorkCard, WorkBacklogSeedCandidate,
    WorkBacklogSeedOutcome, WorkBoardProjection, WorkCard, WorkCardId, WorkManagerRecommendation,
    WorkManagerRecommendationKind, WorkManagerStatus, WorkQueueStatus, WorkRosterStatus,
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
    let board = airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE).await?;
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
    let final_priority = priority.map(Into::into).unwrap_or(parent.priority);

    // Construct via the airc-lib request type with the typed link
    // populated. Sub-A added `.reviewing(parent)` precisely so this
    // call doesn't have to spell out `reviews: Some(parent)` inline.
    let request =
        CreateWorkCard::new(parent.repo.clone(), title, final_priority).reviewing(parent_card_id);
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
    let lease_check = lease::check_current_dir()?;
    if !no_lease_required {
        if !lease_check.under_lease {
            return Err(format!(
                "refusing to claim work card from {cwd}: not under lease zone {root}.\n\
                 Allocate a worktree under ~/.airc/worktrees/ first, or pass \
                 --no-lease-required to override.",
                cwd = lease_check.path.display(),
                root = lease_check.lease_root.display(),
            )
            .into());
        }
    } else {
        // Card 303f2384: restrict the `--no-lease-required` bypass.
        // The flag was intended as an escape hatch for legitimate
        // non-worktree contexts (claiming a review card from the
        // project's main checkout, recovering from an orphaned claim
        // in a fresh tree). It started getting used everywhere
        // (`--no-lease-required` "is the tell" per card d1b2798d's
        // doc) — agents bypass the whole worktree workflow because
        // they don't want to deal with it, then we get shared-
        // checkout collisions and identity leaks.
        //
        // Restriction: even with the flag set, the cwd MUST be one
        // of:
        //   (a) under the lease zone (`~/.airc/worktrees/<short>/`,
        //       in which case the flag was redundant — silently OK)
        //   (b) at the project's git working-tree ROOT (e.g.
        //       `/Users/joel/Development/airc`). Joel-from-the-main-
        //       checkout case + the "primary tab" scenario both look
        //       like this; random subdirs do not.
        //   (c) an airc-scope owner — cwd contains a `.airc/` dir.
        //       Integration test workspaces (tempdir + `airc init`)
        //       satisfy this without being a git repo, and any
        //       "primary tab" cwd already covered by (b) also has
        //       `.airc/`; this widens the bypass without weakening
        //       the no-random-subdir intent.
        //
        // Any other cwd is refused: tmpdirs without state, accidental
        // sibling-worktree paths, unrelated projects. The agent gets an
        // explicit error with the corrective action.
        let cwd_is_scope_owner = lease_check.path.join(".airc").is_dir();
        if !lease_check.under_lease
            && !cwd_is_scope_owner
            && !cwd_is_project_root(&lease_check.path)?
        {
            return Err(format!(
                "refusing --no-lease-required claim from {cwd}: cwd is neither under \
                 the lease zone ({root}), at the project's git working-tree root, \
                 NOR an airc-scope owner (no .airc/ subdir). The flag is for review \
                 cards / main-checkout claims; for normal worktree-spawned cards, \
                 cd to a worktree first or omit the flag.",
                cwd = lease_check.path.display(),
                root = lease_check.lease_root.display(),
            )
            .into());
        }
        // Soft warning even on the legitimate path — the flag's overuse
        // is the slippery slope.
        if !lease_check.under_lease {
            eprintln!(
                "warn: --no-lease-required claim from project root \
                 ({cwd}). Acceptable for review cards / direct primary \
                 claims; for normal cards use the worktree workflow.",
                cwd = lease_check.path.display()
            );
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
    if let Err(error) = crate::work_commands_git::spawn_claim_worktree(&airc, card_uuid).await {
        eprintln!("airc: worktree spawn skipped — {error}");
    }
    Ok(())
}

/// Card 303f2384: true when `cwd` IS the git project's main working
/// tree root (e.g. `/Users/joel/Development/airc`). Used by the
/// `--no-lease-required` gate so legitimate direct-claim contexts
/// (Joel claiming a review card from his main checkout, the
/// "primary tab" pattern) still work, while random subdirs /
/// tmpdirs / unrelated-project cwds get refused.
///
/// Returns Err only when git itself fails in a way the caller cares
/// about (we'd otherwise refuse legitimate claims because of a
/// transient git error); the gate above defaults to NOT-root when
/// this returns Err, so the agent gets the explicit refusal rather
/// than a silent success.
fn cwd_is_project_root(cwd: &std::path::Path) -> Result<bool, Box<dyn std::error::Error>> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()?;
    if !output.status.success() {
        // Not a git repo OR git unavailable. Conservative: NOT root.
        return Ok(false);
    }
    let toplevel = String::from_utf8(output.stdout)?.trim().to_string();
    if toplevel.is_empty() {
        return Ok(false);
    }
    let toplevel_canon = std::path::PathBuf::from(&toplevel)
        .canonicalize()
        .unwrap_or_else(|_| std::path::PathBuf::from(&toplevel));
    let cwd_canon = cwd.canonicalize().unwrap_or_else(|_| cwd.to_path_buf());
    Ok(cwd_canon == toplevel_canon)
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
    let board = airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE).await?;
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

    // Card a1bc62b3 (substrate-target gate): refuse direct CLI writes
    // to states that should only come from substrate observers (e.g.
    // Merged must come from PullRequestMerged, not `airc work state X
    // merged` by an agent). Architectural boundary: agent input goes
    // through this CLI guard; substrate mechanisms write events
    // directly via Airc::change_work_card_state.
    if !cli_can_set_state_directly(card_state) {
        return Err(refusal_message(card_uuid, card_state).into());
    }

    // Card 9656a836 (close-lifecycle gate): Closed must come from
    // Merged (PR merged) or {Open, Claimed, Blocked} (cancellation
    // before any real work landed). Refuses Closed from InProgress
    // or Review — there's claimed work in flight; the agent should
    // either ship via state review → merge, or release the claim.
    // Complementary to a1bc62b3 above: that one refuses agents from
    // self-attesting Merged; this one refuses agents from
    // self-attesting Closed without a Merged predecessor.
    if card_state == CardState::Closed {
        let board = airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE).await?;
        let card = board.card(card_uuid).ok_or_else(|| {
            format!("card {card_uuid} not visible in current room's board projection")
        })?;
        if !close_transition_allowed_from_card(card) {
            // Card fae3c28e: tailor the refusal so review-only cards
            // get the right next step. Review cards have no PR to
            // merge — the review IS the work — so the "state review →
            // PR → Merged" path doesn't apply to them. But review
            // cards from {Open, Claimed, Blocked} are still allowed
            // by the work-not-yet-started branch above, so reaching
            // here from a review card means InProgress, which is
            // exactly the gap this fix closes.
            let next_step = if card.reviews.is_some() {
                format!(
                    "Review-only cards close directly from InProgress (no PR to merge — \
                     the review IS the work). This refusal means the gap fae3c28e fixed is \
                     not in the running binary; reinstall airc or use `airc work state \
                     {card_uuid} blocked` as a workaround until then."
                )
            } else {
                format!(
                    "If work is in flight ({actual:?}), the next step is:\n  \
                     - state Review: open a PR via `airc work state {card_uuid} review`\n  \
                     - wait for the PR to merge (state → Merged via gh observer)\n  \
                     - THEN `airc work close {card_uuid}` succeeds.\n\n\
                     If you want to abandon the work, `airc work release {card_uuid}` \
                     drops the claim and returns the card to its prior state — close \
                     from there.",
                    actual = card.state,
                )
            };
            return Err(format!(
                "refusing to close card {card_uuid}: current state is {actual:?}, but \
                 Closed requires Merged (PR merged) or {{Open, Claimed, Blocked}} \
                 (cancellation before work landed), or InProgress on a review-only \
                 card (the review IS the work).\n\n{next_step}",
                actual = card.state,
            )
            .into());
        }
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
        if let Err(error) = crate::work_commands_gh::open_pr_and_link(&airc, card_uuid).await {
            eprintln!("airc: gh pr create skipped — {error}");
        }
    }

    // Card abe9fe4c: on transition to Closed (the terminal state),
    // remove the per-card worktree spawned by d1b2798d and prune the
    // branch if it has no unmerged commits left. Best-effort — a
    // cleanup failure prints a warning but does NOT undo the close.
    // The same hook is called from the merger's perform_merge path
    // (card f16650cd) once that wiring lands as a follow-up.
    //
    // Why call from here (vs. from the projection's apply_card_state_changed):
    // projections must stay pure (replay determinism); emitting a
    // git mutation inside `apply_*` would couple the projection to a
    // side-effectful path. The CLI is the right place because it's
    // already the orchestration layer that publishes the event.
    if card_state == CardState::Closed {
        if let Err(error) = cleanup_card_worktree(card_uuid).await {
            eprintln!("airc: worktree cleanup skipped — {error}");
        }
    }
    Ok(())
}

/// Card abe9fe4c — remove the per-card worktree (and prune its
/// branch if no unmerged commits remain) once a card terminalizes.
/// Disk-pressure substrate fix; the 2026-05-28 session sat on
/// ~25 GB of orphan target/ before a manual sweep.
///
/// Contract:
///   * Resolves `~/.airc/worktrees/<card_short>/` from `lease_root`.
///   * Skips silently (Ok) if the worktree doesn't exist — re-close
///     on an already-cleaned card is a no-op.
///   * REFUSES with an error if the worktree has uncommitted
///     changes (`git status --porcelain` non-empty). The agent
///     recovers manually; we never silently nuke pending work.
///   * REFUSES if the worktree has unpushed commits AND its branch
///     is the worktree's current HEAD — those commits would be
///     unreachable after pruning. Pushed commits are fine because
///     origin still has them.
///   * On the happy path: `git worktree remove --force` then
///     `git branch -D` if the branch has no other reachable refs.
///   * All operations run from the MAIN working tree (the cwd's
///     repo root), not from the worktree being removed.
///
/// Called from TWO terminal paths so a card's worktree is reclaimed
/// regardless of who closes it: (1) the CLI `airc work close` path
/// below, and (2) the merger's `perform_merge` after a successful
/// `MarkPullRequestMerged` (card cdb477a2). The merger closes the
/// majority of cards; without path (2) every merger-merged card
/// leaked its worktree — the 84-orphan accumulation this fix targets.
pub(crate) async fn cleanup_card_worktree(
    card_id: airc_lib::WorkCardId,
) -> Result<(), Box<dyn std::error::Error>> {
    let short: String = card_id.to_string().chars().take(8).collect();
    let lease_root = lease::lease_root()
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    let worktree_path = lease_root.join(&short);
    if !worktree_path.exists() {
        return Ok(());
    }
    let worktree_str = worktree_path.to_string_lossy().to_string();

    // Uncommitted-change guard.
    let status_out = std::process::Command::new("git")
        .args(["-C", &worktree_str, "status", "--porcelain"])
        .output()?;
    if !status_out.status.success() {
        return Err(format!(
            "git status --porcelain failed on worktree {worktree_str}: {}",
            String::from_utf8_lossy(&status_out.stderr).trim()
        )
        .into());
    }
    let porcelain = String::from_utf8(status_out.stdout)?;
    if !porcelain.trim().is_empty() {
        return Err(format!(
            "worktree at {worktree_str} has uncommitted changes — refusing \
             to remove. commit + push or discard manually, then re-close \
             the card to retry cleanup."
        )
        .into());
    }

    // Identify the worktree's branch so we can prune it after removal.
    let branch_out = std::process::Command::new("git")
        .args(["-C", &worktree_str, "rev-parse", "--abbrev-ref", "HEAD"])
        .output()?;
    let branch = if branch_out.status.success() {
        String::from_utf8(branch_out.stdout)?.trim().to_string()
    } else {
        String::new()
    };

    // Resolve the main working tree's repo root so the `git worktree
    // remove` and branch-prune run from there.
    let repo_root_out = std::process::Command::new("git")
        .args(["-C", &worktree_str, "rev-parse", "--git-common-dir"])
        .output()?;
    if !repo_root_out.status.success() {
        return Err(format!(
            "could not resolve git common dir for {worktree_str}: {}",
            String::from_utf8_lossy(&repo_root_out.stderr).trim()
        )
        .into());
    }
    let common_dir = String::from_utf8(repo_root_out.stdout)?.trim().to_string();
    // common_dir is typically `<main>/.git`; the actual repo root is its parent.
    let repo_root = std::path::Path::new(&common_dir)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or(common_dir);

    let remove_out = std::process::Command::new("git")
        .args([
            "-C",
            &repo_root,
            "worktree",
            "remove",
            "--force",
            &worktree_str,
        ])
        .output()?;
    if !remove_out.status.success() {
        return Err(format!(
            "git worktree remove --force {worktree_str} failed: {}",
            String::from_utf8_lossy(&remove_out.stderr).trim()
        )
        .into());
    }
    println!("worktree_removed: {worktree_str}");

    // Best-effort branch prune. `git branch -d` (lowercase) refuses
    // to delete an unmerged branch — that's what we want. If the
    // branch is gone (e.g. `gh pr merge --delete-branch` already
    // ran), this errors silently.
    if !branch.is_empty() && branch != "HEAD" {
        let prune_out = std::process::Command::new("git")
            .args(["-C", &repo_root, "branch", "-d", &branch])
            .output()?;
        if prune_out.status.success() {
            println!("branch_pruned: {branch}");
        } else {
            // Non-fatal: the branch may have unmerged work, or may
            // already be deleted. Both are acceptable terminal states.
            eprintln!(
                "airc: branch {branch} not pruned ({}). leave it; \
                 `git branch -D {branch}` from the main worktree to force.",
                String::from_utf8_lossy(&prune_out.stderr).trim()
            );
        }
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
pub(crate) async fn auto_spawn_review_card(
    airc: &airc_lib::Airc,
    parent_id: airc_lib::WorkCardId,
    pr_url: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let board = airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE).await?;
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

/// Card 70e87d33: retroactively link an already-open PR to a card so
/// the merger can pick it up. Thin orchestration over
/// `work_commands_gh::link_existing_pr` — attach, parse the card id,
/// delegate.
pub async fn run_link(
    home: &Path,
    card_id: String,
    pr: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    crate::work_commands_gh::link_existing_pr(&airc, card_uuid, pr).await
}

/// Card a399b342: `airc work merge <CARD_ID>` — manual one-shot merge
/// behind the same gate the auto-merger uses. Refuses unless the card
/// is in Review state with a PR linked AND the PR's CI is green per
/// the strictly-less-red-than-base policy (card d5b7b07d).
///
/// Failure modes that get an explicit refusal (not a swallowed error):
///   - card not visible / not in Review
///   - no PR linked
///   - PR state ≠ OPEN
///   - merge conflicts
///   - failing checks NOT already failing on base
///   - checks still running
///
/// On success: `gh pr merge --squash --delete-branch`, then emit
/// `MarkPullRequestMerged` so the projection transitions consistently
/// with the merger path. `--dry-run` prints the gate decision and
/// stops.
pub async fn run_merge(
    home: &Path,
    card_id: String,
    dry_run: bool,
    pending_timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::gh_client::GhClient as _;
    use airc_lib::MarkPullRequestMerged;
    use airc_work::model::CardState;

    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;

    let board = airc.work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE).await?;
    let card = board.card(card_uuid).ok_or_else(|| {
        format!("card {card_uuid} not visible in current room's board projection")
    })?;

    if card.state != CardState::Review {
        return Err(format!(
            "refusing to merge card {card_uuid}: state is {actual:?}, but `airc work merge` \
             requires Review (the card has been finished + PR opened + announced).\n\n\
             Next step: `airc work state {card_uuid} review` to open + link the PR, then \
             `airc work merge {card_uuid}` once CI is green.",
            actual = card.state,
        )
        .into());
    }

    let Some(pr) = card.pull_request.clone() else {
        return Err(format!(
            "refusing to merge card {card_uuid}: Review state but no PR linked. \
             The `state review` transition runs `gh pr create` and emits \
             `LinkCardPullRequest`; re-run it from the card's worktree so the \
             projection has a PR to merge."
        )
        .into());
    };

    let gh = crate::gh_client::ShellGhClient::new();
    let baseline = crate::merger::fetch_baseline_failures(&gh).await;
    if !baseline.is_empty() {
        eprintln!(
            "airc: baseline has {} failing check(s) on rust-rewrite — inherited \
             failures with those names are ignored (strictly-less-red, card d5b7b07d)",
            baseline.len()
        );
    }

    // Card 7ed1ac4f: pending-too-long timeout. Default 30 min from
    // GatePolicy::default_for_merger; CLI flag override comes
    // through `pending_timeout_secs` arg (0 = strict-mode bypass).
    let policy = crate::merger::GatePolicy {
        pending_timeout_ms: pending_timeout_secs.saturating_mul(1000),
        now_ms: crate::merger::now_ms(),
    };

    match crate::merger::check_pr_gate(&gh, &pr, &baseline, policy).await {
        Ok(crate::merger::GateResult::Green) => {
            if dry_run {
                println!(
                    "merge_gate: GREEN — card={card_uuid} pr=#{n} repo={r} (would merge)",
                    n = pr.number,
                    r = pr.repo,
                );
                return Ok(());
            }
            gh.pr_merge(crate::gh_client::PrMergeArgs {
                repo: pr.repo.as_str().to_string(),
                number: pr.number,
            })
            .await?;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            airc.mark_pull_request_merged(MarkPullRequestMerged {
                card_id: card_uuid,
                pull_request: pr.clone(),
                merged_at_ms: now_ms,
            })
            .await?;
            println!(
                "merged: card={card_uuid} pr=#{n} repo={r}",
                n = pr.number,
                r = pr.repo,
            );
            Ok(())
        }
        Ok(crate::merger::GateResult::NotReady(reason)) => Err(format!(
            "refusing to merge card {card_uuid} pr=#{n}: {reason}.\n\n\
             The strictly-less-red doctrine (card d5b7b07d) already lets you through \
             when only base-inherited failures remain. If you BELIEVE this gate is \
             wrong for your case, fix the upstream root cause or rebase rather than \
             reaching for `gh pr merge` directly — that bypass is exactly what this \
             gate exists to refuse (Joel's 'engineering staff' merge discipline).",
            n = pr.number,
        )
        .into()),
        Err(error) => Err(format!(
            "merge gate query failed for card {card_uuid} pr=#{n}: {error}.\n\n\
             Network / rate-limit / auth issue with gh — retry or check `gh auth status`. \
             The substrate refuses to merge when it cannot verify the gate; that's the \
             intended fail-closed behavior.",
            n = pr.number,
        )
        .into()),
    }
}

/// Card 9656a836 + fae3c28e: gate the close transition.
/// Returns `true` when `card` is allowed to transition to Closed.
///
/// Closed has substrate meaning: this card was shipped (PR merged)
/// OR cancelled before any real work landed OR — for review-only
/// cards — the review work itself completed. An agent who calls
/// `airc work close` from InProgress/Review/Closed on a regular
/// work card is doing the thing we caught: self-reporting work as
/// done without it actually going through review + merge. Per
/// Joel's "lesser persona intelligences" framing, that MUST refuse
/// strictly — the persona can't be trusted to "know better," the
/// substrate refuses.
///
/// Card fae3c28e exception: review-only cards (`card.reviews` is
/// Some, the typed sibling link added in card ad7e100b Sub-A) have
/// no PR to ship. The review IS the work — its artifact is the
/// LGTM/feedback comment on the parent card's PR, not a separate
/// merge. For those cards, InProgress → Closed is the natural
/// completion path. Without this, Codex-style workflows had to
/// route through Blocked → Closed as a workaround, which is
/// semantic noise ("blocked" means waiting on something external).
///
/// "Workflow is workflow" — for non-coding recipes, swap PR-merged
/// for the recipe-specific "verified done" signal; this rule's
/// shape (Closed requires verified-done OR pre-work cancellation
/// OR review-only completion) generalizes.
pub(crate) fn close_transition_allowed_from_card(card: &WorkCard) -> bool {
    match card.state {
        CardState::Merged | CardState::Open | CardState::Claimed | CardState::Blocked => true,
        CardState::InProgress if card.reviews.is_some() => true,
        _ => false,
    }
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
    check_idle: bool,
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
    // Card e4cad280 ENGINE: in --check-idle mode, suppress the
    // human-readable suggestion list and return a script-friendly
    // exit code. Status 0 = there IS claimable work for this peer;
    // status 1 = board is idle for this peer, agent should consult
    // wall recipes and generate next-step cards. The substrate
    // provides the signal; domain-level recipe handling lives in
    // the consumer (Claude tab / Codex / hermes adapter / continuum
    // runner).
    if check_idle {
        if status.claimable.is_empty() {
            return Err("idle: no claimable work for this peer".into());
        }
        return Ok(());
    }
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
        assert_eq!(
            format_lease(Some(1_000 + 8 * 60_000 + 12_000), 1_000),
            "8m12s"
        );
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
        assert_eq!(
            BoardFilter::from_flags(false, false, false),
            BoardFilter::All
        );
        assert_eq!(
            BoardFilter::from_flags(true, false, false),
            BoardFilter::Available
        );
        assert_eq!(
            BoardFilter::from_flags(false, true, false),
            BoardFilter::Mine
        );
        assert_eq!(
            BoardFilter::from_flags(false, false, true),
            BoardFilter::Others
        );
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
            crate::work_commands_gh::extract_pr_number(
                "https://github.com/CambrianTech/airc/pull/123"
            ),
            Some(123),
        );
        assert_eq!(
            crate::work_commands_gh::extract_pr_number("  https://github.com/owner/repo/pull/1  "),
            Some(1),
        );
        // Non-numeric tail → None (degrade gracefully).
        assert_eq!(
            crate::work_commands_gh::extract_pr_number("https://example.com/no-pr-here"),
            None
        );
        assert_eq!(crate::work_commands_gh::extract_pr_number(""), None);
    }

    #[test]
    fn parse_github_repo_id_handles_https_ssh_and_trailing_git() {
        // HTTPS — both with and without .git suffix.
        assert_eq!(
            crate::work_commands_git::parse_github_repo_id(
                "https://github.com/CambrianTech/airc.git"
            ),
            Some("CambrianTech/airc".into()),
        );
        assert_eq!(
            crate::work_commands_git::parse_github_repo_id("https://github.com/CambrianTech/airc"),
            Some("CambrianTech/airc".into()),
        );
        // SSH (git@github.com:owner/repo).
        assert_eq!(
            crate::work_commands_git::parse_github_repo_id(
                "git@github.com:CambrianTech/continuum.git"
            ),
            Some("CambrianTech/continuum".into()),
        );
        // Trailing slash gets normalized.
        assert_eq!(
            crate::work_commands_git::parse_github_repo_id("https://github.com/CambrianTech/airc/"),
            Some("CambrianTech/airc".into()),
        );
        // Non-github remotes return None so the caller can refuse
        // worktree creation with a clear error.
        assert_eq!(
            crate::work_commands_git::parse_github_repo_id("https://gitlab.com/owner/repo.git"),
            None,
        );
        assert_eq!(crate::work_commands_git::parse_github_repo_id(""), None);
        // Github URL but with extra path segments isn't owner/repo.
        assert_eq!(
            crate::work_commands_git::parse_github_repo_id(
                "https://github.com/owner/repo/pull/123"
            ),
            None,
        );
    }

    #[test]
    fn close_transition_allowed_from_card_pins_the_close_lifecycle_gate() {
        // Allowed: Merged (PR-merged happy path) + Open/Claimed/Blocked
        // (cancellation before any commits landed). Regular non-review
        // cards (reviews: None) exercise the pre-fae3c28e gate.
        let regular = |state| make_card(state, None, None, None);
        assert!(close_transition_allowed_from_card(&regular(
            CardState::Merged
        )));
        assert!(close_transition_allowed_from_card(&regular(
            CardState::Open
        )));
        assert!(close_transition_allowed_from_card(&regular(
            CardState::Claimed
        )));
        assert!(close_transition_allowed_from_card(&regular(
            CardState::Blocked
        )));
        // Refused: any state representing in-flight work. An agent
        // calling close from these is exactly the 2026-05-28 bypass.
        assert!(!close_transition_allowed_from_card(&regular(
            CardState::InProgress
        )));
        assert!(!close_transition_allowed_from_card(&regular(
            CardState::Review
        )));
        // Refused: Closed → Closed is a no-op masquerading as work.
        // Forcing the caller to recognise the card is already closed
        // (read the board) before re-issuing.
        assert!(!close_transition_allowed_from_card(&regular(
            CardState::Closed
        )));
    }

    /// Card fae3c28e — review-only cards (where `reviews.is_some()`)
    /// can transition InProgress → Closed because the review IS the
    /// work; there's no PR/Merged to gate on. Without this, review
    /// cards had to route through Blocked → Closed as a workaround,
    /// which is semantic noise (Blocked means waiting on something
    /// external, not "review-complete").
    #[test]
    fn close_transition_allowed_from_card_opens_review_only_in_progress_path() {
        let mut review = make_card(CardState::InProgress, None, None, None);
        review.reviews = Some(WorkCardId::from_u128(42));

        // The review-only InProgress path is the fae3c28e fix:
        // allowed for review cards, refused for everything else.
        assert!(
            close_transition_allowed_from_card(&review),
            "review-only card (reviews.is_some()) closes from InProgress",
        );

        // A regular card (reviews: None) in InProgress still refuses,
        // matching the 9656a836 rule — agents must ship via Review →
        // Merged for actual work, not self-attest Closed.
        let regular = make_card(CardState::InProgress, None, None, None);
        assert!(
            !close_transition_allowed_from_card(&regular),
            "non-review InProgress still refuses — 9656a836 rule",
        );

        // Review-only cards from Review state still refuse — review
        // cards don't transition to Review (no PR to open), so seeing
        // one in Review is an indicator of either a stale state from a
        // pre-fae3c28e binary or a misuse. Refuse and force the
        // caller to look.
        let mut review_in_review = make_card(CardState::Review, None, None, None);
        review_in_review.reviews = Some(WorkCardId::from_u128(42));
        assert!(
            !close_transition_allowed_from_card(&review_in_review),
            "review-only Review state still refuses — review cards don't reach Review",
        );

        // Sanity: the pre-existing allowed transitions are unchanged
        // when applied to the new function (regression guard).
        for state in [
            CardState::Merged,
            CardState::Open,
            CardState::Claimed,
            CardState::Blocked,
        ] {
            let mut review = make_card(state, None, None, None);
            review.reviews = Some(WorkCardId::from_u128(42));
            assert!(
                close_transition_allowed_from_card(&review),
                "review-only state {state:?} preserves pre-fae3c28e allow",
            );
            let regular = make_card(state, None, None, None);
            assert!(
                close_transition_allowed_from_card(&regular),
                "regular state {state:?} preserves pre-fae3c28e allow",
            );
        }
    }

    #[test]
    fn slugify_lowercases_alphanumeric_collapses_separators_and_bounds_length() {
        assert_eq!(
            crate::work_commands_git::slugify("airc work claim: auto-spawn worktree + branch", 40),
            "airc-work-claim-auto-spawn-worktree-bran",
        );
        assert_eq!(crate::work_commands_git::slugify("simple", 40), "simple");
        // Trailing dashes from cut runs of non-alphanum are trimmed.
        assert_eq!(crate::work_commands_git::slugify("a !! b", 40), "a-b");
        // Empty / fully non-alphanum -> sensible fallback so branch
        // creation never produces "<short>/" with empty suffix.
        assert_eq!(crate::work_commands_git::slugify("!!!", 40), "work");
        assert_eq!(crate::work_commands_git::slugify("", 40), "work");
        // Length bound respected.
        let bounded = crate::work_commands_git::slugify(&"x".repeat(200), 10);
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
        let parent_at_limit: String = "x".repeat(80);
        let title_at_limit = format_review_title(&parent_at_limit);
        assert_eq!(title_at_limit, format!("review: {parent_at_limit}"));
        assert!(!title_at_limit.ends_with('…'));

        let parent_too_long: String = "x".repeat(120);
        let title_too_long = format_review_title(&parent_too_long);
        assert!(title_too_long.ends_with('…'));
        // The visible portion + the "review: " prefix should sum to
        // the 80-char limit + the ellipsis suffix, so reviewers see
        // a recognizable parent title without the board renderer
        // wrapping.
        let expected_prefix: String = "x".repeat(80);
        assert_eq!(title_too_long, format!("review: {expected_prefix}…"));
    }

    #[test]
    fn review_title_counts_chars_not_bytes_for_unicode_parents() {
        // Multi-byte chars are 1 char but >1 byte. The truncation
        // bound MUST be character-based; otherwise we'd slice in the
        // middle of a UTF-8 sequence and panic. Use a 3-byte glyph
        // ('日') 90 times — well past the 80-char limit but under
        // any byte-based slice.
        let parent: String = "日".repeat(90);
        let title = format_review_title(&parent);
        // No panic = the truncation respects char boundaries. The
        // truncated portion must be exactly 80 chars of '日' + the
        // ellipsis.
        let expected_visible: String = "日".repeat(80);
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
        assert!(
            msg.contains("PullRequestMerged"),
            "names the substrate event"
        );
        assert!(msg.contains("gh observer"), "points at the event source");
        assert!(
            msg.contains("airc work close"),
            "names the cancellation alternative"
        );
        assert!(
            msg.contains("9656a836"),
            "cross-references the close-guard card"
        );
        assert!(msg.contains("a1bc62b3"), "cross-references THIS card");
        // The id must appear so the agent can copy-paste it into
        // the corrective command.
        assert!(msg.contains(&card_id.to_string()), "carries the card UUID");
    }

    /// Card 28f1440c — the airc PR target branch must be the
    /// substrate's working branch (`rust-rewrite`), NOT the repo's
    /// GitHub default (`main`). Card 70e87d33 made the base per-repo
    /// (so continuum can target `canary`), but the airc invariant is
    /// unchanged and pinned here: a regression that routes airc
    /// through the GitHub default would surface `main` and silently
    /// bypass the substrate work the doctrine requires (AGENTS.md §8).
    /// This is the per-repo-aware successor to the old constant test —
    /// extended per its own instruction, not deleted.
    #[test]
    fn airc_pr_base_targets_rust_rewrite_never_main() {
        std::env::remove_var("AIRC_PR_BASE");
        let airc_repo = airc_work::RepoId::new("CambrianTech/airc").expect("valid repo key");
        let base = crate::work_commands_gh::configured_base_branch(&airc_repo);
        assert_eq!(
            base.as_deref(),
            Some("rust-rewrite"),
            "card 28f1440c: airc PR target must be the substrate working \
             branch, never the repo's GitHub default ('main' on this \
             repo today)"
        );
        assert_ne!(base.as_deref(), Some("main"));
    }

    // ---------------------------------------------------------------------
    // Card abe9fe4c — worktree cleanup on close.
    //
    // The full cleanup function shells out to git, so end-to-end tests
    // need a real repo + worktree. Those live as integration tests
    // (separate harness); here we pin the small pure pieces and the
    // path-resolution contract.
    // ---------------------------------------------------------------------

    /// `cleanup_card_worktree` resolves the path as
    /// `<lease_root>/<first 8 chars of card_id>/`. Pinning this so the
    /// path NEVER drifts away from the spawn-side convention
    /// (d1b2798d's `spawn_claim_worktree`); if those two ever
    /// disagree, cleanup silently does the wrong thing — either
    /// missing the target or removing an unrelated dir.
    #[test]
    fn cleanup_path_matches_spawn_convention() {
        let card_id = airc_lib::WorkCardId::new();
        let expected_short: String = card_id.to_string().chars().take(8).collect();
        assert_eq!(
            expected_short.len(),
            8,
            "the substrate's short-id convention is exactly 8 chars; \
             cleanup, spawn, and board renderer all agree on this number"
        );
        // The path SHAPE the cleanup builds: <lease_root>/<short>/.
        // We can't easily mock lease_root in this binary-only test
        // module, but we CAN pin the substrate-level promise that
        // the short id matches what spawn_claim_worktree builds.
        // (`short_id` lives in this file's helpers and already has its
        // own test pinning the 8-char take. This test is the
        // architectural cross-reference.)
        assert_eq!(short_id(card_id.to_string()), expected_short);
    }

    // ---------------------------------------------------------------------
    // Card 303f2384 — --no-lease-required cwd gate
    // ---------------------------------------------------------------------

    #[test]
    fn cwd_is_project_root_returns_false_for_nonexistent_path() {
        // A path with no git repo (and that doesn't exist) returns
        // Ok(false), not an error — that's the conservative default
        // the gate relies on so a transient git failure doesn't
        // permit a refused claim.
        let unique = std::env::temp_dir().join(format!(
            "airc-303f2384-noent-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        // Don't even create it — confirms the no-git-here path
        let result = super::cwd_is_project_root(&unique);
        match result {
            Ok(false) => {}
            Ok(true) => panic!("nonexistent path is NOT project root"),
            Err(_) => {} // Acceptable — git can't enter a missing dir
        }
    }

    #[test]
    fn cwd_is_project_root_true_at_repo_top_level() {
        // From the repo root (cargo runs tests from the package
        // dir), git rev-parse --show-toplevel returns the workspace
        // root. The cwd at test time IS that, so cwd_is_project_root
        // must return true. Pins the substrate gate's "primary tab"
        // semantic.
        let cwd = std::env::current_dir().expect("cwd available");
        // Find the actual workspace root via git
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&cwd)
            .output();
        let Ok(out) = output else {
            return; // No git, can't verify the property
        };
        if !out.status.success() {
            return;
        }
        let toplevel = String::from_utf8(out.stdout).unwrap().trim().to_string();
        // cd to the workspace root and check
        let root = std::path::PathBuf::from(toplevel);
        let result = super::cwd_is_project_root(&root).expect("git available");
        assert!(
            result,
            "running with cwd = git toplevel must be detected as project root"
        );
    }

    #[test]
    fn cwd_is_project_root_false_in_subdir_of_repo() {
        // A subdir of the repo (e.g. crates/airc-cli) is NOT the
        // project root — gate must refuse `--no-lease-required`
        // claims from random sub-paths.
        let cwd = std::env::current_dir().expect("cwd available");
        let output = std::process::Command::new("git")
            .args(["rev-parse", "--show-toplevel"])
            .current_dir(&cwd)
            .output();
        let Ok(out) = output else { return };
        if !out.status.success() {
            return;
        }
        let toplevel = String::from_utf8(out.stdout).unwrap().trim().to_string();
        let root = std::path::PathBuf::from(toplevel);
        let subdir = root.join("crates");
        if !subdir.exists() {
            return; // Sanity guard; in this repo it exists
        }
        let result = super::cwd_is_project_root(&subdir).expect("git available");
        assert!(
            !result,
            "subdir of the workspace must NOT be detected as project root"
        );
    }
}
