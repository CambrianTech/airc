//! Card lifecycle handlers — `airc work create | seed | claim | release |
//! heartbeat | update | state | close`.
//!
//! Card c0bd865c phase 2 (5aa9e780): extracted from
//! `work_commands.rs` to shrink that file's 1823-line drift and give
//! consumer-embedders (continuum / hermes / openclaw / codex when they
//! read this code to plan integrations) a focused surface for the
//! card-state-machine.
//!
//! The state-machine transitions Open → Claimed → InProgress → Review
//! → Merged → Closed are governed here; the substrate-side guards
//! (`close_transition_allowed_from`, `cli_can_set_state_directly`,
//! `refusal_message`) refuse the persona-trash-the-card paths that
//! `airc-lib` doesn't yet refuse at the substrate layer.
//!
//! Cross-module helpers (parsers, `now_ms`, `spawn_claim_worktree`,
//! `open_pr_and_link`, `auto_spawn_review_card`) stay in
//! `work_commands.rs` and are called via the crate path. They're
//! `pub(crate)` so this module reaches them; phases 3-5 (`git`, `gh`,
//! `review`) will pull each into its own sibling module and tighten
//! the visibility back down.

use std::path::Path;

use airc_diagnostics::{DiagnosticCode, DiagnosticComponent, DiagnosticEvent};

use airc_lib::{
    CardState, ChangeWorkCardState, ClaimWorkCard, CreateWorkCard, ReleaseWorkClaim, RepoId,
    UpdateWorkCard, WorkBacklogSeedCandidate, WorkBacklogSeedOutcome,
};

use crate::lease;
use crate::work_cli::{CliCardState, CliPriority};

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
            lane_id: crate::work_commands::parse_optional_lane_id(lane_id.as_deref())?,
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
            lane_id: crate::work_commands::parse_optional_lane_id(lane_id.as_deref())?,
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
    let card_uuid = crate::work_commands::parse_work_card_id(&card_id)?;
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
    if let Err(error) = crate::work_commands::spawn_claim_worktree(&airc, card_uuid).await {
        eprintln!("airc: worktree spawn skipped — {error}");
    }
    Ok(())
}

pub async fn run_release(
    home: &Path,
    card_id: String,
    claim_id: Option<String>,
    reason: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = crate::commands::attached_airc(home).await?;
    let card_uuid = crate::work_commands::parse_work_card_id(&card_id)?;
    // Default: resolve THIS peer's active claim from the board so
    // callers don't have to track claim_ids the system already knows
    // (kink card acb8bfcd: release ergonomics).
    let claim_uuid = match claim_id {
        Some(raw) => crate::work_commands::parse_claim_id(&raw)?,
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
    let card_uuid = crate::work_commands::parse_work_card_id(&card_id)?;
    let claim_uuid = crate::work_commands::parse_claim_id(&claim_id)?;
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

pub async fn run_update(
    home: &Path,
    card_id: String,
    title: Option<String>,
    body: Option<String>,
    priority: Option<CliPriority>,
) -> Result<(), Box<dyn std::error::Error>> {
    let card_uuid = crate::work_commands::parse_work_card_id(&card_id)?;
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
    let card_uuid = crate::work_commands::parse_work_card_id(&card_id)?;
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
        let board = airc.work_board(usize::MAX).await?;
        let card = board.card(card_uuid).ok_or_else(|| {
            format!("card {card_uuid} not visible in current room's board projection")
        })?;
        if !close_transition_allowed_from(card.state) {
            return Err(format!(
                "refusing to close card {card_uuid}: current state is {actual:?}, but \
                 Closed requires Merged (PR merged) or {{Open, Claimed, Blocked}} \
                 (cancellation before work landed).\n\n\
                 If work is in flight ({actual:?}), the next step is:\n  \
                 - state Review: open a PR via `airc work state {card_uuid} review`\n  \
                 - wait for the PR to merge (state → Merged via gh observer)\n  \
                 - THEN `airc work close {card_uuid}` succeeds.\n\n\
                 If you want to abandon the work, `airc work release {card_uuid}` \
                 drops the claim and returns the card to its prior state — close \
                 from there.",
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
        if let Err(error) = crate::work_commands::open_pr_and_link(&airc, card_uuid).await {
            eprintln!("airc: gh pr create skipped — {error}");
        }
    }
    Ok(())
}

pub(crate) fn cli_can_set_state_directly(target: CardState) -> bool {
    !matches!(target, CardState::Merged)
}

pub(crate) fn refusal_message(card_uuid: airc_lib::WorkCardId, target: CardState) -> String {
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

pub(crate) fn close_transition_allowed_from(state: CardState) -> bool {
    matches!(
        state,
        CardState::Merged | CardState::Open | CardState::Claimed | CardState::Blocked
    )
}
