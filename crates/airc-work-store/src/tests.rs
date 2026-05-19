use airc_core::{
    Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
    TranscriptKind,
};
use airc_store::{EventStore, InMemoryEventStore};
use airc_work::{
    encode_work_event, CardCreated, CardState, CardStateChanged, Priority, RepoId, WorkCardId,
    WorkEvent,
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
