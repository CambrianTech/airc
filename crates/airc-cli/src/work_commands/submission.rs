//! `airc work submit` and `airc work submission-review`: publishing a card's content-addressed
//! work, and filing a reviewer's judgement of one submission.

use super::*;

/// `airc work submission-review <REVIEW_CARD> --outcome passed|failed --evidence-file F`:
/// a peer review on a submission, filed under THIS scope's identity (an agent reviewing a
/// citizen's work). The parent card and the claim come from the review card (it must be
/// yours, claimed); the submission defaults to the parent's latest. airc-lib's
/// `review_work_submission_in` had no verb, so no agent could file one from the CLI.
/// `airc work submit`: publish a submission on a card the caller holds. continuum #4825's
/// card went to Review with its PR linked, but no submission existed, so Kimi's verdict
/// was refused ("no submission to review yet", 2026-10-06): the CLI had no way to publish
/// one. airc-lib's `submit_work_in` validates the claim, the base and the instance.
pub async fn run_submit(
    home: &Path,
    room: Option<String>,
    card_id: String,
    patch: std::path::PathBuf,
    base: String,
    instance: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let bytes =
        std::fs::read(&patch).map_err(|e| format!("patch file {}: {e}", patch.display()))?;
    let base_sha = airc_work::GitObjectId::new(base.trim())
        .map_err(|e| format!("--base `{base}`: {e} (give the FULL commit sha)"))?;
    let airc = crate::commands::attached_airc(home).await?;
    let shown = Shown::try_from(card_id.as_str())?;
    let room = room_or_current(&airc, room.as_deref(), "submit work in").await?;
    let board = airc.work_board_in(&room).await?;
    let card_id = card_on_board(&board, &shown)?;
    submit_patch(&airc, home, &room, card_id, &bytes, base_sha, instance).await
}

/// Publish `bytes` as a submission on `card_id`, under the caller as its holder: the
/// one path behind `airc work submit` and the review transition's PR (BigMama,
/// 2026-10-06: a PR opened with no submission left the reviewer's verdict nothing to
/// attach to). Only the holder submits. The bytes are retained in the scope's blob store
/// BEFORE their hash is published (BigMama on #1548): a reviewer, grader or lift reads a
/// submission by hash, and the patch file can change after this call.
pub(crate) async fn submit_patch(
    airc: &airc_lib::Airc,
    home: &Path,
    room: &airc_lib::Room,
    card_id: airc_lib::WorkCardId,
    bytes: &[u8],
    base_sha: airc_work::GitObjectId,
    instance: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let board = airc.work_board_in(room).await?;
    let card = board
        .card(card_id)
        .ok_or_else(|| format!("card {card_id} is not on {}'s board", room.name))?;
    let store = airc_blobs::FsStore::new(airc_lib::machine_account_home(home).join("blobs"))
        .map_err(|e| format!("blob store: {e}"))?;
    let claim_id = match (card.owner, card.claim_id) {
        (Some(owner), Some(claim)) if owner == airc.peer_id() => claim,
        (Some(owner), _) => {
            return Err(format!(
                "card {card_id} is held by {owner}, not by you: only its holder submits"
            )
            .into())
        }
        (None, _) => {
            return Err(format!("card {card_id} is not claimed: `airc work claim` it first").into())
        }
    };
    let artifact = retain_patch(&store, bytes)?;
    let instance = instance.unwrap_or_else(|| card.repo.to_string());
    let submission = airc
        .submit_work_in(
            room,
            airc_lib::SubmitWork {
                submission_id: airc_work::SubmissionId::new(),
                card_id,
                claim_id,
                instance,
                base_sha,
                artifact,
            },
        )
        .await?;
    println!(
        "submission: {} card={} size={} base={}",
        submission.submission_id,
        submission.card_id,
        submission.artifact.size_bytes,
        submission.base_sha
    );
    Ok(())
}

/// Retain a patch in `store` and return its artifact reference: the hash the store holds
/// it under, its length, `text/x-patch`. An empty patch is refused: there is nothing to
/// review. The same shape as continuum's `work/submit` (`retain_patch`).
pub(super) fn retain_patch(
    store: &airc_blobs::FsStore,
    bytes: &[u8],
) -> Result<airc_work::SubmissionArtifact, String> {
    use airc_blobs::ContentAddressedStore;
    if bytes.is_empty() {
        return Err("the patch file is empty: there is nothing to review".to_string());
    }
    let hash = store
        .put(bytes)
        .map_err(|e| format!("could not retain the patch: {e}"))?;
    Ok(airc_work::SubmissionArtifact {
        hash,
        size_bytes: bytes.len() as u64,
        mime: Some("text/x-patch".to_string()),
    })
}

pub async fn run_submission_review(
    home: &Path,
    room: Option<String>,
    review_card_id: String,
    outcome: String,
    evidence_file: std::path::PathBuf,
    submission: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let outcome = parse_review_outcome(&outcome)?;
    let evidence_text = std::fs::read_to_string(&evidence_file)
        .map_err(|e| format!("evidence file {}: {e}", evidence_file.display()))?;
    let evidence = evidence_artifact(&evidence_text)?;
    let submission = submission.as_deref().map(Uuid::parse_str).transpose()?;
    let airc = crate::commands::attached_airc(home).await?;
    let review_shown = Shown::try_from(review_card_id.as_str())?;
    let room = room_or_current(&airc, room.as_deref(), "review a submission in").await?;
    let board = airc.work_board_in(&room).await?;
    let review_card = card_on_board(&board, &review_shown)?;
    let held = board
        .card(review_card)
        .ok_or_else(|| format!("review card {review_card} is not on {}'s board", room.name))?;
    let card_id = held
        .reviews
        .ok_or_else(|| format!("card {review_card} is not a review card (it reviews nothing)"))?;
    let review_claim_id = match (held.owner, held.claim_id) {
        (Some(owner), Some(claim)) if owner == airc.peer_id() => claim,
        (Some(owner), _) => {
            return Err(format!(
                "review card {review_card} is held by {owner}, not by you: only its holder reviews"
            )
            .into())
        }
        (None, _) => {
            return Err(format!(
                "review card {review_card} is not claimed: `airc work claim` it first"
            )
            .into())
        }
    };
    let chosen = board.candidate_for_review(
        review_card,
        submission.map(airc_work::SubmissionId::from_uuid),
    )?;
    let review = airc
        .review_work_submission_in(
            &room,
            airc_lib::ReviewWorkSubmission {
                evidence_text: Some(evidence_text),
                review_id: airc_work::WorkReviewId::from_uuid(Uuid::new_v4()),
                card_id,
                submission_id: chosen.submission_id,
                artifact: chosen.artifact.clone(),
                review_card_id: review_card,
                review_claim_id,
                outcome,
                evidence,
            },
        )
        .await?;
    println!(
        "review: {} card={} submission={} outcome={:?}",
        review.review_id, review.card_id, review.submission_id, review.outcome
    );
    Ok(())
}

/// PURE: the verdict word, by name. Unknown is not a verdict a reviewer files.
pub(super) fn parse_review_outcome(word: &str) -> Result<airc_work::WorkReviewOutcome, String> {
    match word.trim() {
        "passed" => Ok(airc_work::WorkReviewOutcome::Passed),
        "failed" => Ok(airc_work::WorkReviewOutcome::Failed),
        other => Err(format!(
            "--outcome must be `passed` or `failed`, got `{other}`"
        )),
    }
}

/// PURE: the evidence reference for what the reviewer wrote: SHA-256 over its bytes, its
/// length, `text/plain`. Empty evidence is refused: a verdict without evidence is not one.
pub(super) fn evidence_artifact(text: &str) -> Result<airc_work::SubmissionArtifact, String> {
    if text.trim().is_empty() {
        return Err(
            "the evidence file is empty: a verdict without evidence is not a verdict".to_string(),
        );
    }
    Ok(airc_work::SubmissionArtifact {
        hash: airc_blobs::ContentHash::from_bytes(text.as_bytes()),
        size_bytes: text.len() as u64,
        mime: Some("text/plain".to_string()),
    })
}
