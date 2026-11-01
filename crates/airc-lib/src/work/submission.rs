//! A card's submissions and their reviews: publishing the content-addressed work a card offers,
//! and a reviewer's judgement of one submission.

use super::*;

/// Stable id makes retrying a timed-out publish safe. Author and time are
/// supplied by AIRC, never by the caller's manifest.
#[derive(Debug, Clone)]
pub struct SubmitWork {
    pub submission_id: airc_work::SubmissionId,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub instance: String,
    pub base_sha: airc_work::GitObjectId,
    /// The commit the patch ends at (`git diff base_sha..head`): what a verdict on it covers.
    pub head: airc_work::GitObjectId,
    pub artifact: airc_work::SubmissionArtifact,
}

/// Review an exact accepted candidate as the caller. The immutable review id
/// survives retries; the SDK owns reviewer identity and publication time.
#[derive(Debug, Clone)]
pub struct ReviewWorkSubmission {
    pub review_id: airc_work::WorkReviewId,
    pub card_id: WorkCardId,
    pub submission_id: airc_work::SubmissionId,
    pub artifact: airc_work::SubmissionArtifact,
    pub review_card_id: WorkCardId,
    pub review_claim_id: ClaimId,
    pub outcome: airc_work::WorkReviewOutcome,
    pub evidence: airc_work::SubmissionArtifact,
    pub evidence_text: Option<String>,
}

/// Canonical pagination size for complete work-board projections.
///
/// Card acd72c81: a complete projection read as ONE Inbox RPC asks the
/// daemon to serialize every transcript event in the room into a
/// single CBOR frame, which trips `airc_ipc::codec::MAX_FRAME_BYTES`
/// (16 MiB) once the room accumulates ~9000+ events. Empirically
/// caught on Joel's box 2026-06-01: `airc work state X closed` failed
/// with `daemon I/O: ipc frame too large: 16973574 bytes exceeds
/// 16777216`.
///
/// `work_board_complete(page_size)` paginates: each IPC frame stays
/// well under the cap while the projection is still complete.
/// 1024 events per page is a comfortable middle: each frame fits
/// (events are typed, not blob payloads — kilobytes each, not MB),
/// and the round-trip count stays low for the 9000+ event case
/// (~9 RPCs instead of one giant one).
pub const WORK_BOARD_PROJECTION_PAGE_SIZE: usize = 1024;

impl Airc {
    /// Materialize one peer-review handoff for an accepted candidate in its own
    /// room. Retries (including concurrent publishers) preserve reviewer custody.
    /// Policy deciding whether review is required belongs to the caller.
    pub async fn ensure_submission_review_in(
        &self,
        room: &Room,
        parent: WorkCardId,
        submission: airc_work::SubmissionId,
        mut request: CreateWorkCard,
    ) -> Result<WorkCardId, AircError> {
        let board = self.work_board_in(room).await?;
        let card_id = airc_work::submission_review_card_id(parent, submission);
        if let Some(recorded) = board.review_request(card_id) {
            // Returning accepted custody is a read; only materializing a new
            // request below requires the candidate publisher's signature.
            recorded.validate_for_board(&board)?;
            return Ok(card_id);
        }
        // a new handoff is a new action on both identities: neither may be conflicted
        super::refuse_conflicted(&board, parent, super::CardMutation::Act)?;
        super::refuse_conflicted(&board, card_id, super::CardMutation::Act)?;
        request.reviews = Some(parent);
        let handoff = airc_work::SubmissionReviewRequested {
            card: build_operator_card_created(card_id, self.peer_id(), now_ms()?, request),
            submission_id: submission,
        };
        handoff.validate_for_board(&board)?;
        self.publish_work_event_in(room, &WorkEvent::SubmissionReviewRequested(handoff))
            .await?;
        Ok(card_id)
    }

    /// Record judgement with evidence under the historical linked review claim.
    /// This does not infer a pass from card state, fetch evidence, or establish
    /// independent review: consumers compare the reviewer with the publisher.
    pub async fn review_work_submission_in(
        &self,
        room: &Room,
        request: ReviewWorkSubmission,
    ) -> Result<airc_work::WorkSubmissionReview, AircError> {
        let board = self.work_board_in(room).await?;
        let review = airc_work::WorkSubmissionReview {
            review_id: request.review_id,
            card_id: request.card_id,
            submission_id: request.submission_id,
            artifact: request.artifact,
            review_card_id: request.review_card_id,
            review_claim_id: request.review_claim_id,
            reviewer: self.peer_id(),
            outcome: request.outcome,
            evidence: request.evidence,
            evidence_text: request.evidence_text,
            reviewed_at_ms: now_ms()?,
        };
        review.validate_for_board(&board)?;
        if let Some(prior) = board.submission_review(review.review_id) {
            return Ok(prior.clone());
        }
        // a new judgement is a new action on the parent and on its review card
        super::refuse_conflicted(&board, review.card_id, super::CardMutation::Act)?;
        super::refuse_conflicted(&board, review.review_card_id, super::CardMutation::Act)?;
        self.publish_work_event_in(room, &WorkEvent::WorkSubmissionReviewed(review.clone()))
            .await?;
        Ok(review)
    }

    /// Publish an immutable artifact reference into the card's own room. This
    /// does not upload the blob or claim remote availability: consumers fetch
    /// by content hash and verify size/hash before using it.
    pub async fn submit_work_in(
        &self,
        room: &Room,
        request: SubmitWork,
    ) -> Result<airc_work::WorkSubmission, AircError> {
        let board = self.work_board_in(room).await?;
        let card = board
            .card(request.card_id)
            .ok_or(airc_work::ProjectionError::UnknownCard(request.card_id))?;
        super::refuse_conflicted(&board, request.card_id, super::CardMutation::Act)?;
        let mut submission = airc_work::WorkSubmission {
            submission_id: request.submission_id,
            card_id: request.card_id,
            claim_id: request.claim_id,
            instance: request.instance,
            base_sha: request.base_sha,
            head: airc_work::SubmissionHead::At(request.head),
            artifact: request.artifact,
            publisher: self.peer_id(),
            submitted_at_ms: now_ms()?,
        };
        if let Some(prior) = card
            .submissions
            .iter()
            .find(|s| s.submission_id == submission.submission_id)
        {
            submission.submitted_at_ms = prior.submitted_at_ms;
            submission.validate_for_card(card)?;
            return Ok(prior.clone());
        }
        submission.validate_for_card(card)?;
        self.publish_work_event_in(room, &WorkEvent::WorkSubmitted(submission.clone()))
            .await?;
        Ok(submission)
    }
}
