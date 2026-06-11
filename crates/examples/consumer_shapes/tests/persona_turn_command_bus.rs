//! Persona turn request/reply over the command bus — card ee2a339f
//! (persona-peer 3/8).
//!
//! Proves `TurnRequested` / `TurnEmitted` double as the command-bus
//! request/reply pair: the requester rides `Airc::request` (which
//! stamps `airc.correlation_id` + `airc.reply_to` + `airc.deadline`),
//! the persona-side responder decodes the typed turn, reads the
//! reply address back via `turn_reply_address`, and answers through
//! `reply_turn_emitted` so the requester's `await_reply` resolves
//! with the `TurnEmitted` body before the deadline. LAN-TCP with
//! separate homes so it cannot pass through shared local-fs.
//!
//! All waits bounded (d2ba719c): `await_reply` is deadline-bounded by
//! construction, and the responder task only runs while the requester
//! side is still in flight.

use std::net::SocketAddr;
use std::time::Duration;

use airc_lib::{Airc, AircError, MentionTarget};
use consumer_shapes::continuum::{
    decode_persona_event, reply_turn_emitted, request_turn, turn_reply_address, PersonaEvent,
    TurnEmitted, TurnRequested, BODY_HINT_FORGE_PERSONA_EVENT, HEADER_FORGE_PERSONA_MODEL_HINT,
};
use futures::StreamExt;
use tempfile::TempDir;

const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";

fn turn_request() -> TurnRequested {
    TurnRequested {
        persona_id: "clio".to_string(),
        activity_id: "activity-chat".to_string(),
        turn_id: "turn-007".to_string(),
        prompt: "take a correlated turn".to_string(),
        model_hint: Some("lora-clio-v3".to_string()),
        requested_at_ms: 1_700_000_000_000,
    }
}

fn turn_reply() -> TurnEmitted {
    TurnEmitted {
        persona_id: "clio".to_string(),
        activity_id: "activity-chat".to_string(),
        turn_id: "turn-007".to_string(),
        text: "correlated turn complete".to_string(),
        emitted_at_ms: 1_700_000_000_100,
    }
}

#[tokio::test]
async fn turn_request_reply_resolves_await_reply_before_deadline() {
    let requester_home = TempDir::new().expect("requester home");
    let persona_home = TempDir::new().expect("persona home");

    let requester = Airc::open(requester_home.path())
        .await
        .expect("requester opens");
    let persona = Airc::open(persona_home.path())
        .await
        .expect("persona opens");

    let requester_spec = requester.peer_spec().parse().expect("requester peer spec");
    let persona_spec = persona.peer_spec().parse().expect("persona peer spec");
    requester
        .add_peer(persona_spec)
        .await
        .expect("requester trusts persona");
    persona
        .add_peer(requester_spec)
        .await
        .expect("persona trusts requester");

    requester.join("persona-turn-bus").await.unwrap();
    persona.join("persona-turn-bus").await.unwrap();

    let persona_addr = persona
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("persona listens on LAN");
    requester
        .connect_lan(persona_addr, persona.peer_id())
        .await
        .expect("requester connects to persona over LAN");

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let responder = tokio::spawn(handle_one_turn_request(persona.clone(), ready_tx));
    ready_rx.await.expect("persona subscriber is ready");

    let pending = request_turn(
        &requester,
        MentionTarget::All,
        &turn_request(),
        Duration::from_secs(5),
    )
    .await
    .expect("turn request emits");

    // await_reply is bounded by the 5s deadline; resolving at all
    // proves the reply landed before it.
    let reply = requester
        .await_reply(pending)
        .await
        .expect("await_reply resolves with the TurnEmitted reply");
    let decoded = decode_persona_event(&reply.headers, reply.body.as_ref())
        .expect("reply decodes as a persona event");
    assert_eq!(decoded, PersonaEvent::TurnEmitted(turn_reply()));

    responder.await.expect("responder completes");
}

async fn handle_one_turn_request(persona: Airc, ready: tokio::sync::oneshot::Sender<()>) {
    let mut stream = persona.subscribe().await.unwrap();
    let _ = ready.send(());
    loop {
        let Some(Ok(event)) = stream.next().await else {
            continue;
        };
        if event.peer_id == persona.peer_id() {
            continue;
        }
        if event
            .headers
            .get(HEADER_FORGE_BODY_HINT)
            .map(String::as_str)
            != Some(BODY_HINT_FORGE_PERSONA_EVENT)
        {
            continue;
        }
        let PersonaEvent::TurnRequested(request) =
            decode_persona_event(&event.headers, event.body.as_ref()).unwrap()
        else {
            continue;
        };

        // The model hint rides as a filterable header alongside the body.
        assert_eq!(request.model_hint.as_deref(), Some("lora-clio-v3"));
        assert_eq!(
            event
                .headers
                .get(HEADER_FORGE_PERSONA_MODEL_HINT)
                .map(String::as_str),
            Some("lora-clio-v3"),
        );

        // The command bus stamped its correlation + reply-to + deadline
        // headers on the request; the typed view recovers all three.
        let address = turn_reply_address(&event.headers).unwrap();
        assert!(
            address.deadline_at_ms.is_some(),
            "Airc::request must stamp airc.deadline on the turn request",
        );

        reply_turn_emitted(&persona, &event.headers, &turn_reply())
            .await
            .expect("turn reply emits");
        return;
    }
}

#[tokio::test]
async fn turn_request_with_no_responder_errors_at_deadline() {
    let requester_home = TempDir::new().expect("requester home");
    let silent_home = TempDir::new().expect("silent peer home");

    let requester = Airc::open(requester_home.path())
        .await
        .expect("requester opens");
    // A live, trusted, connected peer that simply never replies —
    // the request emits fine, but no TurnEmitted ever comes back.
    let silent = Airc::open(silent_home.path()).await.expect("silent opens");

    let requester_spec = requester.peer_spec().parse().expect("requester peer spec");
    let silent_spec = silent.peer_spec().parse().expect("silent peer spec");
    requester
        .add_peer(silent_spec)
        .await
        .expect("requester trusts silent peer");
    silent
        .add_peer(requester_spec)
        .await
        .expect("silent peer trusts requester");

    requester.join("persona-turn-bus-silent").await.unwrap();
    silent.join("persona-turn-bus-silent").await.unwrap();

    let silent_addr = silent
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("silent peer listens on LAN");
    requester
        .connect_lan(silent_addr, silent.peer_id())
        .await
        .expect("requester connects to silent peer over LAN");

    let pending = request_turn(
        &requester,
        MentionTarget::All,
        &turn_request(),
        Duration::from_millis(300),
    )
    .await
    .expect("turn request emits");
    let correlation_id = pending.correlation_id;

    // Nobody is listening: await_reply must fail loudly AT the
    // deadline (bounded wait), not hang.
    let err = requester
        .await_reply(pending)
        .await
        .expect_err("await_reply must error once the deadline elapses");
    assert!(
        matches!(
            err,
            AircError::CommandDeadline {
                correlation_id: expired
            } if expired == correlation_id
        ),
        "expected CommandDeadline for {correlation_id}, got: {err:?}",
    );
}
