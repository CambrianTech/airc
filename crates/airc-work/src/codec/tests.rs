use airc_core::{Body, PeerId};
use airc_protocol::{FrameKind, HEADER_FORGE_BODY_HINT};

use super::*;
use crate::{
    BranchName, CardCreated, ClaimId, LaneId, Priority, RepoId, WorkCardId, WorkEvent, WorkspaceId,
    WorkspaceRequested,
};

fn card_created() -> WorkEvent {
    WorkEvent::CardCreated(CardCreated {
        card_id: WorkCardId::from_u128(1),
        repo: RepoId::new("CambrianTech/airc").unwrap(),
        title: "make work events routable".to_string(),
        body: None,
        priority: Priority::P1,
        lane_id: Some(LaneId::from_u128(2)),
        created_by: PeerId::from_u128(3),
        created_at_ms: 4,
    })
}

#[test]
fn work_event_roundtrips_through_headers_and_body() {
    let event = card_created();

    let (headers, body) = encode_work_event(&event).unwrap();
    let decoded = decode_work_event(&headers, Some(&body)).unwrap();

    assert_eq!(decoded, event);
    assert_eq!(
        headers.get(HEADER_FORGE_BODY_HINT).map(String::as_str),
        Some(BODY_HINT_FORGE_WORK_EVENT)
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_EVENT_KIND)
            .map(String::as_str),
        Some("card_created")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/airc")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CARD_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-000000000001")
    );
}

#[test]
fn subscription_matches_work_events_without_parsing_body() {
    let event = card_created();
    let (headers, _) = encode_work_event(&event).unwrap();
    let sub = work_event_subscription();

    assert!(sub.headers_filter.matches(&headers));
    assert!(sub.kinds.contains(&FrameKind::Event));
}

#[test]
fn decode_rejects_wrong_hint_and_non_json_body() {
    let event = card_created();
    let (mut headers, _) = encode_work_event(&event).unwrap();
    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        "forge.chat.text".to_string(),
    );

    assert!(matches!(
        decode_work_event(&headers, Some(&Body::text("not work"))),
        Err(WorkEventCodecError::BodyHintMismatch { .. })
    ));

    headers.insert(
        HEADER_FORGE_BODY_HINT.to_string(),
        BODY_HINT_FORGE_WORK_EVENT.to_string(),
    );
    assert!(matches!(
        decode_work_event(&headers, Some(&Body::Binary(vec![1, 2, 3]))),
        Err(WorkEventCodecError::NonJsonBody)
    ));
}

#[test]
fn workspace_headers_include_workspace_claim_card_and_repo() {
    let event = WorkEvent::WorkspaceRequested(WorkspaceRequested {
        workspace_id: WorkspaceId::from_u128(10),
        card_id: WorkCardId::from_u128(11),
        claim_id: ClaimId::from_u128(12),
        owner: PeerId::from_u128(13),
        repo: RepoId::new("CambrianTech/continuum").unwrap(),
        branch: BranchName::new("feat/rust-work").unwrap(),
        base: BranchName::new("rust-rewrite").unwrap(),
        requested_at_ms: 14,
    });

    let headers = work_event_headers(&event);

    assert_eq!(
        headers
            .get(HEADER_FORGE_WORK_WORKSPACE_ID)
            .map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000a")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CARD_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000b")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_CLAIM_ID).map(String::as_str),
        Some("00000000-0000-0000-0000-00000000000c")
    );
    assert_eq!(
        headers.get(HEADER_FORGE_WORK_REPO).map(String::as_str),
        Some("CambrianTech/continuum")
    );
}
