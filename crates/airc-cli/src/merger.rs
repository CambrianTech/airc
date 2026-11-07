//! Continuous-merge background loop — auto-merges Review-state cards
//! whose linked PRs have green CI, then publishes `PullRequestMerged`
//! so the projection transitions the card to `Merged`.
//!
//! Card f16650cd. Joel directive 2026-05-28: "we do NOT just leave
//! branches laying around after CI passes" + "we need continuous
//! merging somewhere". Without this, an agent has to remember to run
//! `gh pr merge` — a discipline that breaks the moment a less-capable
//! persona attaches. Substrate-driven; agents go idle, work keeps
//! shipping.
//!
//! ## What this gate checks
//!
//! On every tick (default 30s):
//!
//! 1. Read the work board projection.
//! 2. For every card in `Review` state with a `pull_request`:
//!    - Fetch the PR's check rollup via `gh pr view --json statusCheckRollup`.
//!    - Apply the *strictly-less-red-than-base* doctrine refinement: a PR
//!      is mergeable when no NEW red was introduced vs. the base branch
//!      (the bar agreed during #1033 — pre-existing red doesn't block;
//!      new red does). For the first cut we use a simpler form: no
//!      FAILURE/CANCELLED conclusions in the rollup. The pre-existing-red
//!      bypass is carded as a follow-up.
//!    - If the gate passes: `gh pr merge --squash --delete-branch`.
//!    - On success, publish `MarkPullRequestMerged`. The projection
//!      transitions the card to `Merged`; a separate observer (or the
//!      next merger tick) closes the card.
//!
//! ## What this gate does NOT yet check (future cards)
//!
//! - Peer-LGTM convention (card 267d68f5): for multi-author rooms we
//!   should require a non-author LGTM. For now, ANY Review card
//!   auto-merges if green — appropriate for single-author scopes; for
//!   multi-author, the doctrine says "review before Review state."
//! - Card-close on merged (currently relies on projection state and an
//!   agent's explicit close; an observer would automate it).
//!
//! ## Singleton enforcement
//!
//! A naive multi-tab launch would race on `gh pr merge` and one would
//! fail with "PR already merged." A file lock at
//! `<home>/merger.lock` prevents two merger processes per scope. The
//! lock is `flock(LOCK_EX | LOCK_NB)` — non-blocking; a second launch
//! exits cleanly with a "merger already running" message.

use std::collections::{hash_map::Entry, HashMap, HashSet};
use std::path::Path;
use std::time::Duration;

use airc_lib::{Airc, WorkCard};

/// Run the continuous-merge loop until shutdown (Ctrl-C / SIGTERM).
/// Default interval at the CLI layer is 30 seconds — CI pipelines on
/// this project take 2–5 minutes; polling faster wastes API calls
/// without shipping more.
///
/// Holds an exclusive file lock at `<home>/merger.lock` so two
/// invocations against the same scope don't race on `gh pr merge`.
pub async fn run(
    home: &Path,
    interval: Duration,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let _lock = acquire_singleton_lock(home)?;

    let socket = crate::cli::default_socket_path_in(home);
    crate::commands::ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    let airc = Airc::attach(home, socket).await?;

    eprintln!(
        "airc-merger: started (home={}, interval={:?}, dry_run={})",
        home.display(),
        interval,
        dry_run
    );
    eprintln!("airc-merger: peer_id={}", airc.peer_id());

    // Cards dec35ec7 + a094aa81: one GhClient per merger session,
    // passed downstream as `&dyn GhClient` so the gate logic never
    // knows which impl it's on. Card c1090a24: the impl is selected
    // by production_gh_client — ReqwestGhClient by default (no
    // per-call gh spawn), ShellGhClient only via explicit
    // AIRC_GH_BACKEND=shell or a loudly-warned construction fallback.
    let gh = crate::gh_reqwest::production_gh_client();

    let mut ticker = tokio::time::interval(interval);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                eprintln!("airc-merger: shutdown signal received, exiting cleanly");
                return Ok(());
            }
            _ = ticker.tick() => {
                if let Err(error) = tick_once(gh.as_ref(), &airc, dry_run).await {
                    // A tick failing should NOT bring down the loop —
                    // gh might be rate-limited, the daemon might be
                    // momentarily unreachable, etc. Log and continue.
                    eprintln!("airc-merger: tick failed: {error}");
                }
            }
        }
    }
}

/// One pass over the board: scan eligible cards, gate, merge.
async fn tick_once(
    gh: &dyn crate::gh_client::GhClient,
    airc: &Airc,
    dry_run: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    // Card 1291173d: the COMPLETE board, not a recent window — a
    // Review card whose last event scrolled out of a fixed 256-event
    // window would silently never merge. `work_board_complete` is the
    // documented surface for scheduling code, and with the persistent
    // projection cache each tick costs one incremental resume from
    // the last-applied cursor instead of a full event-log replay.
    let room = airc.current_room().await?;
    let board = airc.work_board_in(&room).await?;
    let snapshot = board.snapshot();

    // Card 045083e8: a room can contain cards from multiple repositories
    // and target branches. Reuse only the same immutable target's baseline,
    // and discard all cached observations at the end of this tick.
    let mut baselines = BaselineCache::default();

    // Card 7ed1ac4f: snapshot `now_ms` ONCE per tick so every per-card
    // gate sees the same wall clock. Otherwise a long-running tick
    // (rare but possible under heavy gh latency) could promote a
    // pending check across the timeout mid-iteration, which would be
    // observable as "first-pass refused, second-pass accepted" on
    // the same data — confusing, and breaks the determinism the
    // dry-run mode promises.
    let policy = GatePolicy::default_for_merger(now_ms());

    for card in &snapshot.cards {
        let Some(decision) = evaluate(gh, card, &mut baselines, policy).await? else {
            continue;
        };
        match decision {
            MergeDecision::Merge(pr) => {
                if dry_run {
                    eprintln!(
                        "airc-merger: [DRY-RUN] would merge card={} pr=#{} ({})",
                        card.card_id, pr.number, pr.repo
                    );
                    continue;
                }
                match perform_merge(gh, card, &pr, airc, &room).await {
                    Ok(()) => eprintln!(
                        "airc-merger: merged card={} pr=#{} ({})",
                        card.card_id, pr.number, pr.repo
                    ),
                    Err(error) => eprintln!(
                        "airc-merger: merge failed card={} pr=#{}: {error}",
                        card.card_id, pr.number
                    ),
                }
            }
            MergeDecision::Reconcile(pr, merged_at_ms) => {
                if dry_run {
                    eprintln!(
                        "airc-merger: [DRY-RUN] would reconcile already-merged card={} pr=#{} ({})",
                        card.card_id, pr.number, pr.repo
                    );
                    continue;
                }
                match perform_reconcile(card, &pr, merged_at_ms, airc, &room).await {
                    Ok(()) => eprintln!(
                        "airc-merger: reconciled already-merged card={} pr=#{} ({})",
                        card.card_id, pr.number, pr.repo
                    ),
                    Err(error) => eprintln!(
                        "airc-merger: reconcile failed card={} pr=#{}: {error}",
                        card.card_id, pr.number
                    ),
                }
            }
            MergeDecision::Skip(reason) => {
                eprintln!("airc-merger: skip card={} reason={reason}", card.card_id);
            }
        }
    }
    Ok(())
}

/// Card acd72c81 follow-up: emit `PullRequestMerged` for a card whose
/// PR is ALREADY merged on GitHub. Skips `gh pr merge` (the GH merge
/// already happened), runs the same kanban-event + worktree-cleanup
/// shape as `perform_merge` so reconciled cards reach the exact same
/// terminal state as freshly-merged ones.
async fn perform_reconcile(
    card: &WorkCard,
    pr: &airc_work::model::PullRequestRef,
    merged_at_ms: u64,
    airc: &Airc,
    room: &airc_lib::Room,
) -> Result<(), Box<dyn std::error::Error>> {
    // Card edf3670c: every "PR merged" terminal path routes
    // through `mark_merged_and_reclaim`. Eliminates the hand-paired
    // `mark_pull_request_merged + cleanup_card_worktree` duplication
    // and pins both wires at one site.
    crate::work_commands::mark_merged_and_reclaim(
        airc,
        room,
        card.card_id,
        pr.clone(),
        merged_at_ms,
    )
    .await
}

enum MergeDecision {
    Merge(airc_work::model::PullRequestRef),
    /// Card acd72c81 follow-up: PR is ALREADY merged on GitHub but the
    /// kanban projection hasn't observed it (no `PullRequestMerged`
    /// event for this card). The merger reconciles by emitting one
    /// — no `gh pr merge` call needed; the merge already happened.
    /// `merged_at_ms` comes from gh's `mergedAt` ISO-8601 timestamp
    /// when available, falls back to wall-clock-now if gh didn't
    /// supply it (older gh schema or transient).
    Reconcile(airc_work::model::PullRequestRef, u64),
    Skip(String),
}

/// Per-card eligibility check. Returns `Ok(None)` if the card isn't
/// even a candidate (not in Review, no PR linked); `Ok(Some(Merge))`
/// if everything passes; `Ok(Some(Skip(reason)))` if it's a candidate
/// that failed a gate (so we can log it instead of silently dropping).
async fn evaluate(
    gh: &dyn crate::gh_client::GhClient,
    card: &WorkCard,
    baselines: &mut BaselineCache,
    policy: GatePolicy,
) -> Result<Option<MergeDecision>, Box<dyn std::error::Error>> {
    use airc_work::model::CardState;
    if card.state != CardState::Review {
        return Ok(None);
    }
    let Some(pr) = card.pull_request.clone() else {
        return Ok(None);
    };

    match check_pr_gate(gh, &pr, baselines, policy).await {
        Ok(GateResult::Green) => Ok(Some(MergeDecision::Merge(pr))),
        Ok(GateResult::AlreadyMerged { merged_at_ms }) => {
            Ok(Some(MergeDecision::Reconcile(pr, merged_at_ms)))
        }
        Ok(GateResult::NotReady(reason)) => Ok(Some(MergeDecision::Skip(reason))),
        Err(error) => Ok(Some(MergeDecision::Skip(format!(
            "gh status query failed: {error}"
        )))),
    }
}

mod gate;
pub(crate) use gate::*;


/// Actually merge. `gh pr merge --squash --delete-branch` matches the
/// convention every recent PR has used (see merged-PR survey in card
/// 28f1440c). On success, publish `MarkPullRequestMerged` so the
/// projection transitions to `Merged`.
/// Card dec35ec7: typed `gh pr merge` via `GhClient`. `GhError`
/// surfaces conflicts as `PrNotMergeable` so an upcoming
/// less-red-than-base bypass can match the case and skip rather
/// than retry indefinitely. `MarkPullRequestMerged` still fires
/// after the merge succeeds — the projection contract is unchanged.
async fn perform_merge(
    gh: &dyn crate::gh_client::GhClient,
    card: &WorkCard,
    pr: &airc_work::model::PullRequestRef,
    airc: &Airc,
    room: &airc_lib::Room,
) -> Result<(), Box<dyn std::error::Error>> {
    gh.pr_merge(crate::gh_client::PrMergeArgs {
        repo: pr.repo.as_str().to_string(),
        number: pr.number,
    })
    .await?;

    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    // Card edf3670c (was card cdb477a2): same helper as
    // `perform_reconcile` so the daemon's two terminal paths and the
    // CLI's two terminal paths all agree. Before extraction, four
    // sites duplicated this pair and the CLI half forgot the cleanup
    // wire — recurring disk-full crash.
    crate::work_commands::mark_merged_and_reclaim(airc, room, card.card_id, pr.clone(), now_ms)
        .await
}

/// Acquire a non-blocking exclusive lock at `<home>/merger.lock`.
/// Returns the held file handle (drop = release). A second launch in
/// the same scope exits with a clear error instead of racing.
fn acquire_singleton_lock(home: &Path) -> Result<std::fs::File, Box<dyn std::error::Error>> {
    use fs2::FileExt;
    std::fs::create_dir_all(home)?;
    let path = home.join("merger.lock");
    let file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)?;
    file.try_lock_exclusive().map_err(|e| {
        format!(
            "another airc-merger is already running for {} ({}). \
             only one merger per scope at a time — kill the other or wait.",
            home.display(),
            e
        )
    })?;
    Ok(file)
}

#[cfg(test)]
mod tests;
