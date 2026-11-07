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

use airc_work::{
    WorkBoardProjection, WorkCard, WorkReviewOutcome, WorkSubmission, WorkSubmissionReview,
};

/// Why a card's verdict does not authorize a merge.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum VerdictGap {
    NoSubmission,
    HeadUnrecorded,
    PrHeadUnknown,
    HeadMoved { submitted: String, pr: String },
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
    pr_head: Option<&str>,
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
    pr_head: Option<&str>,
) -> Result<(), VerdictGap> {
    let submitted = submission.head.commit().ok_or(VerdictGap::HeadUnrecorded)?;
    let pr_head = pr_head.ok_or(VerdictGap::PrHeadUnknown)?;
    if submitted.as_str() != pr_head {
        return Err(VerdictGap::HeadMoved {
            submitted: submitted.as_str().to_owned(),
            pr: pr_head.to_owned(),
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

/// The whole gate over one card: its verdict at the PR's head, then CI no redder than the base,
/// both read from ONE fetched view so the head checked is the head judged.
pub(crate) async fn check_card_gate(
    gh: &dyn crate::gh_client::GhClient,
    board: &WorkBoardProjection,
    card: &WorkCard,
    pr: &airc_work::model::PullRequestRef,
    baselines: &mut super::BaselineCache,
    policy: super::GatePolicy,
) -> Result<super::GateResult, crate::gh_client::GhError> {
    let view = super::fetch_pr_view(gh, pr).await?;
    let gate = super::check_view_gate(gh, pr, &view, baselines, policy).await;
    // An already-merged PR reconciles whatever its verdict: the merge happened.
    if matches!(gate, super::GateResult::AlreadyMerged { .. }) {
        return Ok(gate);
    }
    if let Err(gap) = verdict_authorizes(board, card, view.head_ref_oid.as_deref()) {
        return Ok(super::GateResult::NotReady(gap.to_string()));
    }
    Ok(gate)
}

#[cfg(test)]
mod tests;
