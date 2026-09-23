//! Append-only work-domain events.

use serde::{Deserialize, Serialize};

use airc_core::PeerId;

use crate::goal_event::{
    CardOrigin, GoalAbandoned, GoalAchieved, GoalCreated, GoalDryTickRecorded,
};
use crate::ids::{ClaimId, LaneId, RepoId, WorkCardId, WorkspaceId};
use crate::model::{
    AgentAvailabilityState, BranchName, CardState, DirtyState, DrainCandidate, DrainOutcome,
    GitObjectId, HygieneReport, LaneState, PrCheckState, PrMergeState, PrReviewState,
    PressureLevel, Priority, PullRequestRef,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkEvent {
    CardCreated(CardCreated),
    CardUpdated(CardUpdated),
    CardClaimed(WorkCardClaimed),
    ClaimHeartbeat(ClaimHeartbeat),
    ClaimReleased(ClaimReleased),
    CardStateChanged(CardStateChanged),
    WorkSubmitted(WorkSubmission),
    WorkSubmissionReviewed(WorkSubmissionReview),
    /// Derived during authenticated transcript replay, never accepted from wire.
    #[serde(skip)]
    SubmissionRejected(RejectedSubmission),
    /// Derived during authenticated replay; callers cannot publish a rejection.
    #[serde(skip)]
    ReviewRejected(RejectedWorkReview),
    LaneCreated(LaneCreated),
    LaneStateChanged(LaneStateChanged),
    WorkspaceRequested(WorkspaceRequested),
    WorkspaceAllocated(WorkspaceAllocated),
    WorkspaceHeartbeat(WorkspaceHeartbeat),
    WorkspaceReleased(WorkspaceReleased),
    WorkspacePressureReported(WorkspacePressureReported),
    WorkspaceDrainRequested(WorkspaceDrainRequested),
    WorkspaceDrainCompleted(WorkspaceDrainCompleted),
    GitCommitObserved(GitCommitObserved),
    GitBranchMoved(GitBranchMoved),
    GitDirtyStateChanged(GitDirtyStateChanged),
    PullRequestCheckSuiteChanged(PullRequestCheckSuiteChanged),
    PullRequestReviewSubmitted(PullRequestReviewSubmitted),
    PullRequestMergeStateChanged(PullRequestMergeStateChanged),
    PullRequestLinked(PullRequestLinked),
    PullRequestRelinked(PullRequestRelinked),
    PullRequestMerged(PullRequestMerged),
    HygieneReportRecorded(HygieneReportRecorded),
    ManagerHatClaimed(ManagerHatClaimed),
    ManagerHatReleased(ManagerHatReleased),
    AgentAvailabilityReported(AgentAvailabilityReported),
    // Card e4cad280 slice C2a: idle-agent engine goal-lifecycle events.
    // Goal creation, operator-path achievement, and abandonment ride
    // the WorkEvent union so the codec, replay, and subscription paths
    // all carry them without per-event-type plumbing. Per the verdict
    // residual 4 on PR #1123 ('every replayer re-derives the condition;
    // if each emits, the log gets N copies'), the auto-projection path
    // for ExitCondition::{DryForTicks/MilestoneClosed/AllCardsClosed}
    // is PURELY DERIVED state — no event on the wire. GoalAchieved
    // exists only for the operator path. GoalDryTickRecorded is the
    // synthesizer's typed dry-tick witness (v2 residual 1 fix).
    GoalCreated(GoalCreated),
    GoalAchieved(GoalAchieved),
    GoalAbandoned(GoalAbandoned),
    GoalDryTickRecorded(GoalDryTickRecorded),
}

impl WorkEvent {
    pub fn occurred_at_ms(&self) -> u64 {
        match self {
            WorkEvent::CardCreated(e) => e.created_at_ms,
            WorkEvent::CardUpdated(e) => e.updated_at_ms,
            WorkEvent::CardClaimed(e) => e.claimed_at_ms,
            WorkEvent::ClaimHeartbeat(e) => e.heartbeat_at_ms,
            WorkEvent::ClaimReleased(e) => e.released_at_ms,
            WorkEvent::CardStateChanged(e) => e.changed_at_ms,
            WorkEvent::WorkSubmitted(e) => e.submitted_at_ms,
            WorkEvent::WorkSubmissionReviewed(e) => e.reviewed_at_ms,
            WorkEvent::SubmissionRejected(e) => e.submitted_at_ms,
            WorkEvent::ReviewRejected(e) => e.reviewed_at_ms,
            WorkEvent::LaneCreated(e) => e.created_at_ms,
            WorkEvent::LaneStateChanged(e) => e.changed_at_ms,
            WorkEvent::WorkspaceRequested(e) => e.requested_at_ms,
            WorkEvent::WorkspaceAllocated(e) => e.allocated_at_ms,
            WorkEvent::WorkspaceHeartbeat(e) => e.heartbeat_at_ms,
            WorkEvent::WorkspaceReleased(e) => e.released_at_ms,
            WorkEvent::WorkspacePressureReported(e) => e.reported_at_ms,
            WorkEvent::WorkspaceDrainRequested(e) => e.requested_at_ms,
            WorkEvent::WorkspaceDrainCompleted(e) => e.completed_at_ms,
            WorkEvent::GitCommitObserved(e) => e.observed_at_ms,
            WorkEvent::GitBranchMoved(e) => e.moved_at_ms,
            WorkEvent::GitDirtyStateChanged(e) => e.changed_at_ms,
            WorkEvent::PullRequestCheckSuiteChanged(e) => e.changed_at_ms,
            WorkEvent::PullRequestReviewSubmitted(e) => e.submitted_at_ms,
            WorkEvent::PullRequestMergeStateChanged(e) => e.changed_at_ms,
            WorkEvent::PullRequestLinked(e) => e.linked_at_ms,
            WorkEvent::PullRequestRelinked(e) => e.relinked_at_ms,
            WorkEvent::PullRequestMerged(e) => e.merged_at_ms,
            WorkEvent::HygieneReportRecorded(e) => e.report.recorded_at_ms,
            WorkEvent::ManagerHatClaimed(e) => e.claimed_at_ms,
            WorkEvent::ManagerHatReleased(e) => e.released_at_ms,
            WorkEvent::AgentAvailabilityReported(e) => e.reported_at_ms,
            WorkEvent::GoalCreated(e) => e.created_at_ms,
            WorkEvent::GoalAchieved(e) => e.achieved_at_ms,
            WorkEvent::GoalAbandoned(e) => e.abandoned_at_ms,
            WorkEvent::GoalDryTickRecorded(e) => e.recorded_at_ms,
        }
    }
}

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
    /// Signed author's clock, like WorkSubmission::submitted_at_ms. This is
    /// checked against the historical claim; it is not trusted global time.
    pub reviewed_at_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[serde(rename_all = "snake_case")]
pub enum WorkReviewRejectionReason {
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
    #[error("review evidence is empty or has an oversized MIME hint")]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardCreated {
    pub card_id: WorkCardId,
    pub repo: RepoId,
    pub title: String,
    pub body: Option<String>,
    pub priority: Priority,
    pub lane_id: Option<LaneId>,
    pub created_by: PeerId,
    pub created_at_ms: u64,
    /// If this card is a sibling review of another card, the
    /// reviewed card's id. Card ad7e100b (peer-agent review loop)
    /// Sub-A: makes "this card is a review of X" a typed link
    /// rather than a body-string convention, so observers /
    /// scheduling logic can ask the projection
    /// (`WorkBoardProjection::review_cards_for(parent_id)`) which
    /// reviews exist for a card.
    ///
    /// Optional: regular cards omit it. Serde-back-compat: legacy
    /// `CardCreated` events on the wire decode with `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviews: Option<WorkCardId>,
    /// Typed provenance per v2 A1 (PR #1123 verdict). Card e4cad280
    /// slice C2a: the `Synthesized` variant carries dedup_key, goal_id,
    /// recipe_id, and synthesizer_peer so the projection (C2b) can
    /// arbitrate first-write-wins and attribute the audit trail without
    /// joining a side-channel event. Per
    /// `[[strong-typing-across-boundaries]]` and the positron #1602
    /// "first-class but never anonymous" precedent: provenance rides
    /// the primary object.
    ///
    /// Optional with `#[serde(default)]` so legacy `CardCreated` events
    /// (every card filed before C2a lands) decode with `None`. The
    /// projection treats `None` as the `Operator { peer_id: created_by }`
    /// origin per the legacy-event interpretation rule (no `Synthesized`
    /// origin can be inferred without explicit metadata — silent
    /// inference would violate `[[no-fallbacks-ever]]`). Same back-compat
    /// shape as the `reviews` field on this struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<CardOrigin>,
}

/// Amend a card's editable fields after creation. Card 5ac0a359 —
/// recurring friction during the autonomous flywheel: cards are
/// often filed with provisional bodies that need updating once
/// decomposition or scope refinement happens, and the substrate
/// previously had no typed way to do that without close+re-create
/// (which loses the card id, breaks `reviews` links pointing at the
/// old id, and forces every observer to re-project).
///
/// Each updatable field is `Option`; `None` means "leave the
/// projection's current value alone." An event with every field
/// `None` is legal and projects as a no-op (updating only
/// `updated_at_ms`) — convenient for liveness markers / "I touched
/// this card" without changing semantics.
///
/// Body semantics: setting an empty string is the canonical "clear
/// the body" path — the projection records exactly what the
/// amendment specifies. A true tri-state ("leave" vs "clear to
/// None" vs "set to s") would require a custom serde shape and
/// gives back only the ability to distinguish `Some("")` from
/// `None` on the projection's body field, which no observer
/// actually depends on (board renderers treat both as "no body").
/// If that distinction ever matters, swap this field for a tagged
/// enum (`BodyAmendment::Leave | Clear | Set(String)`) — the wire
/// shape can change behind serde's `untagged` discipline.
///
/// Fields that are deliberately NOT updatable post-creation:
///   * `card_id` — identity (would defeat the whole point of cards
///     being stable references).
///   * `repo` — cards live where the work lives; cross-repo
///     migration is its own card type.
///   * `created_by`, `created_at_ms` — attribution / temporal
///     anchors, append-only.
///   * `reviews` — typed sibling link set at creation; rebinding
///     a review to a different parent is structurally a different
///     review card.
///   * `lane_id` — lane membership is a separate event (lane
///     reassignment is its own concern).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardUpdated {
    /// Explicit choice of an already-held claim, without changing its lease.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_selection: Option<ClaimSelection>,
    pub card_id: WorkCardId,
    /// New title, if changing. `None` leaves the existing title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// New body, if changing. `None` leaves the existing body
    /// alone; `Some(s)` sets it (empty string clears in practice).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// New priority, if changing. `None` leaves the existing
    /// priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<Priority>,
    /// Peer that emitted the amendment. Audit / attribution.
    pub updated_by: PeerId,
    pub updated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimSelection {
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub selected_at_ms: u64,
}

/// Why the owner took this lease. Old events remain unknown, never inferred
/// from the owner's identity or the card's title.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClaimOrigin {
    Explicit,
    Automatic,
    #[default]
    #[serde(other)]
    Unknown,
}

impl ClaimOrigin {
    pub fn is_unknown(&self) -> bool {
        matches!(self, Self::Unknown)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkCardClaimed {
    /// Original decision time when recovering the same owner's lapsed choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_at_ms: Option<u64>,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub ttl_ms: u64,
    pub claimed_at_ms: u64,
    #[serde(default, skip_serializing_if = "ClaimOrigin::is_unknown")]
    pub origin: ClaimOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimHeartbeat {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub ttl_ms: u64,
    pub heartbeat_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimReleased {
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub reason: Option<String>,
    pub released_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardStateChanged {
    pub card_id: WorkCardId,
    pub state: CardState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneCreated {
    pub lane_id: LaneId,
    pub repo: RepoId,
    pub title: String,
    pub state: LaneState,
    pub created_by: PeerId,
    pub created_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaneStateChanged {
    pub lane_id: LaneId,
    pub state: LaneState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRequested {
    pub workspace_id: WorkspaceId,
    pub card_id: WorkCardId,
    pub claim_id: ClaimId,
    pub owner: PeerId,
    pub repo: RepoId,
    pub branch: BranchName,
    pub base: BranchName,
    pub requested_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceAllocated {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub allocated_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceHeartbeat {
    pub workspace_id: WorkspaceId,
    pub disk_bytes: Option<u64>,
    pub heartbeat_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceReleased {
    pub workspace_id: WorkspaceId,
    pub released_at_ms: u64,
}

/// Disk pressure observation for a workspace. Telemetry event; the
/// emitter is whichever peer has visibility into the workspace's disk
/// state. Workspace-id keyed and intentionally independent of the
/// card/claim lease flow — pressure can be observed and reported on
/// any known `WorkspaceId`, leased or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspacePressureReported {
    pub workspace_id: WorkspaceId,
    pub repo: RepoId,
    pub reporter: PeerId,
    pub total_bytes: u64,
    pub available_bytes: u64,
    pub level: PressureLevel,
    pub reported_at_ms: u64,
}

/// Drain request. Captures the candidate list at decision time so the
/// completion outcome can be compared against intent in record/replay.
/// Workspace-id keyed; no card/claim coupling required.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDrainRequested {
    pub workspace_id: WorkspaceId,
    pub repo: RepoId,
    pub requester: PeerId,
    /// Stable identifier of the policy rule that emitted this request
    /// (e.g. `"default.rebuildable"`, `"user.aggressive"`). Lets the
    /// runtime correlate outcomes to rules over time.
    pub policy_rule_id: String,
    /// True = inspection only, no paths are touched. Completion must
    /// echo `dry_run = true` and `paths_touched` must be empty.
    pub dry_run: bool,
    pub candidates: Vec<DrainCandidate>,
    pub requested_at_ms: u64,
}

/// Drain completion. Honest about partial outcomes — see [`DrainOutcome`]
/// for the bytes/paths/errors fields. `performer` records which peer
/// actually executed the cleanup so audit can attribute reclaim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceDrainCompleted {
    pub workspace_id: WorkspaceId,
    pub repo: RepoId,
    pub performer: PeerId,
    pub policy_rule_id: String,
    pub dry_run: bool,
    pub outcome: DrainOutcome,
    pub completed_at_ms: u64,
}

/// A commit was observed by a local git adapter, CI adapter, or
/// external forge adapter. This is an observation event, not a command
/// to mutate git.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitObserved {
    pub repo: RepoId,
    pub commit: GitObjectId,
    pub branch: Option<BranchName>,
    pub summary: Option<String>,
    pub observed_by: PeerId,
    pub observed_at_ms: u64,
}

/// A branch head moved. Consumers can subscribe to this instead of
/// polling `git fetch && git rev-parse` in every runtime.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitBranchMoved {
    pub repo: RepoId,
    pub branch: BranchName,
    pub old_head: Option<GitObjectId>,
    pub new_head: GitObjectId,
    pub moved_by: PeerId,
    pub moved_at_ms: u64,
}

/// Worktree dirty state changed. This is intentionally small and
/// closed: detailed path inventories belong in a follow-up inventory
/// event, while this event is what monitors need to wake up.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDirtyStateChanged {
    pub repo: RepoId,
    pub workspace_id: Option<WorkspaceId>,
    pub path: String,
    pub state: DirtyState,
    pub dirty_paths: u64,
    pub untracked_paths: u64,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestCheckSuiteChanged {
    pub pull_request: PullRequestRef,
    pub state: PrCheckState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestReviewSubmitted {
    pub pull_request: PullRequestRef,
    pub reviewer: PeerId,
    pub state: PrReviewState,
    pub submitted_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestMergeStateChanged {
    pub pull_request: PullRequestRef,
    pub state: PrMergeState,
    pub changed_by: PeerId,
    pub changed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestLinked {
    pub card_id: WorkCardId,
    pub pull_request: PullRequestRef,
    pub linked_by: PeerId,
    pub linked_at_ms: u64,
}

/// Card 09fddedd: re-point a card's linked PR at a successor PR.
///
/// `PullRequestLinked` is effectively first-write-wins at the CLI
/// layer (`airc work link` / `state review` both no-op on an
/// already-linked card), so a card whose round-1 PR was closed or
/// merged without the real fix (orphaned stacked PRs, recovered
/// lanes — failure class: card 6967921d) could never be pointed at
/// the round-2 PR carrying the actual work. The merger then skips
/// the card forever and the successor PR needs a manual merge.
///
/// This event is the intentional-supersede path: it records BOTH
/// links — `old_pull_request` for auditability (which PR the card
/// abandoned, and why the transcript shows two), `new_pull_request`
/// as the new single source of truth the projection adopts. It is
/// deliberately a distinct variant rather than a second
/// `PullRequestLinked` so replayers can tell a supersede from a
/// duplicate link without diffing projection state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestRelinked {
    pub card_id: WorkCardId,
    /// The link being superseded, exactly as the projection held it
    /// when the relink was emitted. Audit trail — the projection
    /// drops it, the transcript keeps it.
    pub old_pull_request: PullRequestRef,
    /// The successor PR the card now tracks.
    pub new_pull_request: PullRequestRef,
    pub relinked_by: PeerId,
    pub relinked_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PullRequestMerged {
    pub card_id: WorkCardId,
    pub pull_request: PullRequestRef,
    pub merged_by: PeerId,
    pub merged_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HygieneReportRecorded {
    pub report: HygieneReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerHatClaimed {
    pub repo: RepoId,
    pub manager: PeerId,
    pub ttl_ms: u64,
    pub claimed_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagerHatReleased {
    pub repo: RepoId,
    pub manager: PeerId,
    pub released_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAvailabilityReported {
    pub repo: RepoId,
    pub peer: PeerId,
    pub state: AgentAvailabilityState,
    pub note: Option<String>,
    pub ttl_ms: u64,
    pub reported_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn review_accepts_corrected_submission_without_relaxing_claim_or_terminal_guards() {
        let card_id = WorkCardId::from_u128(1);
        let owner = PeerId::from_u128(2);
        let claim_id = ClaimId::from_u128(3);
        let mut board = crate::WorkBoardProjection::new();
        board
            .apply(&WorkEvent::CardCreated(CardCreated {
                card_id,
                repo: RepoId::new("CambrianTech/airc").unwrap(),
                title: "correct a submitted patch".into(),
                body: None,
                priority: Priority::P1,
                lane_id: None,
                created_by: owner,
                created_at_ms: 10,
                reviews: None,
                origin: None,
            }))
            .unwrap();
        board
            .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
                card_id,
                claim_id,
                owner,
                ttl_ms: 1000,
                claimed_at_ms: 20,
                origin: ClaimOrigin::Automatic,
                selected_at_ms: None,
            }))
            .unwrap();
        let original = WorkSubmission {
            submission_id: crate::SubmissionId::from_u128(4),
            card_id,
            claim_id,
            instance: "card".into(),
            base_sha: GitObjectId::new("a".repeat(40)).unwrap(),
            artifact: airc_blobs::MediaRef {
                hash: airc_blobs::ContentHash::from_bytes(b"corrected submission fixture"),
                size_bytes: 100,
                mime: Some("text/x-patch".into()),
            },
            publisher: owner,
            submitted_at_ms: 30,
        };
        board
            .apply(&WorkEvent::WorkSubmitted(original.clone()))
            .unwrap();
        board
            .apply(&WorkEvent::CardStateChanged(CardStateChanged {
                card_id,
                state: CardState::Review,
                changed_by: owner,
                changed_at_ms: 40,
            }))
            .unwrap();
        let corrected = WorkSubmission {
            submission_id: crate::SubmissionId::from_u128(5),
            base_sha: GitObjectId::new("c".repeat(40)).unwrap(),
            submitted_at_ms: 50,
            ..original.clone()
        };
        board
            .apply(&WorkEvent::WorkSubmitted(corrected.clone()))
            .unwrap();
        let card = board.card(card_id).unwrap();
        assert_eq!(card.submissions.len(), 2);
        assert_eq!(card.submissions[0].submission_id, corrected.submission_id);
        assert_eq!(card.state, CardState::Review);
        assert_eq!(card.owner, Some(owner));
        assert_eq!(card.claim_id, Some(claim_id));

        let fresh = WorkSubmission {
            submission_id: crate::SubmissionId::from_u128(6),
            ..corrected.clone()
        };
        for state in [CardState::Merged, CardState::Closed] {
            let mut terminal = card.clone();
            terminal.state = state;
            assert_eq!(
                fresh.validate_for_card(&terminal),
                Err(SubmissionRejectionReason::SettledCard)
            );
        }
        let expired = WorkSubmission {
            submitted_at_ms: 1020,
            ..fresh.clone()
        };
        assert_eq!(
            expired.validate_for_card(card),
            Err(SubmissionRejectionReason::ExpiredClaim)
        );
        let foreign = WorkSubmission {
            publisher: PeerId::from_u128(9),
            ..fresh.clone()
        };
        assert_eq!(
            foreign.validate_for_card(card),
            Err(SubmissionRejectionReason::WrongClaim)
        );
        let stale = WorkSubmission {
            claim_id: ClaimId::from_u128(9),
            ..fresh
        };
        assert_eq!(
            stale.validate_for_card(card),
            Err(SubmissionRejectionReason::WrongClaim)
        );
        let conflict = WorkSubmission {
            submission_id: original.submission_id,
            ..corrected
        };
        assert_eq!(
            conflict.validate_for_card(card),
            Err(SubmissionRejectionReason::ConflictingId)
        );
    }

    #[test]
    fn work_event_serializes_with_kind_tag() {
        let event = WorkEvent::CardCreated(CardCreated {
            card_id: WorkCardId::from_u128(1),
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: "build work domain".to_string(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: PeerId::from_u128(2),
            created_at_ms: 10,
            reviews: None,
            origin: None,
        });
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["kind"], "card_created");
        assert_eq!(event.occurred_at_ms(), 10);
    }
}
