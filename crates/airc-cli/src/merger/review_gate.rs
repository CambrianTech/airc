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
//! The board is read first and is free; GitHub is asked only about a card a reviewer already
//! passed at a recorded head, so a tick costs the reviewed queue, not the board.
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
    /// A second, different creation claimed the card's id: it accepts no action but a release or a
    /// close, so the merger never lands it (Fable on #1638).
    Conflicted {
        original_event: airc_core::EventId,
        conflicting_event: airc_core::EventId,
    },
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
            Self::Conflicted { original_event, conflicting_event } => write!(f, "the card's id was created twice (events {original_event} and {conflicting_event}): it accepts only a release or a close until resolved"),
        }
    }
}

/// Why a PR does not land where its repository integrates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TargetGap {
    /// Its base is another branch (a stacked PR): merging now lands it in its parent, not the
    /// integration line (continuum #5806 squashed into #5805's branch, 2026-11-07 08:13Z).
    Stacked {
        base: String,
        default: String,
    },
    BaseUnknown,
    /// The view has no default branch (the shell backend's `gh pr view` has none): refused, never guessed.
    DefaultUnknown,
}

impl std::fmt::Display for TargetGap {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Stacked { base, default } => write!(f, "stacked on {base}: it lands once its parent merges and it retargets to {default}"),
            Self::BaseUnknown => f.write_str("GitHub did not report the PR's base branch, so its landing cannot be checked"),
            Self::DefaultUnknown => f.write_str("the PR view has no repository default branch (the shell backend's gh pr view has none), so its landing cannot be checked"),
        }
    }
}

/// Whether the PR targets its repository's default branch, the one place a merge may land. Pure,
/// read from the same view the verdict and the CI were judged on.
fn lands_in_default(view: &crate::gh_client::PrView) -> Result<(), TargetGap> {
    let base = view
        .base_ref_name
        .as_deref()
        .ok_or(TargetGap::BaseUnknown)?;
    let default = view
        .base_default_branch
        .as_deref()
        .ok_or(TargetGap::DefaultUnknown)?;
    match base == default {
        true => Ok(()),
        false => Err(TargetGap::Stacked {
            base: base.to_string(),
            default: default.to_string(),
        }),
    }
}

/// The verdict as far as the board alone can tell, free: the latest submission's recorded head,
/// when its latest review by anyone but its author passed. A card that fails this costs no GitHub
/// call (airc 315bf919 fetched a PR view for every Review card, ~60 a tick, and drained the
/// account's API to 0/5000 on 2026-11-07; card 638e1453).
fn board_verdict<'a>(
    submission: &'a WorkSubmission,
    reviews: impl Iterator<Item = &'a WorkSubmissionReview>,
) -> Result<&'a GitObjectId, VerdictGap> {
    let submitted = submission.head.commit().ok_or(VerdictGap::HeadUnrecorded)?;
    let latest = reviews
        .filter(|review| {
            review.submission_id == submission.submission_id
                && review.reviewer != submission.publisher
        })
        .max_by_key(|review| review.reviewed_at_ms)
        .ok_or(VerdictGap::NoIndependentReview)?;
    match latest.outcome {
        WorkReviewOutcome::Passed => Ok(submitted),
        other => Err(VerdictGap::LatestReview(other)),
    }
}

/// The half only GitHub can tell: the reviewed head is the PR's head.
fn head_matches(submitted: &GitObjectId, pr_head: Option<&GitObjectId>) -> Result<(), VerdictGap> {
    match pr_head {
        None => Err(VerdictGap::PrHeadUnknown),
        Some(pr) if pr == submitted => Ok(()),
        Some(pr) => Err(VerdictGap::HeadMoved {
            submitted: submitted.clone(),
            pr: pr.clone(),
        }),
    }
}

/// The whole verdict over one submission, its reviews and the PR's head (the tests' entry).
#[cfg(test)]
fn decide<'a>(
    submission: &'a WorkSubmission,
    reviews: impl Iterator<Item = &'a WorkSubmissionReview>,
    pr_head: Option<&GitObjectId>,
) -> Result<(), VerdictGap> {
    head_matches(board_verdict(submission, reviews)?, pr_head)
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
    // The board first: nothing below costs GitHub unless a reviewer already passed a recorded head.
    if let Some(conflict) = board.card_conflicts(card.card_id).first() {
        return Ok(CardGate::NotReady(
            VerdictGap::Conflicted {
                original_event: conflict.original_event,
                conflicting_event: conflict.conflicting_event,
            }
            .to_string(),
        ));
    }
    let Some(submission) = card.latest_submission() else {
        return Ok(CardGate::NotReady(VerdictGap::NoSubmission.to_string()));
    };
    let submitted = match board_verdict(
        submission,
        board.submission_reviews_for(submission.submission_id),
    ) {
        Ok(head) => head.clone(),
        Err(gap) => return Ok(CardGate::NotReady(gap.to_string())),
    };
    let view = super::fetch_pr_view(gh, pr).await?;
    let gate = super::check_view_gate(gh, pr, &view, baselines, policy).await;
    let pr_head = view
        .head_ref_oid
        .as_deref()
        .and_then(|head| GitObjectId::new(head).ok());
    let verdict = head_matches(&submitted, pr_head.as_ref());
    Ok(card_gate(gate, verdict, lands_in_default(&view), pr_head))
}

/// The gate's answer from its halves over one view: the CI result, the verdict, where the PR
/// lands, and the head that view reported. A merge is pinned to that head and to no other. Pure, so
/// the pinning is tested on plain values.
fn card_gate(
    gate: super::GateResult,
    verdict: Result<(), VerdictGap>,
    target: Result<(), TargetGap>,
    pr_head: Option<GitObjectId>,
) -> CardGate {
    match (gate, verdict, target, pr_head) {
        // Already merged on GitHub: the board reconciles, whatever its verdict or target.
        (super::GateResult::AlreadyMerged { merged_at_ms }, _, _, _) => {
            CardGate::AlreadyMerged { merged_at_ms }
        }
        (_, Err(gap), _, _) => CardGate::NotReady(gap.to_string()),
        (_, Ok(()), Err(gap), _) => CardGate::NotReady(gap.to_string()),
        (super::GateResult::NotReady(reason), Ok(()), Ok(()), _) => CardGate::NotReady(reason),
        (super::GateResult::Green, Ok(()), Ok(()), Some(head)) => {
            CardGate::Merge(VerifiedHead(head))
        }
        // A verdict cannot pass without the head; said rather than assumed.
        (super::GateResult::Green, Ok(()), Ok(()), None) => {
            CardGate::NotReady(VerdictGap::PrHeadUnknown.to_string())
        }
    }
}

#[cfg(test)]
mod tests;
