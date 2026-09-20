use airc_core::{
    Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
    TranscriptKind,
};
use airc_store::{EventStore, InMemoryEventStore};
use airc_work::{
    encode_work_event, CardCreated, CardState, CardStateChanged, DrainCandidate,
    DrainCandidateCategory, DrainOutcome, PressureLevel, Priority, RepoId, WorkBoardProjection,
    WorkCardId, WorkEvent, WorkspaceDrainCompleted, WorkspaceDrainRequested, WorkspaceId,
    WorkspacePressureReported,
};

use super::*;

fn card_created(card_id: WorkCardId) -> WorkEvent {
    WorkEvent::CardCreated(CardCreated {
        card_id,
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "persisted work card".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        created_by: PeerId::from_u128(200),
        created_at_ms: 1000,
        reviews: None,
        origin: None,
    })
}

fn card_state_changed(card_id: WorkCardId) -> WorkEvent {
    WorkEvent::CardStateChanged(CardStateChanged {
        card_id,
        state: CardState::Review,
        changed_by: PeerId::from_u128(201),
        changed_at_ms: 2000,
    })
}

fn work_transcript(
    event_id: u128,
    room_id: RoomId,
    lamport: u64,
    event: &WorkEvent,
) -> TranscriptEvent {
    let (headers, body) = encode_work_event(event).unwrap();
    TranscriptEvent {
        event_id: EventId::from_u128(event_id),
        room_id,
        peer_id: PeerId::from_u128(2),
        client_id: ClientId::from_u128(3),
        kind: TranscriptKind::System,
        occurred_at_ms: event.occurred_at_ms(),
        lamport,
        target: MentionTarget::All,
        headers,
        body: Some(body),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}

fn chat_transcript(event_id: u128, room_id: RoomId, lamport: u64) -> TranscriptEvent {
    TranscriptEvent {
        event_id: EventId::from_u128(event_id),
        room_id,
        peer_id: PeerId::from_u128(2),
        client_id: ClientId::from_u128(3),
        kind: TranscriptKind::Message,
        occurred_at_ms: 1000 + lamport,
        lamport,
        target: MentionTarget::All,
        headers: Headers::new(),
        body: Some(Body::text("plain chat")),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}

#[tokio::test]
async fn page_recent_returns_only_decoded_work_events_and_latest_cursor() {
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let card_id = WorkCardId::from_u128(20);

    store
        .append(work_transcript(1, room, 1, &card_created(card_id)))
        .await
        .unwrap();
    store.append(chat_transcript(2, room, 2)).await.unwrap();
    store
        .append(work_transcript(3, room, 3, &card_state_changed(card_id)))
        .await
        .unwrap();

    let work_store = WorkEventStore::new(&store);
    let page = work_store.page_recent(Some(room), 10).await.unwrap();

    assert_eq!(page.events.len(), 2);
    assert_eq!(page.newest_cursor.unwrap().event_id, EventId::from_u128(3));
}

#[tokio::test]
async fn project_recent_rebuilds_board_from_persisted_events() {
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let card_id = WorkCardId::from_u128(20);

    store
        .append(work_transcript(1, room, 1, &card_created(card_id)))
        .await
        .unwrap();
    store
        .append(work_transcript(2, room, 2, &card_state_changed(card_id)))
        .await
        .unwrap();

    let projection = WorkEventStore::new(&store)
        .project_recent(Some(room), 10)
        .await
        .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.state, CardState::Review);
}

#[tokio::test]
async fn project_recent_skips_events_whose_anchor_is_outside_window() {
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let old_card = WorkCardId::from_u128(20);
    let visible_card = WorkCardId::from_u128(21);

    store
        .append(work_transcript(1, room, 1, &card_created(old_card)))
        .await
        .unwrap();
    store
        .append(work_transcript(2, room, 2, &card_state_changed(old_card)))
        .await
        .unwrap();
    store
        .append(work_transcript(3, room, 3, &card_created(visible_card)))
        .await
        .unwrap();

    let projection = WorkEventStore::new(&store)
        .project_recent(Some(room), 2)
        .await
        .unwrap();

    assert!(projection.card(old_card).is_none());
    let card = projection.card(visible_card).unwrap();
    assert_eq!(card.state, CardState::Open);
}

#[tokio::test]
async fn project_complete_pages_from_start_without_recent_window_loss() {
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let old_card = WorkCardId::from_u128(20);

    store
        .append(work_transcript(1, room, 1, &card_created(old_card)))
        .await
        .unwrap();
    for idx in 0..8 {
        store
            .append(chat_transcript(100 + idx, room, 2 + idx as u64))
            .await
            .unwrap();
    }
    store
        .append(work_transcript(2, room, 20, &card_state_changed(old_card)))
        .await
        .unwrap();

    let recent = WorkEventStore::new(&store)
        .project_recent(Some(room), 4)
        .await
        .unwrap();
    assert!(recent.card(old_card).is_none());

    let complete = WorkEventStore::new(&store)
        .project_complete(Some(room), 3)
        .await
        .unwrap();
    let card = complete.card(old_card).unwrap();
    assert_eq!(card.state, CardState::Review);
}

#[tokio::test]
async fn project_complete_tolerates_orphaned_work_events() {
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let orphan_card = WorkCardId::from_u128(19);
    let valid_card = WorkCardId::from_u128(20);

    store
        .append(work_transcript(
            1,
            room,
            1,
            &card_state_changed(orphan_card),
        ))
        .await
        .unwrap();
    store
        .append(work_transcript(2, room, 2, &card_created(valid_card)))
        .await
        .unwrap();

    let complete = WorkEventStore::new(&store)
        .project_complete(Some(room), 1)
        .await
        .unwrap();
    assert!(complete.card(orphan_card).is_none());
    assert!(complete.card(valid_card).is_some());
}

#[tokio::test]
async fn resume_from_uses_store_cursor_contract() {
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let card_a = WorkCardId::from_u128(20);
    let card_b = WorkCardId::from_u128(21);

    store
        .append(work_transcript(1, room, 1, &card_created(card_a)))
        .await
        .unwrap();
    let cursor = store.latest_cursor(Some(room)).await.unwrap().unwrap();
    store
        .append(work_transcript(2, room, 2, &card_created(card_b)))
        .await
        .unwrap();

    let page = WorkEventStore::new(&store)
        .resume_from(&cursor, Some(room), 10)
        .await
        .unwrap();

    assert_eq!(page.events, vec![card_created(card_b)]);
    assert_eq!(page.newest_cursor.unwrap().event_id, EventId::from_u128(2));
}

#[tokio::test]
async fn drain_sequence_through_store_replays_into_projection_state() {
    // Pressure → drain-request → drain-completed goes through the
    // append-only store, gets paged back out, and replays into the
    // expected projection state. Proves the events survive serialization
    // + cursor-based pagination + projection apply end-to-end.
    let store = InMemoryEventStore::new();
    let room = RoomId::from_u128(10);
    let workspace_id = WorkspaceId::from_u128(42);
    let repo = RepoId::new("CambrianTech/airc").unwrap();
    let reporter = PeerId::from_u128(7);
    let rule = "default.rebuildable".to_string();

    let pressure = WorkEvent::WorkspacePressureReported(WorkspacePressureReported {
        workspace_id,
        repo: repo.clone(),
        reporter,
        total_bytes: 1_000,
        available_bytes: 100,
        level: PressureLevel::High,
        reported_at_ms: 1,
    });
    let request = WorkEvent::WorkspaceDrainRequested(WorkspaceDrainRequested {
        workspace_id,
        repo: repo.clone(),
        requester: reporter,
        policy_rule_id: rule.clone(),
        dry_run: false,
        candidates: vec![DrainCandidate {
            path: "/tmp/work/target".to_string(),
            category: DrainCandidateCategory::RebuildableCache,
            est_bytes: 800,
        }],
        requested_at_ms: 2,
    });
    let completed = WorkEvent::WorkspaceDrainCompleted(WorkspaceDrainCompleted {
        workspace_id,
        repo,
        performer: reporter,
        policy_rule_id: rule,
        dry_run: false,
        outcome: DrainOutcome {
            bytes_reclaimed: 800,
            paths_touched: vec!["/tmp/work/target".to_string()],
            paths_skipped: vec![],
            errors: vec![],
        },
        completed_at_ms: 3,
    });

    store
        .append(work_transcript(1, room, 1, &pressure))
        .await
        .unwrap();
    store
        .append(work_transcript(2, room, 2, &request))
        .await
        .unwrap();
    store
        .append(work_transcript(3, room, 3, &completed))
        .await
        .unwrap();

    let page = WorkEventStore::new(&store)
        .page_recent(Some(room), 10)
        .await
        .unwrap();
    assert_eq!(page.events.len(), 3);

    let projection = WorkBoardProjection::replay(page.events).unwrap();
    assert_eq!(
        projection.workspace_pressure(&workspace_id).unwrap().level,
        PressureLevel::High,
    );
    assert!(projection.pending_drains_for(&workspace_id).is_empty());
    let history = projection.drain_history_for(&workspace_id);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].outcome.bytes_reclaimed, 800);
}

// ---------------------------------------------------------------------
// Card 1291173d: incremental resume (`apply_transcripts`) must be the
// SAME fold as the from-scratch replay (`project_transcripts`) — split
// anywhere, including across a first-write-wins arbitration boundary.
// ---------------------------------------------------------------------

fn card_claimed(card_id: WorkCardId, claim: u128, owner: u128, claimed_at_ms: u64) -> WorkEvent {
    WorkEvent::CardClaimed(airc_work::WorkCardClaimed {
        card_id,
        claim_id: airc_work::ClaimId::from_u128(claim),
        owner: PeerId::from_u128(owner),
        ttl_ms: 600_000,
        claimed_at_ms,
    })
}

fn submission(card_id: WorkCardId) -> airc_work::WorkSubmission {
    serde_json::from_value(serde_json::json!({
        "submission_id": airc_work::SubmissionId::from_u128(400),
        "card_id": card_id,
        "claim_id": airc_work::ClaimId::from_u128(90),
        "instance": "cross-grid-case",
        "base_sha": "a".repeat(40),
        "artifact": { "hash": "b".repeat(64), "size_bytes": 5, "mime": "text/x-diff" },
        "publisher": PeerId::from_u128(2),
        "submitted_at_ms": 6000
    }))
    .unwrap()
}

// what this catches: card89af25c7 — only the signed reviewer and the claim at
// review time can attest an exact artifact; snapshot/replay cannot regrade it.
#[tokio::test]
async fn reviewed_submission_preserves_signed_authority_across_every_replay_boundary() {
    use airc_work::{ClaimId, WorkReviewId, WorkReviewOutcome, WorkSubmissionReview};
    let room = RoomId::from_u128(10);
    let parent = WorkCardId::from_u128(20);
    let reviewer_card = WorkCardId::from_u128(21);
    let candidate = submission(parent);
    let mut review_created = card_created(reviewer_card);
    if let WorkEvent::CardCreated(created) = &mut review_created {
        created.reviews = Some(parent);
    }
    let review = WorkSubmissionReview {
        review_id: WorkReviewId::from_u128(700),
        card_id: parent,
        submission_id: candidate.submission_id,
        artifact: candidate.artifact.clone(),
        review_card_id: reviewer_card,
        review_claim_id: ClaimId::from_u128(91),
        reviewer: PeerId::from_u128(3),
        outcome: WorkReviewOutcome::Passed,
        evidence: candidate.artifact.clone(),
        reviewed_at_ms: 7000,
    };
    let (headers, _) =
        encode_work_event(&WorkEvent::WorkSubmissionReviewed(review.clone())).unwrap();
    let legacy_filter = airc_core::HeaderFilter::Exact {
        key: airc_protocol::HEADER_FORGE_BODY_HINT.into(),
        value: airc_work::BODY_HINT_FORGE_WORK_EVENT.into(),
    };
    assert!(
        !legacy_filter.matches(&headers),
        "legacy work readers must ignore the extension"
    );
    assert!(airc_work::work_event_subscription()
        .headers_filter
        .matches(&headers));
    let mut repeated = review.clone();
    repeated.reviewed_at_ms = 900_000; // old claim is now expired/closed
    let mut conflict = repeated.clone();
    conflict.outcome = WorkReviewOutcome::Failed;
    let events = [
        card_created(parent),
        card_claimed(parent, 90, 2, 5000),
        WorkEvent::WorkSubmitted(candidate),
        review_created,
        card_claimed(reviewer_card, 91, 3, 6000),
        WorkEvent::WorkSubmissionReviewed(review.clone()),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id: reviewer_card,
            state: CardState::Closed,
            changed_by: review.reviewer,
            changed_at_ms: 8000,
        }),
        WorkEvent::WorkSubmissionReviewed(repeated),
        WorkEvent::WorkSubmissionReviewed(conflict),
    ];
    let transcripts: Vec<_> = events
        .iter()
        .enumerate()
        .map(|(i, event)| {
            let mut wire = work_transcript(i as u128 + 1, room, i as u64 + 1, event);
            match event {
                WorkEvent::WorkSubmissionReviewed(review) => wire.peer_id = review.reviewer,
                WorkEvent::CardClaimed(claim) => wire.peer_id = claim.owner,
                _ => {}
            }
            wire
        })
        .collect();
    let full = project_transcripts(transcripts.clone()).unwrap();
    assert_eq!(full.submission_review(review.review_id), Some(&review));
    assert_eq!(full.submission_reviews_for(review.submission_id).count(), 1);
    assert_eq!(
        full.last_review_rejection(parent).unwrap().reason,
        airc_work::WorkReviewRejectionReason::ConflictingId
    );
    for split in 0..=transcripts.len() {
        let before = project_transcripts(transcripts[..split].to_vec()).unwrap();
        let mut resumed: WorkBoardProjection =
            serde_json::from_slice(&serde_json::to_vec(&before).unwrap()).unwrap();
        apply_transcripts(&mut resumed, transcripts[split..].to_vec()).unwrap();
        assert_eq!(resumed, full, "split {split}");
    }
    let store = InMemoryEventStore::new();
    for event in transcripts {
        store.append(event).await.unwrap();
    }
    assert_eq!(
        WorkEventStore::new(&store)
            .project_complete(Some(room), 2)
            .await
            .unwrap(),
        full
    );

    // Negative controls use a board with a real accepted submission and live
    // review claim. A validator refusing every review cannot pass this fixture.
    let prefix: Vec<_> = events[..5]
        .iter()
        .enumerate()
        .map(|(i, event)| work_transcript(i as u128 + 1, room, i as u64 + 1, event))
        .collect();
    let mut cases = Vec::new();
    let mut wrong = review.clone();
    wrong.artifact.size_bytes += 1;
    cases.push((
        wrong,
        review.reviewer,
        airc_work::WorkReviewRejectionReason::ArtifactMismatch,
    ));
    let mut wrong = review.clone();
    wrong.submission_id = airc_work::SubmissionId::new();
    cases.push((
        wrong,
        review.reviewer,
        airc_work::WorkReviewRejectionReason::UnknownSubmission,
    ));
    let mut wrong = review.clone();
    wrong.review_card_id = parent;
    cases.push((
        wrong,
        review.reviewer,
        airc_work::WorkReviewRejectionReason::WrongReviewCard,
    ));
    let mut wrong = review.clone();
    wrong.review_claim_id = ClaimId::new();
    cases.push((
        wrong,
        review.reviewer,
        airc_work::WorkReviewRejectionReason::WrongClaim,
    ));
    let mut wrong = review.clone();
    wrong.reviewed_at_ms = 606_000;
    cases.push((
        wrong,
        review.reviewer,
        airc_work::WorkReviewRejectionReason::ExpiredClaim,
    ));
    let mut wrong = review.clone();
    wrong.evidence.size_bytes = 0;
    cases.push((
        wrong,
        review.reviewer,
        airc_work::WorkReviewRejectionReason::InvalidEvidence,
    ));
    cases.push((
        review.clone(),
        PeerId::from_u128(4),
        airc_work::WorkReviewRejectionReason::ReviewerMismatch,
    ));
    for (wrong, signed_author, reason) in cases {
        let mut wire = work_transcript(10, room, 10, &WorkEvent::WorkSubmissionReviewed(wrong));
        wire.peer_id = signed_author;
        let mut projected = project_transcripts(prefix.clone()).unwrap();
        apply_transcripts(&mut projected, vec![wire]).unwrap();
        assert!(
            projected.submission_review(review.review_id).is_none(),
            "{reason:?}"
        );
        let refusal = projected.last_review_rejection(parent).unwrap();
        assert_eq!(refusal.reason, reason);
        assert_eq!(refusal.reviewer, signed_author);
    }
    for outcome in [WorkReviewOutcome::Failed, WorkReviewOutcome::Unknown] {
        let mut judgement = review.clone();
        judgement.outcome = outcome;
        let mut wire = work_transcript(10, room, 10, &WorkEvent::WorkSubmissionReviewed(judgement));
        wire.peer_id = review.reviewer;
        let mut projected = project_transcripts(prefix.clone()).unwrap();
        apply_transcripts(&mut projected, vec![wire]).unwrap();
        assert_eq!(
            projected
                .submission_review(review.review_id)
                .unwrap()
                .outcome,
            outcome
        );
    }
    // A typed extension cannot hide legacy mutations from old readers. Exercise
    // the actual full projection on each side of that filter, not only decoding.
    let mut disguised = prefix.clone();
    for (i, event) in [
        card_created(WorkCardId::from_u128(999)),
        card_claimed(parent, 999, 4, 900_000),
    ]
    .into_iter()
    .enumerate()
    {
        let mut wire = work_transcript(20 + i as u128, room, 20 + i as u64, &event);
        wire.headers.insert(
            airc_protocol::HEADER_FORGE_BODY_HINT.into(),
            airc_work::BODY_HINT_FORGE_WORK_REVIEW.into(),
        );
        assert!(matches!(
            airc_work::decode_work_event(&wire.headers, wire.body.as_ref()),
            Err(airc_work::WorkEventCodecError::VariantHintMismatch { .. })
        ));
        disguised.push(wire);
    }
    let mut unknown = work_transcript(
        22,
        room,
        22,
        &WorkEvent::WorkSubmissionReviewed(review.clone()),
    );
    unknown.body = Some(Body::Json(serde_json::json!({"kind":"unsupported_review"})));
    unknown
        .headers
        .remove(airc_work::HEADER_FORGE_WORK_EVENT_KIND);
    disguised.push(unknown);
    disguised.push(work_transcript(
        23,
        room,
        23,
        &card_created(WorkCardId::from_u128(998)),
    ));
    let old_visible = disguised
        .iter()
        .filter(|event| legacy_filter.matches(&event.headers))
        .cloned()
        .collect();
    let legacy = project_transcripts(old_visible).unwrap();
    let upgraded = project_transcripts(disguised).unwrap();
    assert_eq!(upgraded, legacy);
    assert!(upgraded.card(WorkCardId::from_u128(999)).is_none());
    assert!(upgraded.card(WorkCardId::from_u128(998)).is_some());
    assert_eq!(
        upgraded.card(parent).unwrap().owner,
        Some(PeerId::from_u128(2))
    );

    let mut wrong_hint = work_transcript(
        10,
        room,
        10,
        &WorkEvent::WorkSubmissionReviewed(review.clone()),
    );
    wrong_hint.headers.insert(
        airc_protocol::HEADER_FORGE_BODY_HINT.into(),
        airc_work::BODY_HINT_FORGE_WORK_EVENT.into(),
    );
    let before = project_transcripts(prefix.clone()).unwrap();
    let mut after = before.clone();
    apply_transcripts(&mut after, vec![wrong_hint]).unwrap();
    assert_eq!(after, before, "review under legacy hint cannot be accepted");

    let mut malformed = work_transcript(10, room, 10, &WorkEvent::WorkSubmissionReviewed(review));
    malformed.body = Some(Body::Json(
        serde_json::json!({"kind":"work_submission_reviewed", "reviewer":"broken"}),
    ));
    let before = project_transcripts(prefix).unwrap();
    let mut after = before.clone();
    apply_transcripts(&mut after, vec![malformed]).unwrap();
    assert_eq!(after, before, "malformed review cannot poison the board");
}

#[tokio::test]
async fn submissions_survive_retry_conflict_settle_and_every_snapshot_boundary() {
    let room = RoomId::from_u128(10);
    let id = WorkCardId::from_u128(20);
    let first = submission(id);
    let mut retry = first.clone();
    retry.submitted_at_ms += 100;
    let mut conflict = retry.clone();
    conflict.artifact.size_bytes += 1;
    let mut next = first.clone();
    next.submission_id = airc_work::SubmissionId::from_u128(401);
    next.submitted_at_ms -= 100; // transcript order, not publisher clock, wins
    let events = [
        card_created(id),
        card_claimed(id, 90, 2, 5000),
        WorkEvent::WorkSubmitted(first.clone()),
        WorkEvent::WorkSubmitted(retry.clone()),
        WorkEvent::WorkSubmitted(conflict),
        WorkEvent::WorkSubmitted(next.clone()),
        WorkEvent::CardStateChanged(CardStateChanged {
            card_id: id,
            state: CardState::Closed,
            changed_by: PeerId::from_u128(2),
            changed_at_ms: 7000,
        }),
        WorkEvent::WorkSubmitted(retry),
    ];
    let transcripts: Vec<_> = events
        .iter()
        .enumerate()
        .map(|(i, event)| work_transcript(i as u128 + 1, room, i as u64 + 1, event))
        .collect();
    let full = project_transcripts(transcripts.clone()).unwrap();
    assert_eq!(full.card(id).unwrap().submissions, vec![next, first]);
    assert_eq!(
        full.card(id)
            .unwrap()
            .last_submission_rejection
            .as_ref()
            .unwrap()
            .reason,
        airc_work::SubmissionRejectionReason::ConflictingId
    );
    for split in 0..=transcripts.len() {
        let mut resumed = project_transcripts(transcripts[..split].to_vec()).unwrap();
        apply_transcripts(&mut resumed, transcripts[split..].to_vec()).unwrap();
        assert_eq!(resumed, full);
    }
    let store = InMemoryEventStore::new();
    for event in transcripts {
        store.append(event).await.unwrap();
    }
    assert_eq!(
        WorkEventStore::new(&store)
            .project_complete(Some(room), 2)
            .await
            .unwrap(),
        full
    );
}

#[tokio::test]
async fn malformed_submission_cannot_poison_board_or_hide_other_corruption() {
    let room = RoomId::from_u128(10);
    let id = WorkCardId::from_u128(20);
    for header in [None, Some("card_created"), Some("work_submitted")] {
        let mut malformed = work_transcript(2, room, 2, &WorkEvent::WorkSubmitted(submission(id)));
        malformed.body = Some(Body::Json(
            serde_json::json!({"kind": "work_submitted", "artifact": {"hash":"invalid"}}),
        ));
        malformed
            .headers
            .remove(airc_work::HEADER_FORGE_WORK_EVENT_KIND);
        if let Some(header) = header {
            malformed.headers.insert(
                airc_work::HEADER_FORGE_WORK_EVENT_KIND.into(),
                header.into(),
            );
        }
        let transcripts = vec![
            work_transcript(1, room, 1, &card_created(id)),
            malformed,
            work_transcript(3, room, 3, &card_state_changed(id)),
        ];
        let full = project_transcripts(transcripts.clone()).unwrap();
        assert_eq!(full.card(id).unwrap().state, CardState::Review);
        assert_eq!(
            airc_work::project_transcript_work_events(transcripts.clone()).unwrap(),
            full
        );
        let store = InMemoryEventStore::new();
        for event in transcripts {
            store.append(event).await.unwrap();
        }
        assert_eq!(
            WorkEventStore::new(&store)
                .project_complete(Some(room), 1)
                .await
                .unwrap(),
            full
        );
    }
    let mut unrelated = work_transcript(4, room, 4, &card_created(id));
    unrelated.headers.insert(
        airc_work::HEADER_FORGE_WORK_EVENT_KIND.into(),
        "work_submitted".into(),
    );
    unrelated.body = Some(Body::Json(serde_json::json!({"kind":"card_created"})));
    assert!(project_transcripts(vec![unrelated.clone()]).is_err());
    assert!(airc_work::project_transcript_work_events(vec![unrelated]).is_err());
}

#[test]
fn submission_admission_rejects_spoof_stale_claim_and_invalid_metadata() {
    use airc_work::SubmissionRejectionReason as Reason;
    let room = RoomId::from_u128(10);
    let id = WorkCardId::from_u128(20);
    let initial = vec![
        work_transcript(1, room, 1, &card_created(id)),
        work_transcript(2, room, 2, &card_claimed(id, 90, 2, 5000)),
    ];
    let mut cases = Vec::new();
    let mut s = submission(id);
    s.publisher = PeerId::from_u128(999);
    cases.push((s, Reason::PublisherMismatch));
    let mut s = submission(id);
    s.claim_id = airc_work::ClaimId::from_u128(91);
    cases.push((s, Reason::WrongClaim));
    let mut s = submission(id);
    s.submitted_at_ms = 605000;
    cases.push((s, Reason::ExpiredClaim));
    let mut s = submission(id);
    s.instance.clear();
    cases.push((s, Reason::InvalidInstance));
    let mut s = submission(id);
    s.artifact.mime = Some("x".repeat(129));
    cases.push((s, Reason::InvalidArtifact));
    let mut s = submission(id);
    s.base_sha = serde_json::from_value(serde_json::json!("HEAD")).unwrap();
    cases.push((s, Reason::InvalidBase));
    for (submission, reason) in cases {
        let mut events = initial.clone();
        events.push(work_transcript(
            3,
            room,
            3,
            &WorkEvent::WorkSubmitted(submission),
        ));
        events.push(work_transcript(4, room, 4, &card_state_changed(id)));
        let board = project_transcripts(events).unwrap();
        let card = board.card(id).unwrap();
        assert!(card.submissions.is_empty());
        assert_eq!(
            card.last_submission_rejection.as_ref().unwrap().reason,
            reason
        );
        assert_eq!(card.state, CardState::Review);
    }
}

#[test]
fn submission_wire_stays_small_and_legacy_cards_default_to_no_submissions() {
    let id = WorkCardId::from_u128(20);
    let mut candidate = submission(id);
    candidate.artifact.size_bytes = u64::MAX;
    candidate.instance = "x".repeat(512);
    candidate.artifact.mime = Some("x".repeat(128));
    candidate.validate().unwrap();
    let event = WorkEvent::WorkSubmitted(candidate.clone());
    let (headers, body) = encode_work_event(&event).unwrap();
    assert_eq!(
        airc_work::decode_work_event(&headers, Some(&body)).unwrap(),
        event
    );
    assert!(serde_json::to_vec(&event).unwrap().len() < 2048);
    // Rejection receipts are projection-only, never publisher-supplied truth.
    assert!(serde_json::from_value::<WorkEvent>(serde_json::json!({
        "kind": "submission_rejected",
        "submission_id": candidate.submission_id,
        "card_id": id,
        "publisher": candidate.publisher,
        "submitted_at_ms": candidate.submitted_at_ms,
        "reason": "wrong_claim"
    }))
    .is_err());
    let board = project_transcripts(vec![work_transcript(
        1,
        RoomId::from_u128(10),
        1,
        &card_created(id),
    )])
    .unwrap();
    let mut legacy = serde_json::to_value(board.card(id).unwrap()).unwrap();
    legacy.as_object_mut().unwrap().remove("submissions");
    legacy
        .as_object_mut()
        .unwrap()
        .remove("last_submission_rejection");
    let decoded: airc_work::WorkCard = serde_json::from_value(legacy).unwrap();
    assert!(decoded.submissions.is_empty());
    assert!(decoded.last_submission_rejection.is_none());
}

#[test]
fn apply_transcripts_resume_equals_full_replay_across_claim_arbitration() {
    let room = RoomId::from_u128(10);
    let card_a = WorkCardId::from_u128(20);
    let card_b = WorkCardId::from_u128(21);

    // Claim by peer 300 lands first; peer 301's racing claim arrives
    // AFTER the snapshot boundary and must still lose first-write-wins
    // arbitration against state restored from the snapshot.
    let transcripts = vec![
        work_transcript(1, room, 1, &card_created(card_a)),
        work_transcript(2, room, 2, &card_claimed(card_a, 90, 300, 5_000)),
        // ---- snapshot boundary ----
        work_transcript(3, room, 3, &card_claimed(card_a, 91, 301, 6_000)),
        work_transcript(4, room, 4, &card_created(card_b)),
    ];

    let full = project_transcripts(transcripts.clone()).unwrap();

    let mut resumed = project_transcripts(transcripts[..2].to_vec()).unwrap();
    let newest = apply_transcripts(&mut resumed, transcripts[2..].to_vec())
        .unwrap()
        .expect("non-empty increment yields a cursor");

    assert_eq!(resumed, full, "incremental fold diverged from full replay");
    assert_eq!(newest.event_id, EventId::from_u128(4));
    // The arbitration itself: the pre-boundary claim won, the
    // post-boundary racer was dropped without state change.
    let card = resumed.card(card_a).expect("card A projected");
    assert_eq!(card.owner, Some(PeerId::from_u128(300)));
    assert_eq!(card.claim_id, Some(airc_work::ClaimId::from_u128(90)));
}

#[test]
fn apply_transcripts_advances_cursor_past_non_work_events() {
    let room = RoomId::from_u128(10);
    let card_id = WorkCardId::from_u128(20);
    let mut projection =
        project_transcripts(vec![work_transcript(1, room, 1, &card_created(card_id))]).unwrap();
    let before = projection.clone();

    // A chat-only increment applies nothing but still advances the
    // resume cursor — otherwise every subsequent resume would refetch
    // the same chat tail forever.
    let newest = apply_transcripts(&mut projection, vec![chat_transcript(9, room, 9)])
        .unwrap()
        .expect("chat transcript still yields a cursor");
    assert_eq!(newest.event_id, EventId::from_u128(9));
    assert_eq!(newest.lamport, 9);
    assert_eq!(projection, before);
}

#[test]
fn apply_transcripts_skips_missing_anchor_but_fails_structural_errors() {
    let room = RoomId::from_u128(10);
    let card_a = WorkCardId::from_u128(20);
    let mut projection =
        project_transcripts(vec![work_transcript(1, room, 1, &card_created(card_a))]).unwrap();

    // Missing anchor (state change for a card the snapshot never saw):
    // skipped, same as replay_window.
    let unknown = WorkCardId::from_u128(99);
    apply_transcripts(
        &mut projection,
        vec![work_transcript(2, room, 2, &card_state_changed(unknown))],
    )
    .unwrap();
    assert!(projection.card(unknown).is_none());

    // Structural error (duplicate create): loud failure, not a skip.
    let result = apply_transcripts(
        &mut projection,
        vec![work_transcript(3, room, 3, &card_created(card_a))],
    );
    assert!(matches!(result, Err(WorkStoreError::Projection(_))));
}

#[test]
fn apply_transcripts_resume_applies_relink_after_snapshot_boundary() {
    // Card 09fddedd — the work-board-cache resume path (card 1291173d:
    // projection snapshot keyed by last-applied cursor) must fold a
    // `PullRequestRelinked` event that lands AFTER the snapshot
    // boundary identically to a full from-zero replay. If the resume
    // rule and the full-replay rule ever diverge on the new variant,
    // cached boards would keep pointing the merger at the superseded
    // PR — the exact stale-link failure the relink verb exists to fix.
    let room = RoomId::from_u128(11);
    let card = WorkCardId::from_u128(30);
    let pr = |number: u64, head: &str| airc_work::PullRequestRef {
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        number,
        head: airc_work::BranchName::new(head).unwrap(),
        base: airc_work::BranchName::new("rust-rewrite").unwrap(),
    };
    let linked = WorkEvent::PullRequestLinked(airc_work::PullRequestLinked {
        card_id: card,
        pull_request: pr(1078, "feat/round-1"),
        linked_by: PeerId::from_u128(200),
        linked_at_ms: 1100,
    });
    let relinked = WorkEvent::PullRequestRelinked(airc_work::PullRequestRelinked {
        card_id: card,
        old_pull_request: pr(1078, "feat/round-1"),
        new_pull_request: pr(1137, "feat/round-2"),
        relinked_by: PeerId::from_u128(201),
        relinked_at_ms: 1200,
    });

    let transcripts = vec![
        work_transcript(1, room, 1, &card_created(card)),
        work_transcript(2, room, 2, &linked),
        // ---- snapshot boundary (cache cursor parked here) ----
        work_transcript(3, room, 3, &relinked),
    ];

    let full = project_transcripts(transcripts.clone()).unwrap();

    let mut resumed = project_transcripts(transcripts[..2].to_vec()).unwrap();
    assert_eq!(
        resumed.card(card).unwrap().pull_request,
        Some(pr(1078, "feat/round-1")),
        "snapshot state must hold the superseded link before catch-up"
    );
    let newest = apply_transcripts(&mut resumed, transcripts[2..].to_vec())
        .unwrap()
        .expect("non-empty increment yields a cursor");

    assert_eq!(resumed, full, "cache catch-up diverged from full replay");
    assert_eq!(newest.event_id, EventId::from_u128(3));
    let caught_up = resumed.card(card).unwrap();
    assert_eq!(caught_up.pull_request, Some(pr(1137, "feat/round-2")));
    assert_eq!(caught_up.state, CardState::Review);
}
