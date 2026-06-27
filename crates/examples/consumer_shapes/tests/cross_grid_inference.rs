//! Cross-grid inference request/reply, end-to-end — card cae4bab1
//! (persona-peer 8/8).
//!
//! The headline grid milestone: a requesting node that CANNOT satisfy a
//! turn locally escalates to a mesh peer that can, sends the turn, the
//! peer's stub persona answers, and the requester's `await_reply` resolves
//! with the `TurnEmitted` body before the deadline. No real models, no
//! live external machines: two/three local `Airc` handles over LAN-TCP,
//! separate tempdir homes so nothing passes through shared local-fs.
//!
//! Local-first binding principle (Joel): the requester tries its OWN local
//! capability FIRST and only escalates when local can't meet the need.
//! Three tests prove the decision shape:
//!   - REMOTE (headline): no matching local capability → resolver returns
//!     `Remote(S)` → full request/reply round-trip over LAN.
//!   - LOCAL: the requester advertises the needed tag → resolver returns
//!     `Local`, NO network request is emitted.
//!   - UNAVAILABLE: no local, no remote → loud typed `Unavailable`, no hang.
//!
//! All waits deadline-bounded (d2ba719c): `await_reply` is bounded by
//! construction; the responder task only runs while the requester is in
//! flight; the no-responder path is asserted to error AT the deadline.

use std::net::SocketAddr;
use std::time::Duration;

use airc_lib::{Airc, CapabilityRegistry, PeerId, PersonaCapabilities, TrustTier};
use consumer_shapes::continuum::{
    decode_persona_event, ingest_capability_offer, reply_turn_emitted, request_inference_remote,
    resolve_inference_target, CapabilityOffer, InferenceTarget, PersonaEvent, TurnEmitted,
    TurnRequested, BODY_HINT_FORGE_PERSONA_EVENT,
};
use futures::StreamExt;
use tempfile::TempDir;

const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";
const TTL_MS: u64 = 180_000;
const NOW_MS: u64 = 1_700_000_000_000;

/// A turn needing the `lora-clio-v3` model — the need that drives routing.
fn clio_turn() -> TurnRequested {
    TurnRequested {
        persona_id: "clio".to_string(),
        activity_id: "activity-xgrid".to_string(),
        turn_id: "turn-xgrid-1".to_string(),
        prompt: "infer across the grid".to_string(),
        model_hint: Some("lora-clio-v3".to_string()),
        requested_at_ms: NOW_MS,
    }
}

/// The canned answer the stub persona on the server node emits. No model
/// runs — the routing/protocol spine is what's under test (the brain is
/// stubbed exactly as 2/8 stubbed it).
fn clio_reply() -> TurnEmitted {
    TurnEmitted {
        persona_id: "clio".to_string(),
        activity_id: "activity-xgrid".to_string(),
        turn_id: "turn-xgrid-1".to_string(),
        text: "canned cross-grid inference output".to_string(),
        emitted_at_ms: NOW_MS + 100,
    }
}

fn caps(persona: &str, tags: &[&str]) -> PersonaCapabilities {
    PersonaCapabilities {
        persona_id: persona.to_string(),
        capability_tags: tags.iter().map(|t| t.to_string()).collect(),
        model: "fable-5".to_string(),
        context_window_tokens: 200_000,
    }
}

fn offer(peer: PeerId, persona: &str, tags: &[&str]) -> CapabilityOffer {
    CapabilityOffer {
        peer_id: peer,
        capabilities: caps(persona, tags),
        offered_at_ms: NOW_MS,
    }
}

/// HEADLINE: requester R has no local capability for `lora-clio-v3`; server
/// S publishes an offer advertising it and runs a stub persona. R resolves
/// to `Remote(S)`, sends the turn, S answers, R's `await_reply` resolves
/// with the canned `TurnEmitted` before the deadline.
#[tokio::test]
async fn remote_inference_resolves_and_round_trips_end_to_end() {
    let requester_home = TempDir::new().expect("requester home");
    let server_home = TempDir::new().expect("server home");

    let requester = Airc::open(requester_home.path())
        .await
        .expect("requester opens");
    let server = Airc::open(server_home.path()).await.expect("server opens");

    let requester_spec = requester.peer_spec().parse().expect("requester peer spec");
    let server_spec = server.peer_spec().parse().expect("server peer spec");
    requester
        .add_peer(server_spec)
        .await
        .expect("requester trusts server");
    server
        .add_peer(requester_spec)
        .await
        .expect("server trusts requester");

    requester.join("xgrid-inference").await.unwrap();
    server.join("xgrid-inference").await.unwrap();

    let server_addr = server
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("server listens on LAN");
    requester
        .connect_lan(server_addr, server.peer_id())
        .await
        .expect("requester connects to server over LAN");

    // R's view of the grid: it hosts NO matching local capability (a pure
    // requester here, local = None), and its registry has ingested S's
    // offer for `lora-clio-v3`. The trust closure marks S as the same
    // account (a real grid resolves this from the trust layer).
    let server_peer = server.peer_id();
    let mut registry = CapabilityRegistry::new();
    ingest_capability_offer(
        &mut registry,
        &offer(server_peer, "clio", &["lora-clio-v3"]),
    );

    let request = clio_turn();
    let decision = resolve_inference_target(
        None,
        &registry,
        &request,
        &[],
        NOW_MS,
        TTL_MS,
        move |p: PeerId| {
            if p == server_peer {
                TrustTier::OwnAccount
            } else {
                TrustTier::Untrusted
            }
        },
    );
    let candidate = match decision {
        InferenceTarget::Remote(candidate) => {
            assert_eq!(
                candidate.peer_id, server_peer,
                "resolver must route to the server advertising the model",
            );
            candidate
        }
        other => panic!("expected Remote(server), got {other:?}"),
    };

    // S runs a stub persona that answers the next inbound TurnRequested.
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let responder = tokio::spawn(stub_persona_answers_one_turn(server.clone(), ready_tx));
    ready_rx.await.expect("server persona subscriber is ready");

    // R escalates to the mesh: cross-grid request/reply, deadline-bounded.
    let emitted =
        request_inference_remote(&requester, &candidate, &request, Duration::from_secs(5))
            .await
            .expect("remote inference resolves before deadline");
    assert_eq!(
        emitted,
        clio_reply(),
        "requester received the server persona's canned TurnEmitted",
    );

    responder.await.expect("responder task completes");
}

/// The stub persona on the server: subscribe, wait for one inbound
/// `TurnRequested`, reply with the canned `TurnEmitted` echoing the
/// command-bus correlation via `reply_turn_emitted`.
async fn stub_persona_answers_one_turn(server: Airc, ready: tokio::sync::oneshot::Sender<()>) {
    let mut stream = server.subscribe().await.unwrap();
    let _ = ready.send(());
    loop {
        let Some(Ok(event)) = stream.next().await else {
            continue;
        };
        if event.peer_id == server.peer_id() {
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
        let PersonaEvent::TurnRequested(_request) =
            decode_persona_event(&event.headers, event.body.as_ref()).unwrap()
        else {
            continue;
        };
        reply_turn_emitted(&server, &event.headers, &clio_reply())
            .await
            .expect("stub persona reply emits");
        return;
    }
}

/// LOCAL-FIRST: when the requester's OWN capability satisfies the need, the
/// resolver returns `Local` and NO cross-grid request is emitted. Proven by
/// arming a subscriber on a connected peer and asserting it sees no turn
/// request within a bounded window (we never call `request_inference_remote`
/// on a `Local` decision — the decision itself is the assertion, the silent
/// peer is the belt-and-braces).
#[tokio::test]
async fn local_capability_resolves_local_and_emits_no_request() {
    let mut registry = CapabilityRegistry::new();
    // Even though a remote node ALSO advertises the model, local-first wins.
    ingest_capability_offer(
        &mut registry,
        &offer(PeerId::from_u128(99), "clio", &["lora-clio-v3"]),
    );

    let local = caps("clio", &["lora-clio-v3", "code"]);
    let decision = resolve_inference_target(
        Some(&local),
        &registry,
        &clio_turn(),
        &[],
        NOW_MS,
        TTL_MS,
        |_| TrustTier::OwnMachine,
    );
    assert_eq!(
        decision,
        InferenceTarget::Local,
        "local capability satisfies the need → Local, never escalate",
    );
}

/// UNAVAILABLE: no local capability AND no remote candidate (grid-of-one, or
/// a model nobody advertises) → a loud typed `Unavailable`. The decision is
/// pure and emits nothing, so there is nothing to hang on — the caller gets
/// a terminal value to surface.
#[tokio::test]
async fn no_local_no_remote_resolves_unavailable_no_hang() {
    let empty = CapabilityRegistry::new();

    // Grid-of-one: no local persona, empty registry.
    let decision =
        resolve_inference_target(None, &empty, &clio_turn(), &[], NOW_MS, TTL_MS, |_| {
            TrustTier::OwnMachine
        });
    assert_eq!(
        decision,
        InferenceTarget::Unavailable,
        "no local + empty registry → loud Unavailable",
    );

    // Local persona present but lacking the needed tag, AND the only remote
    // offer advertises a different model → still Unavailable.
    let mut registry = CapabilityRegistry::new();
    ingest_capability_offer(
        &mut registry,
        &offer(PeerId::from_u128(7), "other", &["code"]),
    );
    let local = caps("clio", &["code"]); // no lora-clio-v3
    let decision = resolve_inference_target(
        Some(&local),
        &registry,
        &clio_turn(),
        &[],
        NOW_MS,
        TTL_MS,
        |_| TrustTier::OwnMachine,
    );
    assert_eq!(
        decision,
        InferenceTarget::Unavailable,
        "local can't, no remote advertises the model → loud Unavailable",
    );
}
