//! Continuum persona/activity contract fixtures.
//!
//! Asserts the codec produces the right header projection, the body
//! roundtrips per variant, and the supplied filters admit/reject
//! events as expected. These fixtures are the source-of-truth for
//! the on-wire shape — any change here must coordinate with the
//! Continuum consumer that subscribes against the same filters.

use airc_lib::{Body, Headers};
use consumer_shapes::continuum::{
    activity_event_filter, any_persona_event_filter, decode_persona_event, encode_persona_event,
    ActivityEnded, ActivityStarted, PersonaCodecError, PersonaEvent, TurnEmitted, TurnRequested,
    BODY_HINT_FORGE_PERSONA_EVENT, HEADER_FORGE_CONTINUUM_ACTIVITY_ID,
    HEADER_FORGE_CONTINUUM_TURN_ID, HEADER_FORGE_PERSONA_ID, HEADER_FORGE_PERSONA_KIND,
};

fn turn_requested() -> PersonaEvent {
    PersonaEvent::TurnRequested(TurnRequested {
        persona_id: "skylar".to_string(),
        activity_id: "session-42".to_string(),
        turn_id: "turn-1".to_string(),
        prompt: "what's the meta-goal?".to_string(),
        requested_at_ms: 1_700_000_000_000,
    })
}

fn turn_emitted() -> PersonaEvent {
    PersonaEvent::TurnEmitted(TurnEmitted {
        persona_id: "skylar".to_string(),
        activity_id: "session-42".to_string(),
        turn_id: "turn-1".to_string(),
        text: "ship the substrate".to_string(),
        emitted_at_ms: 1_700_000_000_500,
    })
}

#[test]
fn roundtrip_all_variants_preserves_typed_event() {
    let cases: Vec<PersonaEvent> = vec![
        turn_requested(),
        turn_emitted(),
        PersonaEvent::ActivityStarted(ActivityStarted {
            persona_id: "skylar".to_string(),
            activity_id: "session-42".to_string(),
            label: "morning standup".to_string(),
            started_at_ms: 0,
        }),
        PersonaEvent::ActivityEnded(ActivityEnded {
            persona_id: "skylar".to_string(),
            activity_id: "session-42".to_string(),
            ended_at_ms: 3_600_000,
        }),
    ];
    for event in cases {
        let (headers, body) = encode_persona_event(&event).unwrap();
        let decoded = decode_persona_event(&headers, Some(&body)).unwrap();
        assert_eq!(decoded, event, "roundtrip diverged for {event:?}");
    }
}

#[test]
fn headers_project_persona_and_activity_for_cheap_filtering() {
    let (headers, _) = encode_persona_event(&turn_requested()).unwrap();
    assert_eq!(
        headers.get("forge.body_hint").map(String::as_str),
        Some(BODY_HINT_FORGE_PERSONA_EVENT),
    );
    assert_eq!(
        headers.get(HEADER_FORGE_PERSONA_KIND).map(String::as_str),
        Some("turn_requested"),
    );
    assert_eq!(
        headers.get(HEADER_FORGE_PERSONA_ID).map(String::as_str),
        Some("skylar"),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_CONTINUUM_ACTIVITY_ID)
            .map(String::as_str),
        Some("session-42"),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_CONTINUUM_TURN_ID)
            .map(String::as_str),
        Some("turn-1"),
    );
}

#[test]
fn activity_filter_admits_matching_and_rejects_unrelated() {
    let (matching_headers, _) = encode_persona_event(&turn_requested()).unwrap();
    let filter = activity_event_filter("session-42");
    assert!(
        filter.headers_filter.matches(&matching_headers),
        "filter must admit events for the named activity",
    );

    let off_activity = PersonaEvent::TurnRequested(TurnRequested {
        persona_id: "skylar".to_string(),
        activity_id: "other-activity".to_string(),
        turn_id: "turn-9".to_string(),
        prompt: "unrelated".to_string(),
        requested_at_ms: 0,
    });
    let (off_headers, _) = encode_persona_event(&off_activity).unwrap();
    assert!(
        !filter.headers_filter.matches(&off_headers),
        "filter must reject events for a different activity",
    );

    let any_filter = any_persona_event_filter();
    assert!(any_filter.headers_filter.matches(&matching_headers));
    assert!(any_filter.headers_filter.matches(&off_headers));
}

#[test]
fn decode_rejects_wrong_body_hint() {
    let (mut headers, body) = encode_persona_event(&turn_requested()).unwrap();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.work.event.v1".to_string(),
    );
    let err = decode_persona_event(&headers, Some(&body)).unwrap_err();
    assert!(matches!(err, PersonaCodecError::BodyHintMismatch { .. }));
}

#[test]
fn decode_rejects_missing_body() {
    let (headers, _) = encode_persona_event(&turn_requested()).unwrap();
    let err = decode_persona_event(&headers, None).unwrap_err();
    assert!(matches!(err, PersonaCodecError::MissingBody));
}

#[test]
fn decode_rejects_non_json_body() {
    let (headers, _) = encode_persona_event(&turn_requested()).unwrap();
    let err = decode_persona_event(&headers, Some(&Body::Binary(vec![0x01, 0x02]))).unwrap_err();
    assert!(matches!(err, PersonaCodecError::NonJsonBody));
}

#[test]
fn empty_headers_have_no_body_hint_so_decode_errors_loudly() {
    let headers = Headers::new();
    let err = decode_persona_event(&headers, None).unwrap_err();
    assert!(matches!(
        err,
        PersonaCodecError::BodyHintMismatch { actual: None, .. }
    ));
}
