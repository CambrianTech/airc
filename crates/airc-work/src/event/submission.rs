//! A card's submissions and their reviews: the content-addressed work a card offers and each
//! reviewer's judgement of one submission.

use super::*;

/// A durable submission names content, not a path on its author's machine.
/// Publishing a reference does not assert that another node has fetched it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSubmission {
    pub submission_id: crate::ids::SubmissionId,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub instance: String,
    pub base_sha: GitObjectId,
    pub artifact: airc_blobs::MediaRef,
    pub publisher: PeerId,
    pub submitted_at_ms: u64,
}

/// A reviewer's judgement, not an assertion that the substrate graded the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkReviewOutcome {
    Passed,
    Failed,
    Unknown,
}

/// Immutable review of ONE accepted content-addressed submission. The linked
/// review card's historical claim attributes the work to its actual reviewer.
/// Consumers apply their activity's independence/acceptance policy; signing a
/// judgement proves its author, not its correctness or the evidence's availability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkSubmissionReview {
    pub review_id: crate::ids::WorkReviewId,
    pub card_id: WorkCardId,
    pub submission_id: crate::ids::SubmissionId,
    pub artifact: airc_blobs::MediaRef,
    pub review_card_id: WorkCardId,
    pub review_claim_id: ClaimId,
    pub reviewer: PeerId,
    pub outcome: WorkReviewOutcome,
    pub evidence: airc_blobs::MediaRef,
    /// Signed, bounded reviewer words; absent on legacy hash-only receipts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_text: Option<String>,
    /// Signed author's clock, like WorkSubmission::submitted_at_ms. This is
    /// checked against the historical claim; it is not trusted global time.
    pub reviewed_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum WorkReviewRejectionReason {
    #[error("review handoff is not requested by the accepted submission's publisher")]
    RequestorMismatch,
    #[error("reviewer differs from signed transcript author")]
    ReviewerMismatch,
    #[error("review refers to no accepted submission on the parent card")]
    UnknownSubmission,
    #[error("review artifact differs from the accepted submission")]
    ArtifactMismatch,
    #[error("review card does not review this parent in the same repository")]
    WrongReviewCard,
    #[error("review is not by the review card's current claim holder")]
    WrongClaim,
    #[error("review claim was expired when the review was published")]
    ExpiredClaim,
    #[error("review card is already settled")]
    SettledReviewCard,
    #[error("review evidence is empty, has an oversized MIME hint, or inline text exceeds 32 KiB or differs from its hash/size")]
    InvalidEvidence,
    #[error("review id was already used for different immutable content")]
    ConflictingId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedWorkReview {
    pub review_id: crate::ids::WorkReviewId,
    pub card_id: WorkCardId,
    pub reviewer: PeerId,
    pub reviewed_at_ms: u64,
    pub reason: WorkReviewRejectionReason,
}

impl WorkSubmissionReview {
    pub fn same_judgement(&self, other: &Self) -> bool {
        self.review_id == other.review_id
            && self.card_id == other.card_id
            && self.submission_id == other.submission_id
            && self.artifact == other.artifact
            && self.review_card_id == other.review_card_id
            && self.review_claim_id == other.review_claim_id
            && self.reviewer == other.reviewer
            && self.outcome == other.outcome
            && self.evidence == other.evidence
            && self.evidence_text == other.evidence_text
    }

    pub fn validate(&self) -> Result<(), WorkReviewRejectionReason> {
        if self.evidence.size_bytes == 0
            || self
                .evidence
                .mime
                .as_ref()
                .is_some_and(|mime| mime.len() > 128)
        {
            return Err(WorkReviewRejectionReason::InvalidEvidence);
        }
        if let Some(text) = &self.evidence_text {
            if text.trim().is_empty()
                || text.len() > 32 * 1024
                || text.len() as u64 != self.evidence.size_bytes
                || airc_blobs::ContentHash::from_bytes(text.as_bytes()) != self.evidence.hash
            {
                return Err(WorkReviewRejectionReason::InvalidEvidence);
            }
        }
        Ok(())
    }

    /// Called while replaying at this event's position. A later claim release,
    /// reassignment or card closure cannot retroactively invalidate an accepted
    /// judgement, and an idempotent retry retains its first publication time.
    pub fn validate_for_board(
        &self,
        board: &crate::WorkBoardProjection,
    ) -> Result<(), WorkReviewRejectionReason> {
        use WorkReviewRejectionReason as Reason;
        if let Some(prior) = board.submission_review(self.review_id) {
            return if prior.same_judgement(self) {
                Ok(())
            } else {
                Err(Reason::ConflictingId)
            };
        }
        self.validate()?;
        let parent = board.card(self.card_id).ok_or(Reason::UnknownSubmission)?;
        let submitted = parent
            .submissions
            .iter()
            .find(|candidate| candidate.submission_id == self.submission_id)
            .ok_or(Reason::UnknownSubmission)?;
        if self.artifact != submitted.artifact {
            return Err(Reason::ArtifactMismatch);
        }
        let review_card = board
            .card(self.review_card_id)
            .ok_or(Reason::WrongReviewCard)?;
        if review_card.reviews != Some(self.card_id) || review_card.repo != parent.repo {
            return Err(Reason::WrongReviewCard);
        }
        if board
            .review_request(self.review_card_id)
            .is_some_and(|r| r.submission_id != self.submission_id)
        {
            return Err(Reason::WrongReviewCard);
        }
        if review_card.owner != Some(self.reviewer)
            || review_card.claim_id != Some(self.review_claim_id)
        {
            return Err(Reason::WrongClaim);
        }
        if review_card.state.is_settled() {
            return Err(Reason::SettledReviewCard);
        }
        if review_card
            .claim_expires_at_ms
            .is_none_or(|expiry| self.reviewed_at_ms >= expiry)
        {
            return Err(Reason::ExpiredClaim);
        }
        Ok(())
    }

    pub fn rejected(&self, reason: WorkReviewRejectionReason) -> RejectedWorkReview {
        RejectedWorkReview {
            review_id: self.review_id,
            card_id: self.card_id,
            reviewer: self.reviewer,
            reviewed_at_ms: self.reviewed_at_ms,
            reason,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum SubmissionRejectionReason {
    #[error("submission publisher differs from transcript author")]
    PublisherMismatch,
    #[error("submission base must be a full SHA-1 or SHA-256 commit id")]
    InvalidBase,
    #[error("submission instance must contain 1..=512 UTF-8 bytes")]
    InvalidInstance,
    #[error("artifact MIME hint exceeds 128 UTF-8 bytes")]
    InvalidArtifact,
    #[error("submission does not belong to the current claim holder")]
    WrongClaim,
    #[error("claim was expired when the submission was published")]
    ExpiredClaim,
    #[error("card is already settled")]
    SettledCard,
    #[error("submission id was already used for different immutable content")]
    ConflictingId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedSubmission {
    pub submission_id: crate::ids::SubmissionId,
    pub card_id: WorkCardId,
    pub publisher: PeerId,
    pub submitted_at_ms: u64,
    pub reason: SubmissionRejectionReason,
}

impl WorkSubmission {
    /// Retries retain the first accepted publication time, even when a stale
    /// board read causes the publisher to stamp the retry with a later time.
    pub fn same_candidate(&self, other: &Self) -> bool {
        self.submission_id == other.submission_id
            && self.card_id == other.card_id
            && self.claim_id == other.claim_id
            && self.instance == other.instance
            && self.base_sha == other.base_sha
            && self.artifact == other.artifact
            && self.publisher == other.publisher
    }

    /// Admission is also replayed against historical claim state, never today's clock.
    pub fn validate_for_card(
        &self,
        card: &crate::model::WorkCard,
    ) -> Result<(), SubmissionRejectionReason> {
        use SubmissionRejectionReason as Reason;
        if let Some(prior) = card
            .submissions
            .iter()
            .find(|s| s.submission_id == self.submission_id)
        {
            return if prior.same_candidate(self) {
                Ok(())
            } else {
                Err(Reason::ConflictingId)
            };
        }
        self.validate()?;
        if card.card_id != self.card_id
            || card.owner != Some(self.publisher)
            || card.claim_id != Some(self.claim_id)
        {
            return Err(Reason::WrongClaim);
        }
        // Review settles scheduling, not immutable submission history: the current
        // holder must still be able to publish a corrected candidate for review.
        if matches!(card.state, CardState::Merged | CardState::Closed) {
            return Err(Reason::SettledCard);
        }
        if card
            .claim_expires_at_ms
            .is_none_or(|expiry| self.submitted_at_ms >= expiry)
        {
            return Err(Reason::ExpiredClaim);
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), SubmissionRejectionReason> {
        let base = self.base_sha.as_str();
        if !matches!(base.len(), 40 | 64) || !base.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(SubmissionRejectionReason::InvalidBase);
        }
        if self.instance.trim().is_empty() || self.instance.len() > 512 {
            return Err(SubmissionRejectionReason::InvalidInstance);
        }
        if self.artifact.mime.as_ref().is_some_and(|m| m.len() > 128) {
            return Err(SubmissionRejectionReason::InvalidArtifact);
        }
        Ok(())
    }

    pub fn rejected(&self, reason: SubmissionRejectionReason) -> RejectedSubmission {
        RejectedSubmission {
            submission_id: self.submission_id,
            card_id: self.card_id,
            publisher: self.publisher,
            submitted_at_ms: self.submitted_at_ms,
            reason,
        }
    }
}
