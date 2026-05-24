//! Consumer command-bus integration proof.
//!
//! Continuum, OpenClaw, and Hermes shaped payloads are not just codec
//! fixtures: they can ride the AIRC command-bus request/reply path
//! over a live transport. This test uses LAN-TCP with separate homes
//! so it cannot pass through shared local-fs or GitHub.

use std::net::SocketAddr;
use std::time::Duration;

use airc_lib::{Airc, Body, Headers, MentionTarget, PeerId, TranscriptEvent};
use consumer_shapes::continuum::{
    decode_persona_event, encode_persona_event, PersonaEvent, TurnEmitted, TurnRequested,
    BODY_HINT_FORGE_PERSONA_EVENT,
};
use consumer_shapes::hermes::{
    decode_hermes_event, encode_hermes_event, AgentCommandIssued, AgentResultReturned, HermesEvent,
    BODY_HINT_FORGE_HERMES_EVENT,
};
use consumer_shapes::openclaw::{
    decode_openclaw_event, encode_openclaw_event, ChatMessagePosted, OpenClawEvent, ThreadCreated,
    BODY_HINT_FORGE_OPENCLAW_EVENT,
};
use futures::StreamExt;
use tempfile::TempDir;
use uuid::Uuid;

const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";
const HEADER_AIRC_CORRELATION_ID: &str = "airc.correlation_id";
const HEADER_AIRC_REPLY_TO: &str = "airc.reply_to";

#[tokio::test]
async fn consumer_contracts_round_trip_over_lan_command_bus() {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec = bob.peer_spec().parse().expect("bob peer spec");
    alice.add_peer(bob_spec).await.expect("alice trusts bob");
    bob.add_peer(alice_spec).await.expect("bob trusts alice");

    alice.join("consumer-command-bus").await.unwrap();
    bob.join("consumer-command-bus").await.unwrap();

    let bob_addr = bob
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("bob listens on LAN");
    alice
        .connect_lan(bob_addr, bob.peer_id())
        .await
        .expect("alice connects to bob over LAN");

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let handler = tokio::spawn(handle_three_consumer_requests(bob.clone(), ready_tx));
    ready_rx.await.expect("bob subscriber is ready");

    assert_eq!(
        request_persona_turn(&alice).await,
        PersonaEvent::TurnEmitted(TurnEmitted {
            persona_id: "clio".to_string(),
            activity_id: "activity-chat".to_string(),
            turn_id: "turn-001".to_string(),
            text: "continuum turn complete".to_string(),
            emitted_at_ms: 1_700_000_000_100,
        })
    );
    assert_eq!(
        request_openclaw_thread(&alice).await,
        OpenClawEvent::ThreadCreated(ThreadCreated {
            openclaw_user_id: "u-agent".to_string(),
            openclaw_thread_id: "t-router-debug".to_string(),
            openclaw_workspace_id: "w-cambrian".to_string(),
            title: "AIRC routed thread".to_string(),
            created_at_ms: 1_700_000_000_200,
        })
    );
    assert_eq!(
        request_hermes_tool(&alice).await,
        HermesEvent::AgentResultReturned(AgentResultReturned {
            agent_id: "hermes-orchestrator".to_string(),
            command_id: "cmd-lora-001".to_string(),
            tool: "continuum.lora.invoke".to_string(),
            output: Some(serde_json::json!({"text": "lora invocation complete"})),
            error: None,
            returned_at_ms: 1_700_000_000_300,
        })
    );

    handler.await.expect("handler completes");
}

async fn handle_three_consumer_requests(bob: Airc, ready: tokio::sync::oneshot::Sender<()>) {
    let mut stream = bob.subscribe().await.unwrap();
    let _ = ready.send(());
    let mut handled = 0usize;
    while handled < 3 {
        let Some(Ok(event)) = stream.next().await else {
            continue;
        };
        if event.peer_id == bob.peer_id() {
            continue;
        }
        let Some(body_hint) = event
            .headers
            .get(HEADER_FORGE_BODY_HINT)
            .map(String::as_str)
        else {
            continue;
        };
        match body_hint {
            BODY_HINT_FORGE_PERSONA_EVENT => {
                let PersonaEvent::TurnRequested(request) =
                    decode_persona_event(&event.headers, event.body.as_ref()).unwrap()
                else {
                    continue;
                };
                let response = PersonaEvent::TurnEmitted(TurnEmitted {
                    persona_id: request.persona_id,
                    activity_id: request.activity_id,
                    turn_id: request.turn_id,
                    text: "continuum turn complete".to_string(),
                    emitted_at_ms: 1_700_000_000_100,
                });
                reply_with(&bob, &event, encode_persona_event(&response).unwrap()).await;
                handled += 1;
            }
            BODY_HINT_FORGE_OPENCLAW_EVENT => {
                let OpenClawEvent::ChatMessagePosted(request) =
                    decode_openclaw_event(&event.headers, event.body.as_ref()).unwrap()
                else {
                    continue;
                };
                let response = OpenClawEvent::ThreadCreated(ThreadCreated {
                    openclaw_user_id: "u-agent".to_string(),
                    openclaw_thread_id: request.openclaw_thread_id,
                    openclaw_workspace_id: request.openclaw_workspace_id,
                    title: "AIRC routed thread".to_string(),
                    created_at_ms: 1_700_000_000_200,
                });
                reply_with(&bob, &event, encode_openclaw_event(&response).unwrap()).await;
                handled += 1;
            }
            BODY_HINT_FORGE_HERMES_EVENT => {
                let HermesEvent::AgentCommandIssued(request) =
                    decode_hermes_event(&event.headers, event.body.as_ref()).unwrap()
                else {
                    continue;
                };
                let response = HermesEvent::AgentResultReturned(AgentResultReturned {
                    agent_id: request.agent_id,
                    command_id: request.command_id,
                    tool: request.tool,
                    output: Some(serde_json::json!({"text": "lora invocation complete"})),
                    error: None,
                    returned_at_ms: 1_700_000_000_300,
                });
                reply_with(&bob, &event, encode_hermes_event(&response).unwrap()).await;
                handled += 1;
            }
            _ => {}
        }
    }
}

async fn request_persona_turn(alice: &Airc) -> PersonaEvent {
    let request = PersonaEvent::TurnRequested(TurnRequested {
        persona_id: "clio".to_string(),
        activity_id: "activity-chat".to_string(),
        turn_id: "turn-001".to_string(),
        prompt: "take a turn".to_string(),
        requested_at_ms: 1_700_000_000_000,
    });
    let reply = request_typed(alice, encode_persona_event(&request).unwrap()).await;
    decode_persona_event(&reply.headers, reply.body.as_ref()).unwrap()
}

async fn request_openclaw_thread(alice: &Airc) -> OpenClawEvent {
    let request = OpenClawEvent::ChatMessagePosted(ChatMessagePosted {
        openclaw_user_id: "u-human".to_string(),
        openclaw_thread_id: "t-router-debug".to_string(),
        openclaw_workspace_id: "w-cambrian".to_string(),
        text: "please create a routed thread".to_string(),
        posted_at_ms: 1_700_000_000_010,
    });
    let reply = request_typed(alice, encode_openclaw_event(&request).unwrap()).await;
    decode_openclaw_event(&reply.headers, reply.body.as_ref()).unwrap()
}

async fn request_hermes_tool(alice: &Airc) -> HermesEvent {
    let request = HermesEvent::AgentCommandIssued(AgentCommandIssued {
        agent_id: "hermes-orchestrator".to_string(),
        command_id: "cmd-lora-001".to_string(),
        tool: "continuum.lora.invoke".to_string(),
        input: serde_json::json!({"adapter_id": "lora-clio", "prompt": "summarize"}),
        issued_at_ms: 1_700_000_000_020,
    });
    let reply = request_typed(alice, encode_hermes_event(&request).unwrap()).await;
    decode_hermes_event(&reply.headers, reply.body.as_ref()).unwrap()
}

async fn request_typed(alice: &Airc, payload: (Headers, Body)) -> TranscriptEvent {
    let (headers, body) = payload;
    let pending = alice
        .request(MentionTarget::All, headers, body, Duration::from_secs(3))
        .await
        .expect("request emits");
    alice.await_reply(pending).await.expect("reply returns")
}

async fn reply_with(bob: &Airc, request: &TranscriptEvent, payload: (Headers, Body)) {
    let (headers, body) = payload;
    let reply_to_peer = request_peer(request);
    let correlation_id = request_correlation(request);
    bob.reply(reply_to_peer, correlation_id, headers, body)
        .await
        .expect("reply emits");
}

fn request_peer(event: &TranscriptEvent) -> PeerId {
    let value = event
        .headers
        .get(HEADER_AIRC_REPLY_TO)
        .expect("request carries reply peer");
    PeerId::from_uuid(Uuid::parse_str(value).expect("reply peer is uuid"))
}

fn request_correlation(event: &TranscriptEvent) -> Uuid {
    let value = event
        .headers
        .get(HEADER_AIRC_CORRELATION_ID)
        .expect("request carries correlation id");
    Uuid::parse_str(value).expect("correlation is uuid")
}
