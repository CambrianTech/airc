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
    match card_gate(green(), Ok(()), Ok(()), Some(sha('b'))) {
        CardGate::Merge(head) => assert_eq!(head.commit(), &sha('b')),
        other => panic!("a passed verdict at a green head merges: {other:?}"),
    }
    let moved = Err(VerdictGap::HeadMoved {
        submitted: sha('b'),
        pr: sha('c'),
    });
    assert!(
        matches!(card_gate(green(), moved, Ok(()), Some(sha('c'))), CardGate::NotReady(reason) if reason.contains("verdict at"))
    );
    assert!(matches!(
        card_gate(green(), Ok(()), Ok(()), None),
        CardGate::NotReady(_)
    ));
    assert!(matches!(
        card_gate(crate::merger::GateResult::NotReady("1 failing check(s)".into()), Ok(()), Ok(()), Some(sha('b'))),
        CardGate::NotReady(reason) if reason.contains("failing")
    ));
    assert!(matches!(
        card_gate(
            crate::merger::GateResult::AlreadyMerged { merged_at_ms: 9 },
            Err(VerdictGap::NoSubmission),
            Err(TargetGap::BaseUnknown),
            None
        ),
        CardGate::AlreadyMerged { merged_at_ms: 9 }
    ));
}

// what this catches: a stacked PR merged into its parent (2026-11-07 08:13Z: the merger squashed
// continuum #5806 into #5805's branch, its base, not canary). A PR whose base is not its repository's
// default branch waits, a view with no default branch is refused rather than guessed, and only a PR
// that targets the default lands.
#[test]
fn a_pr_lands_only_into_its_repositorys_default_branch() {
    let view = |base: Option<&str>, default: Option<&str>| crate::gh_client::PrView {
        state: "OPEN".into(),
        mergeable: "MERGEABLE".into(),
        status_check_rollup: Some(Vec::new()),
        merged_at: None,
        base_ref_name: base.map(str::to_string),
        base_ref_oid: None,
        head_ref_oid: None,
        base_default_branch: default.map(str::to_string),
    };
    assert_eq!(
        lands_in_default(&view(Some("canary"), Some("canary"))),
        Ok(())
    );
    let stacked = lands_in_default(&view(Some("fable/5805-collapse-log"), Some("canary")));
    assert!(
        matches!(&stacked, Err(TargetGap::Stacked { base, default }) if base == "fable/5805-collapse-log" && default == "canary")
    );
    assert_eq!(
        lands_in_default(&view(Some("canary"), None)),
        Err(TargetGap::DefaultUnknown)
    );
    assert_eq!(
        lands_in_default(&view(None, Some("canary"))),
        Err(TargetGap::BaseUnknown)
    );
    // A green, passed, pinned card still waits when stacked, and reconciles once already merged.
    let green = || crate::merger::GateResult::Green;
    assert!(matches!(
        card_gate(green(), Ok(()), stacked.clone(), Some(sha('b'))),
        CardGate::NotReady(reason) if reason.contains("stacked on fable/5805-collapse-log")
    ));
    assert!(matches!(
        card_gate(
            crate::merger::GateResult::AlreadyMerged { merged_at_ms: 7 },
            Ok(()),
            stacked,
            Some(sha('b'))
        ),
        CardGate::AlreadyMerged { merged_at_ms: 7 }
    ));
}

// what this catches: the merger draining the account's GitHub API (airc 315bf919, 2026-11-07: a
// PR view per Review card, ~60 a 30 s tick, 0/5000). A card with no board verdict (no submission,
// a submission with no recorded head, a recorded head nobody else reviewed) is decided on the
// board and costs no GitHub call at all.
#[tokio::test]
async fn a_card_without_a_board_verdict_costs_no_github_call() {
    use airc_lib::gh::client::mock::MockGhClient;
    use airc_work::{CardCreated, Priority, RepoId, WorkCardClaimed, WorkEvent};
    let gh = MockGhClient::new();
    let mut board = WorkBoardProjection::new();
    let owner = peer(10);
    let card = |n: u128| airc_work::WorkCardId::from_uuid(uuid::Uuid::from_u128(n));
    for n in [2u128, 3, 4] {
        board
            .apply(&WorkEvent::CardCreated(CardCreated {
                card_id: card(n),
                repo: RepoId::new("CambrianTech/airc").unwrap(),
                title: "a card in review".into(),
                body: None,
                priority: Priority::P1,
                lane_id: None,
                created_by: owner,
                created_at_ms: 1,
                reviews: None,
                origin: None,
            }))
            .unwrap();
    }
    for n in [3u128, 4] {
        board
            .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
                card_id: card(n),
                claim_id: airc_work::ClaimId::from_uuid(uuid::Uuid::from_u128(3)),
                owner,
                ttl_ms: 1_000_000,
                claimed_at_ms: 2,
                origin: airc_work::ClaimOrigin::Automatic,
                selected_at_ms: None,
            }))
            .unwrap();
    }
    // card 3: a legacy submission (no head); card 4: a recorded head nobody reviewed
    let mut legacy = submission(airc_work::SubmissionHead::Unrecorded);
    legacy.card_id = card(3);
    let mut unreviewed = submission(airc_work::SubmissionHead::At(sha('b')));
    unreviewed.card_id = card(4);
    unreviewed.submission_id = airc_work::SubmissionId::from_u128(9);
    for s in [legacy, unreviewed] {
        board.apply(&WorkEvent::WorkSubmitted(s)).unwrap();
    }
    let pr = airc_work::model::PullRequestRef {
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        number: 1,
        head: airc_work::model::BranchName::new("topic").unwrap(),
        base: airc_work::model::BranchName::new("canary").unwrap(),
    };
    let policy = crate::merger::GatePolicy {
        pending_timeout_ms: 0,
        now_ms: 0,
    };
    for n in [2u128, 3, 4] {
        let decided = check_card_gate(
            &gh,
            &board,
            board.card(card(n)).unwrap(),
            &pr,
            &mut crate::merger::BaselineCache::default(),
            policy,
        )
        .await
        .unwrap();
        assert!(
            matches!(decided, CardGate::NotReady(_)),
            "card {n}: {decided:?}"
        );
    }
    assert_eq!(
        gh.pr_view_call_count(),
        0,
        "no GitHub call for a card the board already refuses"
    );
}

// what this catches (Fable on #1638): the merger landing a conflicted card. A second, different
// creation of a card's id leaves it accepting only a release or a close, but the gate read only the
// verdict, so a card in Review with a PASSED independent review at the PR's head merged on GitHub
// and its PullRequestMerged was then refused, retried every tick. The gate refuses it on the board,
// before any GitHub call.
#[tokio::test]
async fn a_conflicted_card_never_merges_and_costs_no_github_call() {
    use airc_lib::gh::client::mock::MockGhClient;
    use airc_work::{CardCreated, Priority, RepoId, WorkCardClaimed, WorkEvent};
    let gh = MockGhClient::new();
    let mut board = WorkBoardProjection::new();
    let owner = peer(10);
    let card_id = airc_work::WorkCardId::from_uuid(uuid::Uuid::from_u128(2));
    let created = |title: &str| {
        WorkEvent::CardCreated(CardCreated {
            card_id,
            repo: RepoId::new("CambrianTech/airc").unwrap(),
            title: title.into(),
            body: None,
            priority: Priority::P1,
            lane_id: None,
            created_by: owner,
            created_at_ms: 1,
            reviews: None,
            origin: None,
        })
    };
    board
        .apply_at(airc_core::EventId::from_u128(1), &created("the card"))
        .unwrap();
    board
        .apply(&WorkEvent::CardClaimed(WorkCardClaimed {
            card_id,
            claim_id: airc_work::ClaimId::from_uuid(uuid::Uuid::from_u128(3)),
            owner,
            ttl_ms: 1_000_000,
            claimed_at_ms: 2,
            origin: airc_work::ClaimOrigin::Automatic,
            selected_at_ms: None,
        }))
        .unwrap();
    let submitted = submission(airc_work::SubmissionHead::At(sha('b')));
    board
        .apply(&WorkEvent::WorkSubmitted(submitted.clone()))
        .unwrap();
    board
        .apply(&WorkEvent::WorkSubmissionReviewed(review(
            &submitted,
            20,
            WorkReviewOutcome::Passed,
            3,
        )))
        .unwrap();
    board
        .apply_at(
            airc_core::EventId::from_u128(2),
            &created("a second creation"),
        )
        .unwrap();
    assert_eq!(
        board.card_conflicts(card_id).len(),
        1,
        "test: the conflict is recorded"
    );
    let pr = airc_work::model::PullRequestRef {
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        number: 1,
        head: airc_work::model::BranchName::new("topic").unwrap(),
        base: airc_work::model::BranchName::new("canary").unwrap(),
    };
    let policy = crate::merger::GatePolicy {
        pending_timeout_ms: 0,
        now_ms: 0,
    };
    let decided = check_card_gate(
        &gh,
        &board,
        board.card(card_id).unwrap(),
        &pr,
        &mut crate::merger::BaselineCache::default(),
        policy,
    )
    .await
    .unwrap();
    assert!(
        matches!(&decided, CardGate::NotReady(reason) if reason.contains("created twice")),
        "{decided:?}"
    );
    assert_eq!(
        gh.pr_view_call_count(),
        0,
        "decided on the board: no GitHub call"
    );
}
