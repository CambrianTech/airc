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
