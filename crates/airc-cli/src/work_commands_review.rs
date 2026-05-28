//! Review card spawn — `airc work review <parent>` plus the
//! auto-spawn that fires on `state review`.
//!
//! Card c0bd865c phase 5 (a3ede6de). Both the manual CLI path
//! (ad7e100b Sub-B) and the auto-spawn (Sub-C) produce
//! structurally-identical review cards via a shared title format;
//! observers filtering `title.starts_with("review:")` pick up both.

use std::path::Path;

use airc_lib::CreateWorkCard;

use crate::work_cli::CliPriority;

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
    let parent_card_id = crate::work_commands::parse_work_card_id(&parent_id)?;
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
pub(crate) fn format_review_title(parent_title: &str) -> String {
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
