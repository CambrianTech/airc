//! `airc work merge`: merge a Review-state card's PR through the merge gate, once.

use super::*;

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
    room: Option<String>,
    card_id: String,
    dry_run: bool,
    pending_timeout_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    use airc_work::model::CardState;

    let airc = crate::commands::attached_airc(home).await?;
    let card_shown = Shown::try_from(card_id.as_str())?;

    let room = room_or_current(&airc, room.as_deref(), "merge work in").await?;
    let board = airc.work_board_in(&room).await?;
    let card_uuid = card_on_board(&board, &card_shown)?;
    let card = board
        .card(card_uuid)
        .ok_or_else(|| format!("card {card_uuid} not visible in room {}", room.name))?;

    if card.state != CardState::Review {
        return Err(format!(
            "refusing to merge card {card_uuid}: state is {actual:?}, but `airc work merge` \
             requires Review (the card has been finished + PR opened + announced).\n\n{next}",
            actual = card.state,
            next = merge_refusal_next_step(card),
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
    let mut baselines = crate::merger::BaselineCache::default();

    // Card 7ed1ac4f: pending-too-long timeout. Default 30 min from
    // GatePolicy::default_for_merger; CLI flag override comes
    // through `pending_timeout_secs` arg (0 = strict-mode bypass).
    let policy = crate::merger::GatePolicy {
        pending_timeout_ms: pending_timeout_secs.saturating_mul(1000),
        now_ms: crate::merger::now_ms(),
    };

    match crate::merger::check_card_gate(gh.as_ref(), &board, card, &pr, &mut baselines, policy)
        .await
    {
        Ok(crate::merger::CardGate::Merge(head)) => {
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
                head: head.commit().clone(),
            })
            .await?;
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            mark_merged_and_reclaim(&airc, &room, card_uuid, pr.clone(), now_ms).await?;
            println!(
                "merged: card={card_uuid} pr=#{n} repo={r}",
                n = pr.number,
                r = pr.repo,
            );
            Ok(())
        }
        Ok(crate::merger::CardGate::AlreadyMerged { merged_at_ms }) => {
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
            mark_merged_and_reclaim(&airc, &room, card_uuid, pr.clone(), merged_at_ms).await?;
            println!(
                "reconciled: card={card_uuid} pr=#{n} repo={r} (PR was already merged on GitHub)",
                n = pr.number,
                r = pr.repo,
            );
            Ok(())
        }
        Ok(crate::merger::CardGate::NotReady(reason)) => Err(format!(
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

/// What a caller does next when `airc work merge` refuses a card that is not in
/// Review. A settled card has nothing left to merge; only an unfinished one is
/// told to open its PR (card b1633915: a Merged card was told to `state review`).
pub(super) fn merge_refusal_next_step(card: &WorkCard) -> String {
    let id = card.card_id;
    let pr = card
        .pull_request
        .as_ref()
        .map(|pr| format!(" (PR #{})", pr.number))
        .unwrap_or_default();
    match card.state {
        CardState::Merged => format!("Nothing to do: card {id} is already merged{pr}."),
        CardState::Closed => format!(
            "Nothing to merge: card {id} is closed{pr}. Reopen it with \
             `airc work state {id} open` if the work is live again."
        ),
        CardState::Open | CardState::Claimed | CardState::InProgress | CardState::Blocked => {
            format!(
                "Next step: `airc work state {id} review` to open + link the PR, then \
                 `airc work merge {id}` once CI is green."
            )
        }
        CardState::Review => format!("Card {id} is in Review; run `airc work merge {id}`."),
    }
}
