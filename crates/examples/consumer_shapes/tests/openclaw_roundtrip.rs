//! OpenClaw chat/thread identity contract fixtures.

use airc_lib::Body;
use consumer_shapes::openclaw::{
    any_openclaw_event_filter, decode_openclaw_event, encode_openclaw_event,
    workspace_event_filter, ChatMessagePosted, OpenClawCodecError, OpenClawEvent, ThreadCreated,
    BODY_HINT_FORGE_OPENCLAW_EVENT, HEADER_FORGE_OPENCLAW_THREAD_ID, HEADER_FORGE_OPENCLAW_USER_ID,
    HEADER_FORGE_OPENCLAW_WORKSPACE_ID,
};

fn chat_event() -> OpenClawEvent {
    OpenClawEvent::ChatMessagePosted(ChatMessagePosted {
        openclaw_user_id: "u-alice".to_string(),
        openclaw_thread_id: "t-router-debug".to_string(),
        openclaw_workspace_id: "w-acme".to_string(),
        text: "PR-G CI is green".to_string(),
        posted_at_ms: 1_700_000_000_000,
    })
}

#[test]
fn roundtrip_all_variants_preserves_typed_event() {
    let cases: Vec<OpenClawEvent> = vec![
        chat_event(),
        OpenClawEvent::ThreadCreated(ThreadCreated {
            openclaw_user_id: "u-alice".to_string(),
            openclaw_thread_id: "t-new".to_string(),
            openclaw_workspace_id: "w-acme".to_string(),
            title: "release readout".to_string(),
            created_at_ms: 0,
        }),
    ];
    for event in cases {
        let (headers, body) = encode_openclaw_event(&event).unwrap();
        let decoded = decode_openclaw_event(&headers, Some(&body)).unwrap();
        assert_eq!(decoded, event);
    }
}

#[test]
fn headers_project_user_thread_workspace_for_filtering() {
    let (headers, _) = encode_openclaw_event(&chat_event()).unwrap();
    assert_eq!(
        headers.get("forge.body_hint").map(String::as_str),
        Some(BODY_HINT_FORGE_OPENCLAW_EVENT),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_OPENCLAW_USER_ID)
            .map(String::as_str),
        Some("u-alice"),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_OPENCLAW_THREAD_ID)
            .map(String::as_str),
        Some("t-router-debug"),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_OPENCLAW_WORKSPACE_ID)
            .map(String::as_str),
        Some("w-acme"),
    );
}

#[test]
fn workspace_filter_admits_matching_and_rejects_other_workspaces() {
    let (matching_headers, _) = encode_openclaw_event(&chat_event()).unwrap();
    let filter = workspace_event_filter("w-acme");
    assert!(filter.headers_filter.matches(&matching_headers));

    let other_ws = OpenClawEvent::ChatMessagePosted(ChatMessagePosted {
        openclaw_user_id: "u-bob".to_string(),
        openclaw_thread_id: "t-other".to_string(),
        openclaw_workspace_id: "w-different".to_string(),
        text: "wrong workspace".to_string(),
        posted_at_ms: 0,
    });
    let (other_headers, _) = encode_openclaw_event(&other_ws).unwrap();
    assert!(!filter.headers_filter.matches(&other_headers));

    let any_filter = any_openclaw_event_filter();
    assert!(any_filter.headers_filter.matches(&matching_headers));
    assert!(any_filter.headers_filter.matches(&other_headers));
}

#[test]
fn decode_rejects_wrong_body_hint() {
    let (mut headers, body) = encode_openclaw_event(&chat_event()).unwrap();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.work.event.v1".to_string(),
    );
    let err = decode_openclaw_event(&headers, Some(&body)).unwrap_err();
    assert!(matches!(err, OpenClawCodecError::BodyHintMismatch { .. }));
}

#[test]
fn decode_rejects_non_json_body() {
    let (headers, _) = encode_openclaw_event(&chat_event()).unwrap();
    let err = decode_openclaw_event(&headers, Some(&Body::Binary(vec![1, 2, 3]))).unwrap_err();
    assert!(matches!(err, OpenClawCodecError::NonJsonBody));
}
