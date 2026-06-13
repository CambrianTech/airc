//! Hermes agent command/event contract fixtures.

use consumer_shapes::hermes::{
    agent_event_filter, any_hermes_event_filter, decode_hermes_event, encode_hermes_event,
    AgentCommandIssued, AgentResultReturned, HermesCodecError, HermesEvent,
    BODY_HINT_FORGE_HERMES_EVENT, HEADER_FORGE_HERMES_AGENT_ID, HEADER_FORGE_HERMES_COMMAND_ID,
    HEADER_FORGE_HERMES_KIND, HEADER_FORGE_HERMES_TOOL,
};

fn command_issued() -> HermesEvent {
    HermesEvent::AgentCommandIssued(AgentCommandIssued {
        agent_id: "agent-orion".to_string(),
        command_id: "cmd-001".to_string(),
        tool: "fs.read".to_string(),
        input: serde_json::json!({ "path": "/etc/hostname" }),
        issued_at_ms: 1_700_000_000_000,
    })
}

fn result_returned() -> HermesEvent {
    HermesEvent::AgentResultReturned(AgentResultReturned {
        agent_id: "agent-orion".to_string(),
        command_id: "cmd-001".to_string(),
        tool: "fs.read".to_string(),
        output: Some(serde_json::json!({ "content": "joelteply-laptop" })),
        error: None,
        returned_at_ms: 1_700_000_000_300,
    })
}

#[test]
fn roundtrip_command_and_result_preserves_typed_event() {
    for event in [command_issued(), result_returned()] {
        let (headers, body) = encode_hermes_event(&event).unwrap();
        let decoded = decode_hermes_event(&headers, Some(&body)).unwrap();
        assert_eq!(decoded, event);
    }
}

#[test]
fn partial_success_is_first_class_in_result() {
    // "It worked" without specifics is not acceptable. A result that
    // produced partial output AND hit an error must serialize both.
    let partial = HermesEvent::AgentResultReturned(AgentResultReturned {
        agent_id: "agent-orion".to_string(),
        command_id: "cmd-002".to_string(),
        tool: "fs.read".to_string(),
        output: Some(serde_json::json!({ "content": "partial-data" })),
        error: Some("EOF before complete read".to_string()),
        returned_at_ms: 0,
    });
    let (headers, body) = encode_hermes_event(&partial).unwrap();
    let decoded = decode_hermes_event(&headers, Some(&body)).unwrap();
    assert_eq!(decoded, partial);
    match decoded {
        HermesEvent::AgentResultReturned(r) => {
            assert!(r.output.is_some(), "partial output preserved");
            assert!(r.error.is_some(), "partial error preserved");
        }
        _ => panic!("variant changed during roundtrip"),
    }
}

#[test]
fn headers_project_agent_command_tool() {
    let (headers, _) = encode_hermes_event(&command_issued()).unwrap();
    assert_eq!(
        headers.get("forge.body_hint").map(String::as_str),
        Some(BODY_HINT_FORGE_HERMES_EVENT),
    );
    assert_eq!(
        headers.get(HEADER_FORGE_HERMES_KIND).map(String::as_str),
        Some("agent_command_issued"),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_HERMES_AGENT_ID)
            .map(String::as_str),
        Some("agent-orion"),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_HERMES_COMMAND_ID)
            .map(String::as_str),
        Some("cmd-001"),
    );
    assert_eq!(
        headers.get(HEADER_FORGE_HERMES_TOOL).map(String::as_str),
        Some("fs.read"),
    );
}

#[test]
fn agent_filter_admits_matching_agent_and_rejects_other_agents() {
    let (orion_cmd_headers, _) = encode_hermes_event(&command_issued()).unwrap();
    let (orion_res_headers, _) = encode_hermes_event(&result_returned()).unwrap();
    let filter = agent_event_filter("agent-orion");

    // Both events for orion match — the full command lifecycle.
    assert!(filter.headers_filter.matches(&orion_cmd_headers));
    assert!(filter.headers_filter.matches(&orion_res_headers));

    let other_agent = HermesEvent::AgentCommandIssued(AgentCommandIssued {
        agent_id: "agent-rigel".to_string(),
        command_id: "cmd-999".to_string(),
        tool: "fs.read".to_string(),
        input: serde_json::json!({}),
        issued_at_ms: 0,
    });
    let (other_headers, _) = encode_hermes_event(&other_agent).unwrap();
    assert!(!filter.headers_filter.matches(&other_headers));

    let any_filter = any_hermes_event_filter();
    assert!(any_filter.headers_filter.matches(&orion_cmd_headers));
    assert!(any_filter.headers_filter.matches(&other_headers));
}

#[test]
fn decode_rejects_wrong_body_hint() {
    let (mut headers, body) = encode_hermes_event(&command_issued()).unwrap();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.persona.event.v1".to_string(),
    );
    let err = decode_hermes_event(&headers, Some(&body)).unwrap_err();
    assert!(matches!(err, HermesCodecError::BodyHintMismatch { .. }));
}
