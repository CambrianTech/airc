use airc_core::{
    Body, ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptEvent,
    TranscriptKind,
};
use airc_protocol::HEADER_FORGE_BODY_HINT;

use super::*;
use crate::{
    encode_work_event, CardCreated, CardState, CardStateChanged, Priority, RepoId, WorkCardId,
    WorkEvent, BODY_HINT_FORGE_WORK_EVENT,
};

fn card_created(card_id: WorkCardId, created_at_ms: u64) -> WorkEvent {
    WorkEvent::CardCreated(CardCreated {
        card_id,
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "recorded work event".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: None,
        created_by: PeerId::from_u128(101),
        created_at_ms,
    })
}

fn card_state_changed(card_id: WorkCardId, changed_at_ms: u64) -> WorkEvent {
    WorkEvent::CardStateChanged(CardStateChanged {
        card_id,
        state: CardState::Review,
        changed_by: PeerId::from_u128(102),
        changed_at_ms,
    })
}

fn transcript(event_id: u128, lamport: u64, work_event: &WorkEvent) -> TranscriptEvent {
    let (headers, body) = encode_work_event(work_event).unwrap();
    TranscriptEvent {
        event_id: EventId::from_u128(event_id),
        room_id: RoomId::from_u128(1),
        peer_id: PeerId::from_u128(2),
        client_id: ClientId::from_u128(3),
        kind: TranscriptKind::System,
        occurred_at_ms: work_event.occurred_at_ms(),
        lamport,
        target: MentionTarget::All,
        headers,
        body: Some(body),
        attachment: None,
        receipt: None,
        metadata: serde_json::Value::Null,
    }
}

#[test]
fn transcript_work_event_decode_preserves_cursor() {
    let event = card_created(WorkCardId::from_u128(10), 1000);
    let transcript = transcript(99, 7, &event);

    let item = decode_transcript_work_event(&transcript).unwrap();

    assert_eq!(item.event, event);
    assert_eq!(item.cursor.lamport, 7);
    assert_eq!(item.cursor.event_id, EventId::from_u128(99));
}

#[test]
fn replay_sorts_by_lamport_then_event_id_before_projection() {
    let card_id = WorkCardId::from_u128(10);
    let create = card_created(card_id, 1000);
    let review = card_state_changed(card_id, 2000);

    let projection = project_transcript_work_events(vec![
        transcript(20, 2, &review),
        transcript(10, 1, &create),
    ])
    .unwrap();

    let card = projection.card(card_id).unwrap();
    assert_eq!(card.state, CardState::Review);
    assert_eq!(card.updated_at_ms, 2000);
}

#[test]
fn replay_rejects_non_work_transcript_explicitly() {
    let mut transcript = transcript(10, 1, &card_created(WorkCardId::from_u128(10), 1000));
    transcript.headers = Headers::new();
    transcript.body = Some(Body::text("plain chat"));

    assert!(matches!(
        project_transcript_work_events(vec![transcript]),
        Err(WorkReplayError::NotWorkEvent { .. })
    ));
}

#[test]
fn replay_rejects_invalid_work_payload_with_event_id() {
    let mut transcript = transcript(10, 1, &card_created(WorkCardId::from_u128(10), 1000));
    transcript.body = Some(Body::Binary(vec![1, 2, 3]));

    assert!(matches!(
        project_transcript_work_events(vec![transcript]),
        Err(WorkReplayError::Codec {
            event_id,
            source: crate::WorkEventCodecError::NonJsonBody
        }) if event_id == EventId::from_u128(10)
    ));
}

#[test]
fn work_event_detection_uses_body_hint_header_only() {
    let mut transcript = transcript(10, 1, &card_created(WorkCardId::from_u128(10), 1000));
    assert!(transcript_is_work_event(&transcript));

    transcript.headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        "forge.persona.turn".to_string(),
    );
    assert!(!transcript_is_work_event(&transcript));

    transcript.headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_WORK_EVENT.to_string(),
    );
    assert!(transcript_is_work_event(&transcript));
}
