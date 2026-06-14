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
    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
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
    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
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
        let board = airc
            .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
            .await?;
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

/// Card edf3670c — emit `MarkPullRequestMerged` AND reclaim the
/// card's worktree as one atomic merge-completion step.
///
/// Every "PR merged" terminal path MUST route through this helper.
/// Before extraction, four call sites (merger daemon × 2 +
/// `run_merge` × 2) each hand-paired the two operations; the CLI
/// merge sites forgot the cleanup wire, leaving 12-19 GB per
/// orphaned worktree on Joel's Intel Mac — the recurring disk-full
/// crash this card targets.
///
/// Contract:
/// 1. Emits `MarkPullRequestMerged` so the projection transitions
///    the card to `Merged` consistently with the daemon path.
/// 2. Best-effort reclaim of the worktree at
///    `~/.airc/worktrees/<short>/`. A cleanup refusal
///    (uncommitted / unpushed work) logs a warning but does NOT
///    fail the merge — the PR is already merged + the projection
///    has transitioned. Same shape as `merger::perform_merge`.
/// 3. Emits a `tracing::info!` breadcrumb at
///    `airc::work::merge::reclaim` so sentinels / operators can
///    subscribe to merge-completion without parsing stderr.
///
/// The source-text regression test `every_merge_site_routes_through_helper`
/// pins the call-site count so a future maintainer who reintroduces
/// the bug (hand-paired `mark_pull_request_merged` + missing
/// cleanup) breaks at compile-time, not at "disk full mid-session."
pub(crate) async fn mark_merged_and_reclaim(
    airc: &airc_lib::Airc,
    card_id: airc_lib::WorkCardId,
    pull_request: airc_work::model::PullRequestRef,
    merged_at_ms: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use airc_lib::MarkPullRequestMerged;
    airc.mark_pull_request_merged(MarkPullRequestMerged {
        card_id,
        pull_request,
        merged_at_ms,
    })
    .await?;
    tracing::info!(
        target: "airc::work::merge::reclaim",
        %card_id,
        "worktree reclaim attempted after MarkPullRequestMerged",
    );
    if let Err(error) = cleanup_card_worktree(card_id).await {
        eprintln!("airc: worktree cleanup skipped for {card_id} — {error}");
    }
    Ok(())
}

/// Card abe9fe4c — remove the per-card worktree (and prune its
/// branch if no unmerged commits remain) once a card terminalizes.
/// Disk-pressure substrate fix; the 2026-05-28 session sat on
/// ~25 GB of orphan target/ before a manual sweep.
///
/// Contract (PR #1105 reviewer round 1 fix — describes ACTUAL
/// behavior, not the previous version's mis-stated guarantees):
///   * Resolves `~/.airc/worktrees/<card_short>/` from `lease_root`.
///   * Skips silently (Ok) if the worktree doesn't exist — re-close
///     on an already-cleaned card is a no-op.
///   * REFUSES via `probe_dirty_status` if any of: (a) `git status
///     --porcelain` non-empty (uncommitted / untracked work — WIP
///     on disk), (b) `git rev-list --count @{u}..HEAD > 0`
///     (committed but unpushed work — WIP only-on-this-machine),
///     or (c) the probe is ambiguous (no upstream, detached HEAD,
///     broken git — refuse to classify per [[no-fallbacks-ever]]).
///     The agent recovers manually; we never silently nuke pending
///     work. This is the "operator's WIP outranks hygiene" contract.
///   * On the happy path: `git worktree remove --force` then
///     `git branch -d` (lowercase — refuses unmerged) as a
///     second line of defense if probe somehow missed unpushed
///     work, or if the branch was used by a different worktree
///     too.
///   * All operations run from the MAIN working tree (the cwd's
///     repo root), not from the worktree being removed.
///
/// Called from every terminal path that ends a card's life — grep
/// for callers (do NOT maintain a list here; card edf3670c's bug
/// shipped because the prior caller-enumeration was stale and the
/// `run_merge` site was forgotten). Today's wires:
///
/// - `airc work close` for review-only + cancellation
/// - `merger::perform_merge` (CI-green daemon path)
/// - `merger::perform_reconcile` (already-merged daemon path)
/// - `mark_merged_and_reclaim` (CLI `airc work merge` — both
///   GREEN + AlreadyMerged branches go through the helper)
///
/// **Future terminal paths MUST route through
/// [`mark_merged_and_reclaim`] (when the path emits
/// `MarkPullRequestMerged`) or call this directly (close-shape
/// path).** A regression test pins the call-site count; if you
/// add a 5th terminal path, the test fails until you wire cleanup.
pub(crate) async fn cleanup_card_worktree(
    card_id: airc_lib::WorkCardId,
) -> Result<(), Box<dyn std::error::Error>> {
    let short: String = card_id.to_string().chars().take(8).collect();
    let lease_root = lease::lease_root()
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    let parent = lease_root.join(&short);
    if !parent.exists() {
        return Ok(());
    }
    // Card 83a5624e: support both the canonical `<short>/` layout
    // and the `<short>/src/` nested layout (used by repos like
    // continuum where `src/` is the workspace root). Without this,
    // the merger's cleanup hook stranded nested worktrees on every
    // merge — observed 2026-06-05 with continuum PRs #1530/#1531.
    let worktree_path = resolve_worktree_path(&parent);
    let worktree_str = worktree_path.to_string_lossy().to_string();

    // WIP guard: refuse if either uncommitted/untracked OR
    // committed-but-unpushed work exists, OR the probe is ambiguous.
    // Same `probe_dirty_status` the run_cleanup classifier uses so
    // both terminal paths apply identical safety.
    match probe_dirty_status(&worktree_path) {
        DirtyStatus::Clean => {}
        DirtyStatus::Dirty => {
            return Err(format!(
                "worktree at {worktree_str} has uncommitted or unpushed work — \
                 refusing to remove. commit + push or discard manually, then \
                 re-close the card to retry cleanup."
            )
            .into());
        }
        DirtyStatus::Unknown => {
            return Err(format!(
                "worktree at {worktree_str} could not be classified \
                 (not a git dir / no upstream tracking / detached HEAD / \
                 broken repo) — refusing to remove. Per [[no-fallbacks-ever]] \
                 the substrate refuses to nuke worktrees it can't \
                 confidently call clean. Inspect with `git -C {worktree_str} \
                 status` and resolve manually."
            )
            .into());
        }
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
    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
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

/// Card c9b28925: prune worktrees whose card has reached a terminal
/// state. Doctrinally the always-on side of two principles from the
/// substrate handbook: `local-worktree-is-temp-dir` (worktrees are L1
/// cache, not durable state) and `substrate-is-a-good-citizen-on-the-host`
/// (no disk filling). This slice is the manual command so operators
/// can audit before automating.
///
/// Default behaviour is dry-run. `--force` actually runs `git worktree
/// remove`. Dirty / locked / orphan worktrees are NEVER touched: the
/// operator's WIP outranks hygiene.
pub async fn run_cleanup(
    home: &Path,
    dry_run: bool,
    force: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;

    let lease_root = lease::lease_root()
        .ok_or_else(|| "HOME/USERPROFILE not set; cannot resolve ~/.airc/worktrees/".to_string())?;
    if !lease_root.exists() {
        println!(
            "no worktrees to inspect: {} does not exist",
            lease_root.display()
        );
        return Ok(());
    }

    // Collect classifications. Pure-function classifier lives below so
    // every disposition gets a unit test.
    let mut classifications: Vec<WorktreeClassification> = Vec::new();
    for entry in std::fs::read_dir(&lease_root)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let basename = entry.file_name().to_string_lossy().to_string();
        let Some(short) = parse_worktree_short_id(&basename) else {
            continue;
        };
        let card = board
            .snapshot()
            .cards
            .iter()
            .find(|c| {
                let card_short: String = c.card_id.to_string().chars().take(8).collect();
                card_short == short
            })
            .cloned();
        let card_state = card.as_ref().map(|c| c.state);
        let pr_number = card
            .as_ref()
            .and_then(|c| c.pull_request.as_ref().map(|pr| pr.number));
        let repo = card.as_ref().map(|c| c.repo.to_string());

        let (effective, dirty_status, disposition) =
            classify_worktree_path(&path, card_state.as_ref());

        classifications.push(WorktreeClassification {
            short,
            path: effective,
            card_state,
            pr_number,
            repo,
            dirty_status,
            disposition,
        });
    }

    // Sort: Removable first (most actionable), then KeepActive, then
    // SkipX (least urgent, but still surface). Inside each bucket sort
    // by short id for determinism.
    classifications.sort_by(|a, b| {
        a.disposition
            .display_priority()
            .cmp(&b.disposition.display_priority())
            .then_with(|| a.short.cmp(&b.short))
    });

    // Always print the table — that's the diagnostic the operator
    // wants. `--force` then takes destructive action; `--dry-run` is
    // the explicit-intent alias of the default.
    println!("worktrees scanned: {}", classifications.len());
    if classifications.is_empty() {
        return Ok(());
    }

    let mut last_bucket = "";
    for c in &classifications {
        let bucket = c.disposition.bucket_label();
        if bucket != last_bucket {
            println!("  {bucket}:");
            last_bucket = bucket;
        }
        let pr_label = c
            .pr_number
            .map(|n| format!("PR #{n}"))
            .unwrap_or_else(|| "no PR".to_string());
        let state_label = c
            .card_state
            .map(|s| format!("{s:?}"))
            .unwrap_or_else(|| "unknown card".to_string());
        let suffix = c.disposition.suffix();
        println!(
            "    {short}  {pr_label}  {state_label}{suffix}",
            short = c.short
        );
    }
    println!();

    let removable: Vec<&WorktreeClassification> = classifications
        .iter()
        .filter(|c| matches!(c.disposition, Disposition::Removable))
        .collect();

    if removable.is_empty() {
        println!("Nothing to remove.");
        return Ok(());
    }

    if !force {
        let suffix = if dry_run { " (--dry-run)" } else { "" };
        println!(
            "Would remove {n} worktree(s) on --force{suffix}.",
            n = removable.len()
        );
        return Ok(());
    }

    // --force: actually remove. Per-worktree error doesn't kill the
    // batch — surface the diagnostic, keep going. The operator
    // re-runs after fixing the specific issue.
    let mut removed = 0usize;
    for c in removable {
        match git_worktree_remove(&c.path) {
            Ok(()) => {
                println!("✓ removed {}", c.path.display());
                removed += 1;
            }
            Err(error) => {
                eprintln!("✗ {} — {error}", c.path.display());
            }
        }
    }
    println!("{removed} worktree(s) removed.");
    Ok(())
}

/// One scan result for `run_cleanup`'s table + the force loop.
struct WorktreeClassification {
    short: String,
    path: std::path::PathBuf,
    card_state: Option<airc_work::CardState>,
    pr_number: Option<u64>,
    #[allow(dead_code)]
    repo: Option<String>,
    #[allow(dead_code)]
    dirty_status: DirtyStatus,
    disposition: Disposition,
}

/// What the operator should do with this worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Disposition {
    /// Card is Closed or Merged; worktree is clean. Safe to remove.
    Removable,
    /// Card is still active (Open / Claimed / InProgress / Review /
    /// Blocked / Merged-but-projection-not-caught-up). Keep.
    KeepActive,
    /// Worktree has uncommitted or unpushed work (detected by
    /// `probe_dirty_status`). Operator's WIP outranks hygiene;
    /// print, never remove.
    SkipDirty,
    /// Couldn't run `git status` (not a git dir, permissions, etc).
    SkipNotGit,
    /// No card matches the worktree's short id. Orphan — surface but
    /// don't touch; could be from a deleted card or a different scope.
    SkipUnknownCard,
}

impl Disposition {
    /// Sort key: lower → earlier in the printed table.
    fn display_priority(self) -> u8 {
        match self {
            Disposition::Removable => 0,
            Disposition::KeepActive => 1,
            Disposition::SkipDirty => 2,
            Disposition::SkipNotGit => 3,
            Disposition::SkipUnknownCard => 4,
        }
    }

    fn bucket_label(self) -> &'static str {
        match self {
            Disposition::Removable => "Removable",
            Disposition::KeepActive => "KeepActive",
            Disposition::SkipDirty => "SkipDirty",
            Disposition::SkipNotGit => "SkipNotGit",
            Disposition::SkipUnknownCard => "SkipUnknownCard",
        }
    }

    fn suffix(self) -> &'static str {
        match self {
            Disposition::Removable => "  (would remove on --force)",
            Disposition::KeepActive => "",
            Disposition::SkipDirty => "  uncommitted or unpushed work",
            Disposition::SkipNotGit => "  not a git worktree",
            Disposition::SkipUnknownCard => "  no matching card on board",
        }
    }
}

/// Whether the worktree carries work the operator has not yet
/// captured upstream. Outcome of `probe_dirty_status` — see that
/// function for the two-pass check (porcelain + `@{u}..HEAD`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirtyStatus {
    /// Working tree matches HEAD AND HEAD is fully pushed upstream.
    Clean,
    /// Either uncommitted/untracked files OR committed-but-unpushed
    /// commits exist. Either way the worktree's files are WIP that
    /// may not be captured anywhere else; refuse to remove.
    Dirty,
    /// Probe failed — `git status` errored, no upstream tracking,
    /// detached HEAD, or not a git dir. Per `[[no-fallbacks-ever]]`
    /// the classifier treats Unknown as SkipNotGit and refuses
    /// rather than guessing Clean.
    Unknown,
}

/// Pure classifier. Card c9b28925 — table-driven so every disposition
/// gets exercised in tests without spinning up a real worktree.
///
/// `upstream_gone`: the worktree's tracking branch is missing on
/// origin. Universal signal for "PR was merged + branch deleted"
/// (the `gh pr merge --delete-branch` flow that the auto-merger
/// AND every manual merge uses). When clean + upstream_gone we
/// override an out-of-sync kanban state and remove — fixes the
/// recurring disk-full crash where worktrees for already-merged
/// PRs lingered because the projection hadn't observed the merge.
/// BIGMAMA review on PR #1198: extract the per-worktree classification
/// pipeline so the regression nets the PRODUCTION code path. Reverting
/// `probe_upstream_gone(&effective)` → `&path` inside THIS function now
/// makes the nested-layout test go red — which is what a regression net
/// must actually guarantee.
///
/// Card 83a5624e: resolve `<short>/` vs `<short>/src/` BEFORE probing so
/// the operator sees the actual git worktree path in the disposition
/// table AND the force-remove loop targets the right directory.
pub(crate) fn classify_worktree_path(
    path: &std::path::Path,
    card_state: Option<&CardState>,
) -> (std::path::PathBuf, DirtyStatus, Disposition) {
    let effective = resolve_worktree_path(path);
    let dirty_status = probe_dirty_status_at(&effective);
    let upstream_gone = probe_upstream_gone(&effective);
    let disposition = classify_worktree(card_state, &dirty_status, upstream_gone);
    (effective, dirty_status, disposition)
}

pub(crate) fn classify_worktree(
    card_state: Option<&airc_work::CardState>,
    dirty: &DirtyStatus,
    upstream_gone: bool,
) -> Disposition {
    use airc_work::CardState;
    if matches!(dirty, DirtyStatus::Unknown) {
        return Disposition::SkipNotGit;
    }
    if matches!(dirty, DirtyStatus::Dirty) {
        return Disposition::SkipDirty;
    }
    // `upstream_gone` is the trump card for cleanups: branch deleted
    // on origin = PR merged (or branch abandoned). Either way the
    // worktree's HEAD is dead weight on disk. Doesn't depend on the
    // card projection being up-to-date — that's the entire point.
    if upstream_gone {
        return Disposition::Removable;
    }
    let Some(state) = card_state else {
        return Disposition::SkipUnknownCard;
    };
    match state {
        CardState::Closed | CardState::Merged => Disposition::Removable,
        CardState::Open
        | CardState::Claimed
        | CardState::InProgress
        | CardState::Review
        | CardState::Blocked => Disposition::KeepActive,
    }
}

/// Returns `true` when the worktree's current branch tracks an
/// upstream that no longer exists on `origin`. The dominant cause is
/// `gh pr merge --delete-branch` (and the auto-merger's equivalent)
/// removing the branch on the remote after the merge commit lands.
///
/// Implementation: `git -C path rev-parse --abbrev-ref @{u}` gives
/// the tracking ref name (e.g. `origin/feat/foo`); split off the
/// remote name, then `git -C path ls-remote --exit-code --heads
/// <remote> <branch>` — exit 2 means the ref is absent on the
/// remote, exit 0 means it's present. Probe failures (no upstream
/// tracking, detached HEAD, network errors) return `false` — the
/// safe default keeps the worktree under the existing classifier
/// rules so a transient remote-list error never deletes work.
fn probe_upstream_gone(path: &std::path::Path) -> bool {
    let path_str = path.to_string_lossy().to_string();

    // Step 1: resolve the upstream tracking ref. No upstream = no
    // signal; return false and let the card-state classifier
    // handle it.
    let upstream_out = match std::process::Command::new("git")
        .args(["-C", &path_str, "rev-parse", "--abbrev-ref", "@{u}"])
        .output()
    {
        Ok(out) => out,
        Err(_) => return false,
    };
    if !upstream_out.status.success() {
        return false;
    }
    let upstream_full = String::from_utf8_lossy(&upstream_out.stdout)
        .trim()
        .to_string();
    let Some((remote, branch)) = upstream_full.split_once('/') else {
        return false;
    };
    if remote.is_empty() || branch.is_empty() {
        return false;
    }

    // Step 2: ask the remote whether the branch ref still exists.
    // `--exit-code` makes ls-remote return 2 when the ref is absent,
    // 0 when present, non-zero-non-2 on transport errors. We treat
    // "absent" as "gone" and anything else (present, transport
    // error) as "not gone" — keeping the safe default.
    let ls_out = match std::process::Command::new("git")
        .args([
            "-C",
            &path_str,
            "ls-remote",
            "--exit-code",
            "--heads",
            remote,
            branch,
        ])
        .output()
    {
        Ok(out) => out,
        Err(_) => return false,
    };
    // Exit code 2 = ref absent. Everything else (0 = present, 128 =
    // transport error, etc.) = don't claim "gone".
    matches!(ls_out.status.code(), Some(2))
}

/// Extract a worktree's short card id from its directory name. The
/// convention from `airc work claim` is the first 8 hex chars of the
/// card UUID; allow trailing extras (slash, branch slug) for
/// robustness against future naming changes.
pub(crate) fn parse_worktree_short_id(basename: &str) -> Option<String> {
    let trimmed = basename.trim().trim_end_matches('/');
    let prefix: String = trimmed.chars().take(8).collect();
    if prefix.len() != 8 || !prefix.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    Some(prefix)
}

/// Probe `path` for any state that means "this worktree carries
/// work the operator has not yet captured upstream." Two passes:
///
///   1. `git status --porcelain` — catches uncommitted-in-working-tree
///      AND untracked files. Non-empty ⇒ Dirty.
///   2. `git rev-list --count @{u}..HEAD` — catches committed-but-
///      not-pushed work. Non-zero ⇒ Dirty. The branch survives
///      the per-card cleanup (because `git branch -d` refuses
///      unmerged), but per `[[local-worktree-is-temp-dir]]` the
///      worktree's *files on disk* are the WIP an operator may
///      still be editing; we don't remove them silently.
///
/// Probe failures (git not on PATH, broken repo, branch with no
/// upstream tracking, detached HEAD) ⇒ Unknown. The classifier
/// treats Unknown as SkipNotGit — refuse to remove anything we
/// can't confidently classify. Per `[[no-fallbacks-ever]]` we
/// surface the ambiguity rather than assume Clean.
///
/// PR #1105 reviewer round 1: the prior version only ran the
/// porcelain probe and silently let committed-but-unpushed work
/// classify as Clean → Removable. A closed-card worktree with
/// unpushed WIP would have been destroyed by `--force`. This is
/// the WIP-outranks-hygiene contract the PR claims to enforce.
fn probe_dirty_status(path: &std::path::Path) -> DirtyStatus {
    let effective = resolve_worktree_path(path);
    probe_dirty_status_at(&effective)
}

/// Resolve `<short>/` to the actual git worktree path. Some repos
/// (continuum is the canonical case) live at `<short>/src/` because
/// `src/` is the workspace root the operator clones into. The
/// `airc work claim` canonical convention is `<short>/`, but
/// `git worktree add <short>/src` is a common manual pattern that
/// leaves the parent `<short>/` as a non-git directory containing
/// a single `src/` worktree.
///
/// This helper:
///
///   1. If `path` IS a git worktree (a `.git` file/dir exists OR
///      `git rev-parse --git-dir` succeeds), return `path`.
///   2. Else if `path/src/` IS a git worktree, return `path/src/`.
///   3. Else return `path` unchanged — downstream probes will
///      classify it as `Unknown`/`SkipNotGit` and the operator
///      sees the diagnostic.
///
/// One-level nested fallback ONLY. Arbitrary deep nesting is
/// out-of-scope (no observed wild case); per `[[no-fallbacks-ever]]`
/// the helper documents exactly the two layouts it accepts.
///
/// Card 83a5624e follow-up to c9b28925: the merger's
/// `cleanup_card_worktree` hook (and the manual `airc work cleanup`)
/// both probe `~/.airc/worktrees/<short>/` directly. Worktrees
/// allocated at `<short>/src/` were stranded across merges
/// (observed 2026-06-05 with continuum PRs #1530 / #1531). This
/// helper closes that gap.
fn resolve_worktree_path(path: &std::path::Path) -> std::path::PathBuf {
    if is_git_worktree(path) {
        return path.to_path_buf();
    }
    let nested = path.join("src");
    if is_git_worktree(&nested) {
        return nested;
    }
    path.to_path_buf()
}

/// Cheap heuristic: `.git` (file or dir) at the path's top level
/// is a strong signal of a git worktree. Avoids spawning `git`
/// just to ask. Both regular worktrees (`.git/` dir) and linked
/// worktrees (`.git` file pointing at the main repo's
/// `worktrees/<name>` metadata dir) qualify.
fn is_git_worktree(path: &std::path::Path) -> bool {
    if !path.is_dir() {
        return false;
    }
    let dot_git = path.join(".git");
    dot_git.exists()
}

/// The inner probe — runs against the ALREADY-resolved git
/// worktree path. Tests that exercise the porcelain + upstream
/// branches directly call this; production callers go through
/// `probe_dirty_status` which resolves the nested-layout first.
fn probe_dirty_status_at(path: &std::path::Path) -> DirtyStatus {
    let path_str = path.to_string_lossy().to_string();

    // Step 1: porcelain probe.
    let porcelain_out = match std::process::Command::new("git")
        .args(["-C", &path_str, "status", "--porcelain"])
        .output()
    {
        Ok(out) => out,
        Err(_) => return DirtyStatus::Unknown,
    };
    if !porcelain_out.status.success() {
        return DirtyStatus::Unknown;
    }
    if !String::from_utf8_lossy(&porcelain_out.stdout)
        .trim()
        .is_empty()
    {
        return DirtyStatus::Dirty;
    }

    // Step 2: unpushed-commits probe. `git rev-list --count @{u}..HEAD`
    // counts commits reachable from HEAD but not from the upstream
    // tracking branch.
    let unpushed_out = match std::process::Command::new("git")
        .args(["-C", &path_str, "rev-list", "--count", "@{u}..HEAD"])
        .output()
    {
        Ok(out) => out,
        Err(_) => return DirtyStatus::Unknown,
    };
    if unpushed_out.status.success() {
        let count_text = String::from_utf8_lossy(&unpushed_out.stdout)
            .trim()
            .to_string();
        let count: u64 = count_text.parse().unwrap_or(0);
        if count > 0 {
            return DirtyStatus::Dirty;
        }
        return DirtyStatus::Clean;
    }

    // Card 8a3082c4: the @{u} probe failed — likely "no upstream
    // configured" (every worktree spawned by `airc work claim` is
    // born on a fresh `<short>/<slug>` branch that's never been
    // pushed) or a detached HEAD. Rather than blanket-Unknown the
    // worktree, fall through to a SECOND positive-proof check: are
    // every commit on HEAD already present in `origin/<default>`,
    // either by SHA equality or by patch-id equality (squash merge)?
    //
    // This is NOT a [[no-fallbacks-ever]] violation — it's a
    // *stricter* check than "ahead==0 of upstream." The original
    // upstream probe trusted the operator's per-branch tracking
    // config; this trusts only what's verifiably already in the
    // remote default branch. The branch will be deleted; the test
    // for "is anything lost?" is "is any commit unique to this
    // branch vs origin/<default>?" — `git cherry` answers exactly
    // that, by patch-id, so squash-merged commits count as already-
    // applied.
    //
    // Concrete case this fixes: today's continuum #1547 → card
    // 8a3082c4. The auto-spawn created
    // `~/.airc/worktrees/8a3082c4/` on branch
    // `8a3082c4/fix-probes-rolling-log-fmt-layer-default` with no
    // upstream. The actual work landed on a DIFFERENT branch which
    // was squash-merged. `airc work merge` then ran cleanup and the
    // classifier refused to remove the worktree, leaving an orphan
    // that needs manual `git -C ... status` inspection. With this
    // fallback the cleanup correctly recognizes "every commit here
    // is already in origin/canary by patch-id."
    is_clean_via_cherry_against_origin_head(&path_str)
}

/// Returns [`DirtyStatus::Clean`] iff there exists at least one
/// well-known integration ref `origin/<name>` where every commit
/// reachable from HEAD is also reachable (by SHA OR patch-id) from
/// that ref. "Captured in at least one integration branch" is the
/// safety property: if `origin/canary` has all the patches and the
/// PR was merged there, work isn't lost when we delete the local
/// branch — even if `origin/main` hasn't been promoted yet.
///
/// Returns [`DirtyStatus::Dirty`] when every candidate ref we try
/// shows at least one `+` line (a commit not yet captured). The
/// branch carries genuinely unique work; refuse to remove.
///
/// Returns [`DirtyStatus::Unknown`] when no candidate ref exists on
/// the remote (we can't form an opinion). Safety posture: refuse
/// to claim Clean without positive proof.
///
/// ## Why a list of candidates
///
/// Per-card branches auto-spawned by `airc work claim` start with no
/// upstream tracking, so the standard `@{u}..HEAD` probe errors out.
/// The cleanup path needs a fallback that proves "work is captured
/// in the integration branch tree" even when the local branch's
/// upstream config is missing.
///
/// `git symbolic-ref refs/remotes/origin/HEAD` returns the repo's
/// default branch (usually `main` or `master`), but the PR may have
/// merged to a different integration ref like `canary` or
/// `rust-rewrite`. Trying the full known list catches every
/// real-world layout we ship (continuum: main + canary; airc:
/// rust-rewrite; generic OSS: main/master) without threading the
/// card's PR base through every call site.
///
/// The risk of a false positive (patch-ids accidentally matching on
/// an unrelated branch) is negligible — patch-ids are SHA1-based and
/// don't collide by accident across two commits with different
/// content.
fn is_clean_via_cherry_against_origin_head(path_str: &str) -> DirtyStatus {
    // Candidate integration refs in priority order. `origin/HEAD`
    // first because it's the operator-configured default; the named
    // refs cover repos whose PRs typically land somewhere other than
    // origin/HEAD (continuum: PRs land on canary; airc: rust-rewrite).
    const CANDIDATES: &[&str] = &[
        "origin/HEAD",
        "origin/main",
        "origin/master",
        "origin/canary",
        "origin/rust-rewrite",
        "origin/develop",
    ];

    let mut any_existed = false;
    for candidate in CANDIDATES {
        // Skip non-existent refs so the loop only considers refs the
        // local repo actually knows about. `rev-parse --verify` is
        // the cheap existence check — succeeds when the ref
        // resolves, fails (non-zero exit) when it doesn't.
        let exists = std::process::Command::new("git")
            .args(["-C", path_str, "rev-parse", "--verify", candidate])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false);
        if !exists {
            continue;
        }
        any_existed = true;

        // `git cherry <upstream> HEAD` lists each commit reachable
        // from HEAD but not from <upstream>. `+ <sha>` = patch-id
        // NOT present upstream (unique work). `- <sha>` = patch-id
        // IS present upstream (squash-merged or cherry-picked).
        // Empty output ⇒ HEAD equals upstream ⇒ trivially Clean.
        let cherry_out = match std::process::Command::new("git")
            .args(["-C", path_str, "cherry", candidate])
            .output()
        {
            Ok(out) => out,
            Err(_) => continue,
        };
        if !cherry_out.status.success() {
            continue;
        }
        let cherry_text = String::from_utf8_lossy(&cherry_out.stdout).to_string();
        let has_unique = cherry_text
            .lines()
            .any(|line| line.trim_start().starts_with('+'));
        if !has_unique {
            // BIGMAMA review fix on PR #1200 (round 2): `git cherry`
            // walks commits by patch-id, which SKIPS MERGE COMMITS
            // entirely (they have multiple parents → no canonical
            // patch-id). An "evil merge" — unique content living only
            // in a merge commit's conflict-resolution diff — emits
            // zero `+` lines, so `has_unique` reads false and we'd
            // return Clean → cleanup DELETES branches whose unique
            // work lives in a merge. Data loss.
            //
            // BIGMAMA caught the first defense attempt:
            // `git diff --quiet candidate..HEAD` is an endpoint diff
            // between two trees, NOT a "what's on HEAD that isn't on
            // candidate" probe. In the normal squash-merge case
            // candidate has advanced past HEAD (other PRs landed
            // since this one's branch point), so the diff is
            // non-empty and the check fires false-positive on every
            // mergeable branch.
            //
            // Correct defense: enumerate the merge commits reachable
            // from HEAD but NOT from candidate. `git rev-list
            // candidate..HEAD --merges` returns exactly those. Cherry
            // already vouched for the non-merge patches being present
            // upstream; if there are no extra merge commits, every
            // commit (non-merge AND merge) is accounted for and the
            // branch is genuinely Clean. If ANY merge exists in that
            // range, cherry's verdict is incomplete — refuse to
            // delete rather than guess whether the merge brought in
            // unique resolution content.
            let extra_merges = std::process::Command::new("git")
                .args([
                    "-C",
                    path_str,
                    "rev-list",
                    "--count",
                    "--merges",
                    &format!("{candidate}..HEAD"),
                ])
                .output()
                .ok()
                .and_then(|o| {
                    if o.status.success() {
                        String::from_utf8(o.stdout).ok()
                    } else {
                        None
                    }
                })
                .and_then(|s| s.trim().parse::<u64>().ok());
            if let Some(0) = extra_merges {
                // Cherry says non-merge patches are upstream AND there
                // are no extra merge commits to worry about. The
                // branch's work is fully captured; safe to delete.
                return DirtyStatus::Clean;
            }
            // Either rev-list failed (treat as can't-prove) or there
            // are merges on HEAD outside candidate (potential evil
            // merge). Keep iterating — another candidate might
            // include the merges; if none does, fall through to the
            // Dirty arm below.
        }
    }

    if any_existed {
        // We checked at least one candidate and ALL of them showed
        // unique commits on HEAD. The branch genuinely has work not
        // captured anywhere — refuse to remove.
        DirtyStatus::Dirty
    } else {
        // No candidate ref existed in the local repo. We can't form
        // a positive-proof opinion. Refuse to classify per
        // `[[no-fallbacks-ever]]` rather than guess.
        DirtyStatus::Unknown
    }
}

/// Run `git worktree remove <path>` against the worktree's repo root.
/// Failure surfaces as a stringified error so the batch loop can
/// log + continue.
///
/// **Submodules.** `git worktree remove` refuses worktrees that contain
/// submodules ("working trees containing submodules cannot be moved or
/// removed"). Continuum is the canonical example — it has llama.cpp +
/// whisper.cpp vendored as submodules. Verified live on Joel's machine
/// 2026-06-07: classifier correctly says Removable, git refuses the
/// remove, worktree leaks anyway. The fix: on that specific error,
/// fall back to `rm -rf` + `git worktree prune` on the repo root. The
/// pre-cleanup classifier already proved the worktree is `Clean` (no
/// uncommitted/unpushed work in either the outer repo OR the submodules
/// reachable via `git status --porcelain`), so `rm -rf` doesn't risk
/// the operator's WIP.
fn git_worktree_remove(path: &std::path::Path) -> Result<(), String> {
    let path_str = path.to_string_lossy().to_string();
    // First find the repo's git-common-dir so `git worktree remove` runs
    // from the right place. Without `-C path`, git would refuse from the
    // worktree itself ("cannot remove main working tree").
    let common_out = std::process::Command::new("git")
        .args(["-C", &path_str, "rev-parse", "--git-common-dir"])
        .output()
        .map_err(|e| format!("spawn git rev-parse: {e}"))?;
    if !common_out.status.success() {
        return Err(format!(
            "git rev-parse --git-common-dir failed: {}",
            String::from_utf8_lossy(&common_out.stderr).trim()
        ));
    }
    let common_dir = String::from_utf8(common_out.stdout)
        .map_err(|e| format!("git common-dir utf8: {e}"))?
        .trim()
        .to_string();
    // `git-common-dir` is the .git directory; the main worktree's repo
    // root is its parent.
    let repo_root = std::path::Path::new(&common_dir)
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|| common_dir.clone());
    let rm_out = std::process::Command::new("git")
        .args(["-C", &repo_root, "worktree", "remove", &path_str])
        .output()
        .map_err(|e| format!("spawn git worktree remove: {e}"))?;
    if rm_out.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&rm_out.stderr).to_string();

    // Submodule fallback. The classifier already proved the worktree
    // is Clean — uncommitted/unpushed WIP would have surfaced as
    // SkipDirty. Safe to nuke the directory and let git's metadata
    // catch up via `worktree prune`.
    if stderr.contains("contains submodules") || stderr.contains("containing submodules") {
        std::fs::remove_dir_all(path).map_err(|e| {
            format!(
                "git refused worktree remove (submodules) and rm -rf {} failed: {e}",
                path.display()
            )
        })?;
        let prune_out = std::process::Command::new("git")
            .args(["-C", &repo_root, "worktree", "prune"])
            .output()
            .map_err(|e| format!("spawn git worktree prune: {e}"))?;
        if !prune_out.status.success() {
            return Err(format!(
                "rm -rf {} succeeded but git worktree prune failed: {}",
                path.display(),
                String::from_utf8_lossy(&prune_out.stderr).trim()
            ));
        }
        return Ok(());
    }

    Err(format!("git worktree remove failed: {}", stderr.trim()))
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

/// Card 09fddedd: `airc work relink <CARD_ID> --pr <number-or-url>` —
/// supersede a card's stale PR link with a successor PR. Thin
/// orchestration over `work_commands_gh::relink_card_pr` — attach,
/// parse the card id + PR spec, delegate. Sibling of [`run_link`].
pub async fn run_relink(
    home: &Path,
    card_id: String,
    pr: String,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;
    let pr_number = parse_pr_spec(&pr)?;
    crate::work_commands_gh::relink_card_pr(&airc, card_uuid, pr_number).await
}

/// Parse a `--pr` argument that is either a bare PR number (`1137`) or
/// a full GitHub PR URL (`https://github.com/owner/repo/pull/1137`).
/// Anything else is a loud error — no guessing, no substring scraping.
fn parse_pr_spec(input: &str) -> Result<u64, Box<dyn std::error::Error>> {
    let trimmed = input.trim();
    if let Ok(number) = trimmed.parse::<u64>() {
        return Ok(number);
    }
    // URL form: require the `/pull/<number>` path segment explicitly
    // so an arbitrary URL (or a branch name with digits) can't sneak
    // through as a PR number.
    if let Some((_, tail)) = trimmed.split_once("/pull/") {
        let digits = tail.split(['/', '?', '#']).next().unwrap_or("");
        if let Ok(number) = digits.parse::<u64>() {
            return Ok(number);
        }
    }
    Err(format!(
        "--pr {input:?} is neither a PR number nor a GitHub PR URL \
         (expected e.g. 1137 or https://github.com/owner/repo/pull/1137)"
    )
    .into())
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
    use airc_work::model::CardState;

    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = parse_work_card_id(&card_id)?;

    let board = airc
        .work_board_complete(airc_lib::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
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

    // Card c1090a24: backend selected by production_gh_client —
    // ReqwestGhClient by default (no per-call gh spawn on the gate +
    // merge path), shell only by explicit opt-out or loud fallback.
    let gh = crate::gh_reqwest::production_gh_client();
    let baseline = crate::merger::fetch_baseline_failures(gh.as_ref()).await;
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

    match crate::merger::check_pr_gate(gh.as_ref(), &pr, &baseline, policy).await {
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
            mark_merged_and_reclaim(&airc, card_uuid, pr.clone(), now_ms).await?;
            println!(
                "merged: card={card_uuid} pr=#{n} repo={r}",
                n = pr.number,
                r = pr.repo,
            );
            Ok(())
        }
        Ok(crate::merger::GateResult::AlreadyMerged { merged_at_ms }) => {
            // Card acd72c81 follow-up: PR is already merged on
            // GitHub. The manual `airc work merge` path reconciles
            // by emitting `PullRequestMerged` — no `gh pr merge`
            // call needed.
            if dry_run {
                println!(
                    "merge_gate: ALREADY_MERGED — card={card_uuid} pr=#{n} repo={r} \
                     (would reconcile)",
                    n = pr.number,
                    r = pr.repo,
                );
                return Ok(());
            }
            mark_merged_and_reclaim(&airc, card_uuid, pr.clone(), merged_at_ms).await?;
            println!(
                "reconciled: card={card_uuid} pr=#{n} repo={r} (PR was already merged on GitHub)",
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

    // Card 09fddedd — `--pr <number-or-url>` parser for `airc work
    // relink`. Loud failures only: anything that is not a bare PR
    // number or a URL with an explicit `/pull/<n>` segment refuses.

    #[test]
    fn parse_pr_spec_accepts_bare_number_and_pull_url() {
        assert_eq!(parse_pr_spec("1137").unwrap(), 1137);
        assert_eq!(parse_pr_spec("  1137 ").unwrap(), 1137);
        assert_eq!(
            parse_pr_spec("https://github.com/CambrianTech/airc/pull/1137").unwrap(),
            1137
        );
        // Trailing path / query / fragment after the number is fine —
        // gh emits and humans paste all three shapes.
        assert_eq!(
            parse_pr_spec("https://github.com/CambrianTech/airc/pull/1137/files").unwrap(),
            1137
        );
        assert_eq!(
            parse_pr_spec("https://github.com/CambrianTech/airc/pull/1137?diff=split").unwrap(),
            1137
        );
        assert_eq!(
            parse_pr_spec("https://github.com/CambrianTech/airc/pull/1137#discussion_r1").unwrap(),
            1137
        );
    }

    #[test]
    fn parse_pr_spec_refuses_non_pr_shapes_loudly() {
        // what this catches: the parser degrading into digit-scraping.
        // An issue URL, a branch name with digits, or an empty string
        // must refuse — guessing a PR number here would relink a card
        // to an arbitrary PR.
        for input in [
            "",
            "abc",
            "-5",
            "https://github.com/CambrianTech/airc/issues/1137",
            "https://github.com/CambrianTech/airc/pull/",
            "https://github.com/CambrianTech/airc/pull/not-a-number",
            "feat/1137-branch",
        ] {
            assert!(
                parse_pr_spec(input).is_err(),
                "parse_pr_spec({input:?}) must refuse, not guess"
            );
        }
    }

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

    // Card c9b28925 — `airc work cleanup` classifier tests. Pure
    // functions, no real filesystem or daemon — every Disposition
    // gets exercised at least once.

    #[test]
    fn parse_worktree_short_id_accepts_canonical_shape() {
        assert_eq!(
            parse_worktree_short_id("acd72c81"),
            Some("acd72c81".to_string())
        );
        assert_eq!(
            parse_worktree_short_id("c9b28925/extra"),
            Some("c9b28925".to_string())
        );
    }

    #[test]
    fn parse_worktree_short_id_rejects_non_hex_basenames() {
        // Could be unrelated dirs in the lease zone — e.g. a README.
        assert_eq!(parse_worktree_short_id("README"), None);
        assert_eq!(parse_worktree_short_id("notes-here"), None);
        // Too short to be a card prefix.
        assert_eq!(parse_worktree_short_id("ab"), None);
    }

    #[test]
    fn classifier_closed_clean_removes() {
        let d = classify_worktree(Some(&CardState::Closed), &DirtyStatus::Clean, false);
        assert_eq!(d, Disposition::Removable);
    }

    #[test]
    fn classifier_merged_clean_removes() {
        let d = classify_worktree(Some(&CardState::Merged), &DirtyStatus::Clean, false);
        assert_eq!(d, Disposition::Removable);
    }

    #[test]
    fn classifier_active_states_keep() {
        for state in [
            CardState::Open,
            CardState::Claimed,
            CardState::InProgress,
            CardState::Review,
            CardState::Blocked,
        ] {
            let d = classify_worktree(Some(&state), &DirtyStatus::Clean, false);
            assert_eq!(d, Disposition::KeepActive, "state={state:?}");
        }
    }

    #[test]
    fn classifier_dirty_never_removes_even_when_card_closed() {
        // Operator's WIP outranks hygiene. Closed card + dirty
        // worktree still classifies as SkipDirty — print, never
        // remove silently.
        let d = classify_worktree(Some(&CardState::Closed), &DirtyStatus::Dirty, false);
        assert_eq!(d, Disposition::SkipDirty);
        let d = classify_worktree(Some(&CardState::Merged), &DirtyStatus::Dirty, false);
        assert_eq!(d, Disposition::SkipDirty);
    }

    #[test]
    fn classifier_no_card_returns_skip_unknown_card() {
        // Orphan worktree — basename matches the short-id pattern but
        // no card is on the current board. Could be from a deleted
        // card, a different scope, or scope drift. Surface but don't
        // touch.
        let d = classify_worktree(None, &DirtyStatus::Clean, false);
        assert_eq!(d, Disposition::SkipUnknownCard);
    }

    #[test]
    fn classifier_not_git_returns_skip_not_git() {
        // git status probe failure (not a git dir, permissions) ⇒
        // refuse to classify as either Clean or Dirty. SkipNotGit
        // surfaces the diagnostic and the operator decides.
        let d = classify_worktree(Some(&CardState::Closed), &DirtyStatus::Unknown, false);
        assert_eq!(d, Disposition::SkipNotGit);
    }

    // ─── upstream_gone: the fix for the recurring disk-full crash ────

    /// THE bug this slice fixes. PR merged via `gh pr merge --delete-
    /// branch`; airc projection hasn't caught up (no `airc work merge`
    /// path was used); card stays Review forever. Before this slice
    /// classifier returned KeepActive → worktree leaked → disk full.
    /// Now upstream_gone=true overrides card_state and returns
    /// Removable.
    #[test]
    fn classifier_upstream_gone_removes_even_when_card_review() {
        let d = classify_worktree(Some(&CardState::Review), &DirtyStatus::Clean, true);
        assert_eq!(d, Disposition::Removable);
    }

    /// Apply the same trump-card behavior to every non-dirty active
    /// state. Any worktree whose branch has been deleted on origin is
    /// dead weight regardless of where the kanban thinks the card is.
    #[test]
    fn classifier_upstream_gone_removes_for_every_active_state_when_clean() {
        for state in [
            CardState::Open,
            CardState::Claimed,
            CardState::InProgress,
            CardState::Review,
            CardState::Blocked,
        ] {
            let d = classify_worktree(Some(&state), &DirtyStatus::Clean, true);
            assert_eq!(
                d,
                Disposition::Removable,
                "upstream gone + clean must remove (state={state:?})"
            );
        }
    }

    /// WIP still outranks hygiene. Even with the upstream branch
    /// deleted, an uncommitted-or-unpushed local change must NOT be
    /// silently destroyed — surface as SkipDirty.
    #[test]
    fn classifier_upstream_gone_still_respects_dirty() {
        let d = classify_worktree(Some(&CardState::Review), &DirtyStatus::Dirty, true);
        assert_eq!(d, Disposition::SkipDirty);
    }

    /// Probe failures still surface as SkipNotGit even with the
    /// upstream-gone signal, because we can't trust we read the
    /// working tree state correctly.
    #[test]
    fn classifier_upstream_gone_still_respects_unknown_git() {
        let d = classify_worktree(Some(&CardState::Review), &DirtyStatus::Unknown, true);
        assert_eq!(d, Disposition::SkipNotGit);
    }

    /// Orphan worktree (no card) + upstream gone = Removable. This is
    /// the "card got pruned out of projection retention but the
    /// worktree is still on disk" path — same disk-full cause, different
    /// projection failure mode.
    #[test]
    fn classifier_upstream_gone_removes_orphan() {
        let d = classify_worktree(None, &DirtyStatus::Clean, true);
        assert_eq!(d, Disposition::Removable);
    }

    // ─── probe_dirty_status: PR #1105 reviewer round 1 fix ───────────
    //
    // Tests that exercise the actual `probe_dirty_status` function
    // against real git fixtures, not just the classifier's reaction
    // to the enum. The reviewer's BLOCK 2 was specifically that the
    // probe didn't check unpushed commits — these tests pin the
    // contract on the function itself, where the bug lived.

    /// Build a tempdir, init a bare "origin" repo + a working clone
    /// that tracks it. Returns the working clone's path (which we
    /// pass to `probe_dirty_status`) and the tempdir handle (which
    /// owns both, kept alive by the caller).
    fn git_fixture_with_upstream(seed_commit: bool) -> (std::path::PathBuf, tempfile::TempDir) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let origin = tmp.path().join("origin.git");
        let clone = tmp.path().join("clone");

        let run = |args: &[&str], cwd: Option<&std::path::Path>| {
            let mut cmd = std::process::Command::new("git");
            cmd.args(args);
            if let Some(d) = cwd {
                cmd.current_dir(d);
            }
            let out = cmd.output().expect("git spawn");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };

        run(&["init", "--bare", origin.to_str().unwrap()], None);
        run(
            &["clone", origin.to_str().unwrap(), clone.to_str().unwrap()],
            None,
        );
        // Configure identity so commits don't fail in CI containers.
        run(
            &[
                "-C",
                clone.to_str().unwrap(),
                "config",
                "user.email",
                "test@example.invalid",
            ],
            None,
        );
        run(
            &["-C", clone.to_str().unwrap(), "config", "user.name", "Test"],
            None,
        );

        if seed_commit {
            // Empty bare origin has no HEAD; create an initial commit
            // on clone + push so the upstream branch exists.
            std::fs::write(clone.join("README"), "seed\n").expect("write seed");
            run(&["-C", clone.to_str().unwrap(), "add", "README"], None);
            run(
                &["-C", clone.to_str().unwrap(), "commit", "-m", "seed"],
                None,
            );
            // Detect default branch (master vs main depending on git config)
            let branch_out = std::process::Command::new("git")
                .args([
                    "-C",
                    clone.to_str().unwrap(),
                    "rev-parse",
                    "--abbrev-ref",
                    "HEAD",
                ])
                .output()
                .expect("rev-parse");
            let branch = String::from_utf8(branch_out.stdout)
                .unwrap()
                .trim()
                .to_string();
            run(
                &[
                    "-C",
                    clone.to_str().unwrap(),
                    "push",
                    "-u",
                    "origin",
                    &branch,
                ],
                None,
            );
        }

        (clone, tmp)
    }

    #[test]
    fn probe_dirty_status_clean_when_committed_and_pushed() {
        let (clone, _tmp) = git_fixture_with_upstream(true);
        assert_eq!(
            probe_dirty_status(&clone),
            DirtyStatus::Clean,
            "freshly-cloned + pushed state must be Clean"
        );
    }

    #[test]
    fn probe_dirty_status_dirty_on_uncommitted_change() {
        let (clone, _tmp) = git_fixture_with_upstream(true);
        std::fs::write(clone.join("scratch"), "untracked\n").expect("write scratch");
        assert_eq!(
            probe_dirty_status(&clone),
            DirtyStatus::Dirty,
            "untracked file must classify as Dirty (porcelain stage)"
        );
    }

    /// PR #1105 reviewer round 1 BLOCK 2: the prior probe only ran
    /// `git status --porcelain` and silently classified
    /// committed-but-unpushed work as Clean. A closed-card worktree
    /// with this state would have been silently destroyed by
    /// `airc work cleanup --force`. This test pins the new
    /// behavior: unpushed commits ⇒ Dirty.
    #[test]
    fn probe_dirty_status_dirty_on_unpushed_commit() {
        let (clone, _tmp) = git_fixture_with_upstream(true);

        // Make a clean commit but DON'T push it.
        std::fs::write(clone.join("local-only"), "committed but not pushed\n").expect("write");
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&clone)
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["add", "local-only"]);
        run(&["commit", "-m", "local-only WIP"]);

        assert_eq!(
            probe_dirty_status(&clone),
            DirtyStatus::Dirty,
            "committed-but-unpushed work must classify as Dirty — \
             this is the [[local-worktree-is-temp-dir]] safety contract \
             reviewer round 1 BLOCK 2 caught"
        );
    }

    /// Card 8a3082c4: a branch with no upstream BUT whose HEAD is
    /// already captured upstream (the branch is just a no-op off
    /// origin's HEAD, the "auto-spawned worktree never used" case)
    /// must classify as Clean, not Unknown. Without this the cleanup
    /// classifier leaves orphan worktrees indefinitely whenever the
    /// auto-spawn never received commits.
    #[test]
    fn probe_dirty_status_clean_when_no_upstream_but_head_already_captured() {
        let (clone, _tmp) = git_fixture_with_upstream(true);

        // Switch to a fresh branch that has no upstream configured
        // but starts from origin/HEAD — no unique content.
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&clone)
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        run(&["checkout", "-b", "local-only-branch"]);

        assert_eq!(
            probe_dirty_status(&clone),
            DirtyStatus::Clean,
            "branch with no upstream but HEAD = origin/HEAD must be \
             Clean — `git cherry origin/HEAD HEAD` shows no `+` lines, \
             so deleting the branch loses nothing"
        );
    }

    /// Card 8a3082c4 mirror: a branch with no upstream AND a unique
    /// commit not yet captured upstream must classify as Dirty. The
    /// no-upstream-tracking case must NOT silently let real WIP slip
    /// through; positive-proof check is "every commit upstream by
    /// patch-id."
    #[test]
    fn probe_dirty_status_dirty_when_no_upstream_and_unique_commit() {
        let (clone, _tmp) = git_fixture_with_upstream(true);

        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(&clone)
                .output()
                .expect("git spawn");
            assert!(
                out.status.success(),
                "{args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        // Branch off, add a unique commit, leave no upstream config.
        run(&["checkout", "-b", "local-only-branch"]);
        std::fs::write(clone.join("unique"), "wip\n").expect("write");
        run(&["add", "unique"]);
        run(&["commit", "-m", "local WIP not on origin"]);

        assert_eq!(
            probe_dirty_status(&clone),
            DirtyStatus::Dirty,
            "branch with no upstream AND a commit not in any origin \
             ref must be Dirty — refusing to silently destroy WIP"
        );
    }

    #[test]
    fn probe_dirty_status_unknown_when_not_a_git_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        assert_eq!(
            probe_dirty_status(tmp.path()),
            DirtyStatus::Unknown,
            "non-git dir must be Unknown"
        );
    }

    // ─── Card 83a5624e: nested <short>/src/ layout ────────────────────
    //
    // Tests cover the substrate gap that stranded continuum PRs
    // #1530 and #1531's worktrees across merge: the cleanup probe
    // ran against the parent `<short>/` (not a git worktree) and
    // returned Unknown → SkipNotGit → no reclaim.

    #[test]
    fn resolve_worktree_path_returns_path_when_it_is_a_git_worktree() {
        let (clone, _tmp) = git_fixture_with_upstream(true);
        let resolved = resolve_worktree_path(&clone);
        assert_eq!(resolved, clone, "git worktree at top level → return as-is");
    }

    #[test]
    fn resolve_worktree_path_falls_back_to_nested_src() {
        // Build a layout like ~/.airc/worktrees/<short>/src where
        // <short>/ is just a container and <short>/src/ is the real
        // worktree. This is the continuum repo case observed in
        // the wild.
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("aabbccdd");
        std::fs::create_dir_all(&parent).expect("mkdir parent");
        let nested = parent.join("src");

        // Init the nested path as a git worktree.
        let init_out = std::process::Command::new("git")
            .args(["init", nested.to_str().unwrap()])
            .output()
            .expect("git init");
        assert!(init_out.status.success(), "git init nested: {:?}", init_out);

        let resolved = resolve_worktree_path(&parent);
        assert_eq!(
            resolved, nested,
            "parent isn't git → fall back to <parent>/src/ which is"
        );
    }

    #[test]
    fn resolve_worktree_path_returns_input_when_neither_is_git() {
        // Both <parent>/ and <parent>/src/ are non-git → return
        // <parent>/ unchanged. Downstream probe classifies as
        // Unknown → SkipNotGit (operator sees the diagnostic).
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("aabbccdd");
        std::fs::create_dir_all(parent.join("src")).expect("mkdir src");
        let resolved = resolve_worktree_path(&parent);
        assert_eq!(
            resolved, parent,
            "neither layout is git → return input unchanged for downstream diagnostic"
        );
    }

    #[test]
    fn probe_dirty_status_handles_nested_src_layout() {
        // End-to-end: probe a `<parent>/` where the actual git
        // worktree lives at `<parent>/src/`. Should return Clean
        // (not Unknown) because the nested fallback finds the real
        // git dir.
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("aabbccdd");
        std::fs::create_dir_all(&parent).expect("mkdir parent");
        let nested = parent.join("src");

        // Set up the nested clone with upstream tracking so the
        // probe's two-pass check returns Clean.
        let (real_clone, _real_tmp) = git_fixture_with_upstream(true);
        // Move the real clone's contents under <parent>/src/
        std::fs::rename(&real_clone, &nested).expect("rename real clone to nested");

        assert_eq!(
            probe_dirty_status(&parent),
            DirtyStatus::Clean,
            "probe must resolve <parent>/ → <parent>/src/ + return its real Clean state"
        );
    }

    /// Regression test for BIGMAMA review on PR #1198: the production
    /// fix at `run_cleanup:958` was `probe_upstream_gone(&effective)`,
    /// but the previous attempt at this test called probe_upstream_gone
    /// directly with both `&effective` and `&parent` — reverting the
    /// production line back to `&path` left the test green because
    /// it never touched the production call site.
    ///
    /// Defense: drive through `classify_worktree_path` (the extracted
    /// helper that wraps the same probes run_cleanup uses), feed it a
    /// nested-layout worktree with a deleted upstream, and assert the
    /// final disposition is `Removable`. `Removable` is only reachable
    /// when `upstream_gone == true` — which requires the helper to
    /// have correctly threaded the resolved `effective` path into
    /// `probe_upstream_gone`. If a future refactor reverts the helper
    /// to pass `&path`, the probe runs against the non-git `<parent>/`
    /// container, returns false, and the disposition falls back to
    /// SkipUnknownCard (no card) → this test goes red.
    #[test]
    fn classify_worktree_path_emits_removable_on_nested_deleted_upstream() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("aabbccdd");
        std::fs::create_dir_all(&parent).expect("mkdir parent");
        let nested = parent.join("src");

        let (real_clone, real_tmp) = git_fixture_with_upstream(true);
        std::fs::rename(&real_clone, &nested).expect("rename real clone to nested");

        // Delete the upstream branch on origin — the universal "PR
        // merged / branch abandoned" signal probe_upstream_gone keys
        // off of.
        let branch_out = std::process::Command::new("git")
            .args([
                "-C",
                nested.to_str().unwrap(),
                "rev-parse",
                "--abbrev-ref",
                "HEAD",
            ])
            .output()
            .expect("rev-parse");
        let branch = String::from_utf8(branch_out.stdout)
            .unwrap()
            .trim()
            .to_string();
        let origin = real_tmp.path().join("origin.git");
        let del = std::process::Command::new("git")
            .args([
                "-C",
                origin.to_str().unwrap(),
                "update-ref",
                "-d",
                &format!("refs/heads/{branch}"),
            ])
            .output()
            .expect("update-ref -d");
        assert!(
            del.status.success(),
            "delete origin ref failed: {}",
            String::from_utf8_lossy(&del.stderr)
        );

        // Drive through the SAME helper run_cleanup uses. The card
        // state is None (no projection match) — `upstream_gone`
        // short-circuits to Removable BEFORE the card-state branch is
        // even consulted, so the disposition signal IS the probe
        // signal. Revert the helper to use unresolved `path` and this
        // assertion fails because probe_upstream_gone(<parent>/) is
        // false → falls through to SkipUnknownCard.
        let (effective, dirty, disposition) = classify_worktree_path(&parent, None);
        assert_eq!(
            disposition,
            Disposition::Removable,
            "nested layout + deleted upstream MUST classify as Removable. \
             effective={effective:?}, dirty={dirty:?}, disposition={disposition:?}. \
             If this fails, classify_worktree_path stopped routing &effective \
             to probe_upstream_gone — re-check the production fix."
        );
    }

    #[test]
    fn classifier_handles_nested_src_layout_via_probe() {
        // End-to-end: nested worktree with a Closed card should
        // classify as Removable, not SkipNotGit. Pins the substrate
        // contract that fired the disk-full incident.
        let tmp = tempfile::tempdir().expect("tempdir");
        let parent = tmp.path().join("aabbccdd");
        std::fs::create_dir_all(&parent).expect("mkdir parent");
        let nested = parent.join("src");

        let (real_clone, _real_tmp) = git_fixture_with_upstream(true);
        std::fs::rename(&real_clone, &nested).expect("rename");

        let dirty = probe_dirty_status(&parent);
        assert_eq!(dirty, DirtyStatus::Clean);

        let disp = classify_worktree(Some(&CardState::Closed), &dirty, false);
        assert_eq!(
            disp,
            Disposition::Removable,
            "nested-layout + Closed card → Removable. Was SkipNotGit before \
             card 83a5624e — the bug the merger's cleanup hook stranded \
             continuum #1530/#1531 worktrees on."
        );
    }

    /// BIGMAMA review round 2 on PR #1200: the first evil-merge defense
    /// (`git diff --quiet candidate..HEAD`) was a TREE-ENDPOINT diff —
    /// non-empty whenever `candidate` has advanced past HEAD with any
    /// commits not on the PR (the normal squash-merge-with-other-PRs
    /// state). That made the defense fire false-positive on every
    /// mergeable branch — the cleanup wouldn't have classified ANY
    /// merged PR as Clean once canary moved on.
    ///
    /// This test pins the correct shape: cherry says no `+` lines
    /// (every non-merge patch is upstream) AND there are no extra
    /// merge commits in `candidate..HEAD` ⇒ defense allows Clean.
    /// Building a real squash-merge under tempdir is fiddly; cherry's
    /// patch-id match is the load-bearing equivalence — we cherry-pick
    /// the PR commit onto the upstream branch (same patch-id, distinct
    /// SHA) and then push other unrelated work onto upstream so the
    /// "candidate advanced" condition the first defense tripped on is
    /// present in the fixture.
    #[test]
    fn is_clean_via_cherry_returns_clean_when_squash_merged_and_candidate_advanced() {
        let (clone, tmp) = git_fixture_with_upstream(true);
        let origin = tmp.path().join("origin.git");
        let other = tmp.path().join("other_clone");

        let run = |args: &[&str], cwd: Option<&std::path::Path>| {
            let mut cmd = std::process::Command::new("git");
            cmd.args(args);
            if let Some(d) = cwd {
                cmd.current_dir(d);
            }
            let out = cmd.output().expect("git spawn");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let clone_str = clone.to_str().unwrap().to_string();
        let other_str = other.to_str().unwrap().to_string();

        // Detect default branch — fixture might be `main` or `master`
        // depending on the test host's git config.
        let branch_out = std::process::Command::new("git")
            .args(["-C", &clone_str, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("rev-parse");
        let default_branch = String::from_utf8(branch_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // PR branch with a commit ("feat.txt") — this is what we'll
        // pretend got squash-merged into the default branch.
        run(&["-C", &clone_str, "checkout", "-b", "pr-feat"], None);
        std::fs::write(clone.join("feat.txt"), "feat content\n").expect("write feat");
        run(&["-C", &clone_str, "add", "feat.txt"], None);
        run(&["-C", &clone_str, "commit", "-m", "pr: add feat"], None);
        run(&["-C", &clone_str, "push", "-u", "origin", "pr-feat"], None);
        let feat_sha_out = std::process::Command::new("git")
            .args(["-C", &clone_str, "rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        let feat_sha = String::from_utf8(feat_sha_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Land the same content on the default branch via a SECOND
        // clone (so the working tree doesn't collide with the PR
        // checkout), then push back to origin so it appears as
        // origin/<default>. Also add an unrelated commit so the
        // candidate has ADVANCED past HEAD — exactly the condition
        // the symmetric diff defense fired on.
        run(&["clone", origin.to_str().unwrap(), &other_str], None);
        run(
            &[
                "-C",
                &other_str,
                "config",
                "user.email",
                "test@example.invalid",
            ],
            None,
        );
        run(&["-C", &other_str, "config", "user.name", "Test"], None);
        // Fetch the pr-feat ref so cherry-pick can resolve it.
        run(&["-C", &other_str, "fetch", "origin", "pr-feat"], None);
        run(&["-C", &other_str, "cherry-pick", &feat_sha], None);
        std::fs::write(other.join("other.txt"), "other PR\n").expect("write other");
        run(&["-C", &other_str, "add", "other.txt"], None);
        run(
            &["-C", &other_str, "commit", "-m", "main: unrelated other PR"],
            None,
        );
        run(&["-C", &other_str, "push", "origin", &default_branch], None);

        // Refresh the PR clone's view of origin so cherry can see the
        // patch-id is present upstream.
        run(&["-C", &clone_str, "fetch", "origin"], None);

        // ASSERT: feat's patch-id is in origin/<default>, no extra
        // merges in candidate..HEAD, defense allows Clean. With the
        // OLD `git diff --quiet candidate..HEAD` defense this would
        // have come back Dirty because candidate has advanced past
        // HEAD via the "other PR" commit.
        let status = is_clean_via_cherry_against_origin_head(&clone_str);
        assert_eq!(
            status,
            DirtyStatus::Clean,
            "squash-merged PR with advanced candidate must be Clean — \
             the symmetric-diff defense fired false-positive here. \
             Defense must be `rev-list candidate..HEAD --merges` (zero \
             extra merges ⇒ cherry's verdict stands)."
        );
    }

    /// BIGMAMA review round 2 on PR #1200: the actual evil-merge case
    /// — a merge commit reachable from HEAD but not from candidate.
    /// `git cherry` skipped it (no canonical patch-id), so its
    /// conflict-resolution diff could carry unique content not
    /// captured upstream. The defense MUST refuse to classify Clean
    /// when extra merges exist; the conservative call is Dirty rather
    /// than guess whether the merge brought in unique resolution
    /// content.
    #[test]
    fn is_clean_via_cherry_refuses_when_extra_merge_lives_in_head() {
        let (clone, tmp) = git_fixture_with_upstream(true);
        let origin = tmp.path().join("origin.git");
        let other = tmp.path().join("other_clone");

        let run = |args: &[&str], cwd: Option<&std::path::Path>| {
            let mut cmd = std::process::Command::new("git");
            cmd.args(args);
            if let Some(d) = cwd {
                cmd.current_dir(d);
            }
            let out = cmd.output().expect("git spawn");
            assert!(
                out.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        let clone_str = clone.to_str().unwrap().to_string();
        let other_str = other.to_str().unwrap().to_string();
        let branch_out = std::process::Command::new("git")
            .args(["-C", &clone_str, "rev-parse", "--abbrev-ref", "HEAD"])
            .output()
            .expect("rev-parse");
        let default_branch = String::from_utf8(branch_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Pr commit on `pr-feat` — same as the clean case.
        run(&["-C", &clone_str, "checkout", "-b", "pr-feat"], None);
        std::fs::write(clone.join("feat.txt"), "feat content\n").expect("write feat");
        run(&["-C", &clone_str, "add", "feat.txt"], None);
        run(&["-C", &clone_str, "commit", "-m", "pr: add feat"], None);
        run(&["-C", &clone_str, "push", "-u", "origin", "pr-feat"], None);
        let feat_sha_out = std::process::Command::new("git")
            .args(["-C", &clone_str, "rev-parse", "HEAD"])
            .output()
            .expect("rev-parse");
        let feat_sha = String::from_utf8(feat_sha_out.stdout)
            .unwrap()
            .trim()
            .to_string();

        // Land feat's patch on default via a sibling clone.
        run(&["clone", origin.to_str().unwrap(), &other_str], None);
        // Different author identity than the PR clone so the
        // cherry-pick lands as a DIFFERENT SHA (otherwise git
        // optimizes back to the same commit and there's nothing to
        // merge — fixture degenerates to fast-forward).
        run(
            &[
                "-C",
                &other_str,
                "config",
                "user.email",
                "other@example.invalid",
            ],
            None,
        );
        run(&["-C", &other_str, "config", "user.name", "Other"], None);
        run(&["-C", &other_str, "fetch", "origin", "pr-feat"], None);
        run(&["-C", &other_str, "cherry-pick", &feat_sha], None);
        run(&["-C", &other_str, "push", "origin", &default_branch], None);

        // Now on pr: pull main back in via `git merge`. This creates
        // a merge commit M on pr-feat whose first parent is the
        // original feat commit, second parent is the cherry-picked
        // version on origin/<default>. cherry's patch-id match still
        // says "no unique commits" — but M itself is not on candidate.
        run(&["-C", &clone_str, "fetch", "origin"], None);
        run(
            &[
                "-C",
                &clone_str,
                "merge",
                "--no-ff",
                "-m",
                "pr: merge main",
                &format!("origin/{default_branch}"),
            ],
            None,
        );

        // ASSERT: defense refuses to classify as Clean. The exact
        // verdict from is_clean_via_cherry_against_origin_head is
        // Dirty (every candidate has either a `+` line or extra
        // merges; outer arm returns Dirty when any_existed).
        let status = is_clean_via_cherry_against_origin_head(&clone_str);
        assert_ne!(
            status,
            DirtyStatus::Clean,
            "PR branch with a merge commit not on candidate MUST NOT be \
             classified as Clean — cherry skips merges, so the merge \
             commit could be an evil merge carrying unique resolution \
             content. Conservative refuse: status = {status:?}"
        );
    }

    #[test]
    fn disposition_display_priority_orders_removable_first() {
        let mut order = [
            Disposition::SkipUnknownCard,
            Disposition::Removable,
            Disposition::KeepActive,
            Disposition::SkipDirty,
            Disposition::SkipNotGit,
        ];
        order.sort_by_key(|d| d.display_priority());
        assert_eq!(
            order,
            [
                Disposition::Removable,
                Disposition::KeepActive,
                Disposition::SkipDirty,
                Disposition::SkipNotGit,
                Disposition::SkipUnknownCard,
            ]
        );
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
    // Card edf3670c — every "PR merged" terminal path MUST route
    // through `mark_merged_and_reclaim`. The original bug shipped
    // because `run_merge` hand-paired `mark_pull_request_merged` +
    // missing-cleanup; the helper extraction + this source-text
    // contract test together pin the invariant going forward.
    //
    // Why a source-text test, not a unit test with a mock Airc:
    // `Airc` is a concrete struct (not a trait) and the cleanup
    // function shells out to `git`. Faking either is heavier than
    // the bug class warrants. The source-text scan catches the
    // exact regression pattern — a maintainer who reintroduces
    // raw `airc.mark_pull_request_merged(...)` calls at a NEW
    // terminal path will break this test until they route the new
    // path through the helper.
    // ---------------------------------------------------------------------

    /// Every direct caller of `airc.mark_pull_request_merged` in
    /// the substrate is a candidate for re-introducing the
    /// disk-full bug. Only the helper itself is allowed to make
    /// that call. Every other site MUST go through
    /// `mark_merged_and_reclaim`.
    ///
    /// This test scans `work_commands.rs` + `merger.rs` source
    /// (the two files that historically owned merge-terminal
    /// paths) and asserts that `mark_pull_request_merged(` appears
    /// at EXACTLY ONE call site (inside the helper). A maintainer
    /// who adds a new merge-terminal path with a raw
    /// `mark_pull_request_merged` call drops this count to >1 and
    /// breaks the test until they route through the helper —
    /// which automatically wires cleanup.
    #[test]
    fn every_merge_site_routes_through_helper() {
        // Scan production code only — strip the `#[cfg(test)]`
        // module before counting so this test's own references to
        // the method name don't inflate the total.
        fn production_only(src: &str) -> &str {
            src.split_once("#[cfg(test)]")
                .map(|(prod, _)| prod)
                .unwrap_or(src)
        }
        let work_commands_prod = production_only(include_str!("work_commands.rs"));
        let merger_prod = production_only(include_str!("merger.rs"));

        // Count direct method-call invocations — the
        // `.mark_pull_request_merged(` form catches both
        // `airc.mark_pull_request_merged(` and any chained
        // access. Doc-comments use backticks around the bare
        // name (`mark_pull_request_merged`) so they don't match.
        let total = work_commands_prod
            .matches(".mark_pull_request_merged(")
            .count()
            + merger_prod.matches(".mark_pull_request_merged(").count();

        assert_eq!(
            total, 1,
            "Found {total} direct callers of `.mark_pull_request_merged(` in \
             production code across work_commands.rs + merger.rs. Card edf3670c \
             contract: the helper `mark_merged_and_reclaim` is the ONLY allowed \
             caller, so every other terminal path inherits worktree cleanup \
             automatically. \n\n\
             If you added a new merge-terminal path: route it through \
             `mark_merged_and_reclaim(&airc, card_id, pr, merged_at_ms)` instead \
             of calling `mark_pull_request_merged` directly. The bug this test \
             prevents shipped to production once already; the recurring disk-full \
             crash on Joel's Intel Mac took out 60 GB per session before this fix."
        );
    }

    /// Companion to `every_merge_site_routes_through_helper`: the
    /// helper itself MUST contain a `cleanup_card_worktree` call.
    /// Without this, a maintainer could "simplify" the helper down
    /// to just `mark_pull_request_merged` and re-ship the bug at
    /// ALL four call sites at once. The function-body scan pins
    /// the cleanup wire inside the helper.
    #[test]
    fn helper_body_contains_cleanup_call() {
        let work_commands = include_str!("work_commands.rs");
        // Extract the body of mark_merged_and_reclaim by finding
        // the function signature + reading until the closing
        // brace at column 0. Substrate convention is one-fn per
        // top-level item with `pub`/`pub(crate)` declarations.
        let needle = "pub(crate) async fn mark_merged_and_reclaim(";
        let start = work_commands
            .find(needle)
            .expect("mark_merged_and_reclaim function must exist (card edf3670c)");
        // Read 4000 chars after the signature — generous bound
        // that covers the full body but bails out at the next
        // top-level item. Cheap fence; not a parser.
        let body_window = &work_commands[start..start.saturating_add(4000)];
        assert!(
            body_window.contains("cleanup_card_worktree("),
            "mark_merged_and_reclaim must call cleanup_card_worktree — that's \
             the entire point of the helper. Removing this call re-ships the \
             disk-full bug at all four merge-terminal paths simultaneously \
             (card edf3670c)."
        );
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
