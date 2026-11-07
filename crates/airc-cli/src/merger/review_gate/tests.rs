use super::*;

fn sha(c: char) -> airc_work::GitObjectId {
    airc_work::GitObjectId::new(c.to_string().repeat(40)).unwrap()
}

fn peer(n: u128) -> airc_core::PeerId {
    airc_core::PeerId::from_uuid(uuid::Uuid::from_u128(n))
}

fn submission(head: airc_work::SubmissionHead) -> WorkSubmission {
    WorkSubmission {
        submission_id: airc_work::SubmissionId::from_u128(1),
        card_id: airc_work::WorkCardId::from_uuid(uuid::Uuid::from_u128(2)),
        claim_id: airc_work::ClaimId::from_uuid(uuid::Uuid::from_u128(3)),
        instance: "repo".into(),
        base_sha: sha('a'),
        head,
        artifact: airc_blobs::MediaRef {
            hash: airc_blobs::ContentHash::from_bytes(b"patch"),
            size_bytes: 5,
            mime: None,
        },
        publisher: peer(10),
        submitted_at_ms: 1,
    }
}

fn review(
    of: &WorkSubmission,
    reviewer: u128,
    outcome: WorkReviewOutcome,
    at: u64,
) -> WorkSubmissionReview {
    WorkSubmissionReview {
        review_id: airc_work::WorkReviewId::from_uuid(uuid::Uuid::from_u128(100 + at as u128)),
        card_id: of.card_id,
        submission_id: of.submission_id,
        artifact: of.artifact.clone(),
        review_card_id: airc_work::WorkCardId::from_uuid(uuid::Uuid::from_u128(4)),
        review_claim_id: airc_work::ClaimId::from_uuid(uuid::Uuid::from_u128(5)),
        reviewer: peer(reviewer),
        outcome,
        evidence: of.artifact.clone(),
        evidence_text: None,
        reviewed_at_ms: at,
    }
}

// what this catches: a PR merging without a verdict that covers what it lands (llama.cpp#95/#96
// merged with none on the board, 2026-11-07). Only a passed, independent, latest review of a
// submission that ends at the PR's exact head authorizes; every other shape names why not.
#[test]
fn only_a_passed_independent_review_at_the_prs_head_authorizes_a_merge() {
    let head = sha('b');
    let at_head = submission(airc_work::SubmissionHead::At(sha('b')));
    let passed = review(&at_head, 20, WorkReviewOutcome::Passed, 5);
    assert_eq!(decide(&at_head, [&passed].into_iter(), Some(&head)), Ok(()));

    let legacy = submission(airc_work::SubmissionHead::Unrecorded);
    assert_eq!(
        decide(&legacy, [&passed].into_iter(), Some(&head)),
        Err(VerdictGap::HeadUnrecorded)
    );
    assert_eq!(
        decide(&at_head, [&passed].into_iter(), None),
        Err(VerdictGap::PrHeadUnknown)
    );
    let moved = sha('c');
    assert!(
        matches!(
            decide(&at_head, [&passed].into_iter(), Some(&moved)),
            Err(VerdictGap::HeadMoved { .. })
        ),
        "a verdict covers only the head it read"
    );

    let own = review(&at_head, 10, WorkReviewOutcome::Passed, 6);
    assert_eq!(
        decide(&at_head, [&own].into_iter(), Some(&head)),
        Err(VerdictGap::NoIndependentReview),
        "the author's own pass never merges"
    );
    assert_eq!(
        decide(&at_head, std::iter::empty(), Some(&head)),
        Err(VerdictGap::NoIndependentReview)
    );

    let failed_later = review(&at_head, 21, WorkReviewOutcome::Failed, 7);
    assert_eq!(
        decide(&at_head, [&passed, &failed_later].into_iter(), Some(&head)),
        Err(VerdictGap::LatestReview(WorkReviewOutcome::Failed)),
        "the latest independent judgement stands"
    );
    let passed_again = review(&at_head, 20, WorkReviewOutcome::Passed, 8);
    assert_eq!(
        decide(
            &at_head,
            [&passed, &failed_later, &passed_again].into_iter(),
            Some(&head)
        ),
        Ok(())
    );
}

// what this catches: a merge landing a head nobody judged (Fable on #1660: a push between the view
// and the merge). The merge is pinned to the head the view reported, a passing CI cannot override a
// verdict gap, and a missing head never merges.
#[test]
fn a_merge_is_pinned_to_the_head_the_gate_judged() {
    let green = || crate::merger::GateResult::Green;
    match card_gate(green(), Ok(()), Some(sha('b'))) {
        CardGate::Merge(head) => assert_eq!(head.commit(), &sha('b')),
        other => panic!("a passed verdict at a green head merges: {other:?}"),
    }
    let moved = Err(VerdictGap::HeadMoved {
        submitted: sha('b'),
        pr: sha('c'),
    });
    assert!(
        matches!(card_gate(green(), moved, Some(sha('c'))), CardGate::NotReady(reason) if reason.contains("verdict at"))
    );
    assert!(matches!(
        card_gate(green(), Ok(()), None),
        CardGate::NotReady(_)
    ));
    assert!(matches!(
        card_gate(crate::merger::GateResult::NotReady("1 failing check(s)".into()), Ok(()), Some(sha('b'))),
        CardGate::NotReady(reason) if reason.contains("failing")
    ));
    assert!(matches!(
        card_gate(
            crate::merger::GateResult::AlreadyMerged { merged_at_ms: 9 },
            Err(VerdictGap::NoSubmission),
            None
        ),
        CardGate::AlreadyMerged { merged_at_ms: 9 }
    ));
}
