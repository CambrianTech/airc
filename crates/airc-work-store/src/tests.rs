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
