//! The verdict half of the merge gate (card 950b3093): a PR merges only on a PASSED, independent
//! review of the card's latest submission, and only when that submission ends at the PR's head.
//!
//! Until this, the gate read Review state, a linked PR and CI only, so a green Review card merged
//! with no verdict at all (llama.cpp#95/#96, 2026-11-07). A verdict covers the submission it read,
//! and a submission names the commit its patch ends at (`SubmissionHead`), so the gate can say
//! whether the head GitHub would land is the head a reviewer passed.
//!
//! Independence is required always. Whether a citizen's own review earns her LEARNING credit is a
//! room policy of continuum's (its recipe binding); landing code is not credit, so the merge gate
//! never admits a self-review.
//!
//! The remaining gap, named: the merger still wakes on its 30 s tick over the whole board rather
//! than on the verdict and CI events themselves (card 7b35d0f4: a webhook -> `CiCompleted` relay).

use airc_work::{
    GitObjectId, WorkBoardProjection, WorkCard, WorkReviewOutcome, WorkSubmission,
    WorkSubmissionReview,
};

/// Why a card's verdict does not authorize a merge.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VerdictGap {
    NoSubmission,
    HeadUnrecorded,
    PrHeadUnknown,
    HeadMoved {
        submitted: GitObjectId,
        pr: GitObjectId,
    },
    NoIndependentReview,
    LatestReview(WorkReviewOutcome),
}

impl std::fmt::Display for VerdictGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSubmission => f.write_str("no submission on the card: `airc work submit` it (or move it to Review from its worktree) so a reviewer can judge it"),
            Self::HeadUnrecorded => f.write_str("the latest submission predates recorded heads: resubmit at the PR head"),
            Self::PrHeadUnknown => f.write_str("GitHub did not report the PR's head commit, so no verdict can be shown to cover it"),
            Self::HeadMoved { submitted, pr } => write!(f, "verdict at {submitted}, head {pr}: the PR moved past its submission; submit the new head for review"),
            Self::NoIndependentReview => f.write_str("no review by anyone but its author on the latest submission"),
            Self::LatestReview(outcome) => write!(f, "the latest independent review is {outcome:?}, not Passed"),
        }
    }
}

/// `Ok` when `card`'s latest submission ends at `pr_head` and its latest independent review on the
/// board passed.
pub(crate) fn verdict_authorizes(
    board: &WorkBoardProjection,
    card: &WorkCard,
    pr_head: Option<&GitObjectId>,
) -> Result<(), VerdictGap> {
    let submission = card.submissions.last().ok_or(VerdictGap::NoSubmission)?;
    decide(
        submission,
        board.submission_reviews_for(submission.submission_id),
        pr_head,
    )
}

/// The decision over one submission and the reviews filed on it: pure, so every gap is tested
/// on plain values.
fn decide<'a>(
    submission: &WorkSubmission,
    reviews: impl Iterator<Item = &'a WorkSubmissionReview>,
    pr_head: Option<&GitObjectId>,
) -> Result<(), VerdictGap> {
    let submitted = submission.head.commit().ok_or(VerdictGap::HeadUnrecorded)?;
    let pr_head = pr_head.ok_or(VerdictGap::PrHeadUnknown)?;
    if submitted != pr_head {
        return Err(VerdictGap::HeadMoved {
            submitted: submitted.clone(),
            pr: pr_head.clone(),
        });
    }
    let latest = reviews
        .filter(|review| {
            review.submission_id == submission.submission_id
                && review.reviewer != submission.publisher
        })
        .max_by_key(|review| review.reviewed_at_ms)
        .ok_or(VerdictGap::NoIndependentReview)?;
    match latest.outcome {
        WorkReviewOutcome::Passed => Ok(()),
        other => Err(VerdictGap::LatestReview(other)),
    }
}

/// The head a verdict was judged at. Its field is private and this module alone mints it, from the
/// view the gate read, so a merge can only pin a head the gate judged (card 950b3093).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedHead(GitObjectId);

impl VerifiedHead {
    pub(crate) fn commit(&self) -> &GitObjectId {
        &self.0
    }
}

/// The whole gate's answer for one card.
#[derive(Debug)]
pub(crate) enum CardGate {
    /// Merge, pinned to the head the verdict and the CI were judged at.
    Merge(VerifiedHead),
    /// Already merged on GitHub: the board reconciles, whatever its verdict.
    AlreadyMerged {
        merged_at_ms: u64,
    },
    NotReady(String),
}

/// The whole gate over one card: its verdict at the PR's head, then CI no redder than the base,
/// both read from ONE fetched view, so the head checked is the head judged and the head merged.
pub(crate) async fn check_card_gate(
    gh: &dyn crate::gh_client::GhClient,
    board: &WorkBoardProjection,
    card: &WorkCard,
    pr: &airc_work::model::PullRequestRef,
    baselines: &mut super::BaselineCache,
    policy: super::GatePolicy,
) -> Result<CardGate, crate::gh_client::GhError> {
    let view = super::fetch_pr_view(gh, pr).await?;
    let gate = super::check_view_gate(gh, pr, &view, baselines, policy).await;
    let pr_head = view
        .head_ref_oid
        .as_deref()
        .and_then(|head| GitObjectId::new(head).ok());
    let verdict = verdict_authorizes(board, card, pr_head.as_ref());
    Ok(card_gate(gate, verdict, pr_head))
}

/// The gate's answer from its two halves over one view: the CI result, the verdict, and the head
/// that view reported. A merge is pinned to that head and to no other. Pure, so the pinning is
/// tested on plain values.
fn card_gate(
    gate: super::GateResult,
    verdict: Result<(), VerdictGap>,
    pr_head: Option<GitObjectId>,
) -> CardGate {
    match (gate, verdict, pr_head) {
        // Already merged on GitHub: the board reconciles, whatever its verdict.
        (super::GateResult::AlreadyMerged { merged_at_ms }, _, _) => {
            CardGate::AlreadyMerged { merged_at_ms }
        }
        (_, Err(gap), _) => CardGate::NotReady(gap.to_string()),
        (super::GateResult::NotReady(reason), Ok(()), _) => CardGate::NotReady(reason),
        (super::GateResult::Green, Ok(()), Some(head)) => CardGate::Merge(VerifiedHead(head)),
        // A verdict cannot pass without the head; said rather than assumed.
        (super::GateResult::Green, Ok(()), None) => {
            CardGate::NotReady(VerdictGap::PrHeadUnknown.to_string())
        }
    }
}

#[cfg(test)]
mod tests;
