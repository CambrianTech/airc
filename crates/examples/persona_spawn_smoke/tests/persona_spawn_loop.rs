//! Card 98bf179b — the persona-spawn loop, proven end to end.
//!
//! Two real `Airc` handles in tempdirs (the `stored_endpoint_dial.rs`
//! style): a PARENT that requests a turn and a spawned PERSONA that
//! serves it. The persona side runs through `PersonaAgent::spawn` —
//! the same code path the `persona-spawn-smoke` binary wires from the
//! environment — so the test proves the worked example, not a
//! parallel implementation.
//!
//! Loop contract proven here:
//!   - the persona opens its OWN home (own PeerId/identity.key),
//!     advertises `PersonaCapabilities` on its identity card via the
//!     card-9e5f8844 typed accessor, enrols the parent, and pins it
//!     at `TrustTier::OwnMachine`;
//!   - parent sends `TurnRequested`; persona answers `TurnEmitted`
//!     with the same activity/turn ids; parent receives it over a
//!     real loopback LAN-TCP route.

use std::net::SocketAddr;
use std::time::Duration;

use airc_core::PersonaCapabilities;
use airc_lib::{Airc, PeerSpec};
use airc_trust::TrustTier;
use consumer_shapes::continuum::{
    any_persona_event_filter, decode_persona_event, encode_persona_event, PersonaEvent,
    TurnRequested,
};
use futures::StreamExt;
use persona_spawn_smoke::persona_agent::{now_ms, PersonaAgent, PersonaAgentConfig};
use tempfile::TempDir;

fn smoke_capabilities() -> PersonaCapabilities {
    PersonaCapabilities {
        persona_id: "skylar-smoke".to_string(),
        capability_tags: vec!["echo".to_string(), "smoke".to_string()],
        model: "example-echo".to_string(),
        context_window_tokens: 8_192,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn parent_requests_turn_and_spawned_persona_replies() {
    let room = "persona-smoke";
    let parent_tmp = TempDir::new().expect("parent tempdir");
    let persona_tmp = TempDir::new().expect("persona tempdir");

    // Parent peer: joins the persona room and listens on loopback so
    // the spawned persona has something to dial.
    let parent = Airc::open(parent_tmp.path().join(".airc"))
        .await
        .expect("parent open");
    parent.join(room).await.expect("parent joins room");
    let parent_addr: SocketAddr = parent
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("parent listens");

    // Spawn the persona through the worked-example surface — exactly
    // what the binary does with AIRC_HOME + AIRC_PARENT_PEER_SPEC.
    let config = PersonaAgentConfig {
        home: persona_tmp.path().join(".airc"),
        parent_spec: parent.peer_spec(),
        room: room.to_string(),
        capabilities: smoke_capabilities(),
        parent_lan_addr: Some(parent_addr),
    };
    let mut persona = PersonaAgent::spawn(config).await.expect("persona spawn");

    // The persona's identity card carries the typed capabilities on
    // the EXISTING integrations map (card 9e5f8844) — decode it back
    // through the loud typed accessor.
    let card = persona.identity_card().expect("persona identity card");
    let advertised = PersonaCapabilities::read_from_identity(&card)
        .expect("capabilities decode must not fail")
        .expect("capabilities must be present on a persona identity");
    assert_eq!(advertised, smoke_capabilities());

    // Spawn handshake, parent side: the parent enrols the persona's
    // reported spec (the binary prints it on stdout for this).
    let persona_spec: PeerSpec = persona.peer_spec().parse().expect("persona spec");
    parent
        .add_peer(persona_spec)
        .await
        .expect("parent enrols persona");

    // The persona pinned its parent at OwnMachine during spawn — the
    // spawn relationship IS the same-machine relationship.
    let parent_record = airc_trust::load(persona.airc().home())
        .await
        .expect("read persona trust store")
        .into_iter()
        .find(|peer| peer.peer_id == persona.parent_peer_id())
        .expect("parent must be enrolled on the persona");
    assert_eq!(parent_record.tier, TrustTier::OwnMachine);

    // Route: persona dials the parent's loopback listener (what the
    // binary does when AIRC_PARENT_LAN_ADDR is set).
    persona
        .connect_parent_lan(parent_addr)
        .await
        .expect("persona dials parent");

    // The inbound link registers on the parent's adapter a beat after
    // the persona's connect resolves — wait for it before sending, the
    // same way a production spawn loop waits for the child to dial in.
    let link_deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = parent
            .route_discovery_snapshot()
            .await
            .expect("parent route snapshot");
        if snapshot
            .connected_lan_peers
            .contains(&persona.airc().peer_id())
        {
            break;
        }
        assert!(
            std::time::Instant::now() < link_deadline,
            "persona's inbound LAN link never registered on the parent; \
             connected: {:?}",
            snapshot.connected_lan_peers,
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }

    // Parent subscribes for persona events BEFORE requesting the turn
    // so the reply cannot race the subscription.
    let mut parent_events = parent
        .subscribe_filtered(any_persona_event_filter())
        .await
        .expect("parent subscribes");

    let request = TurnRequested {
        persona_id: "skylar-smoke".to_string(),
        activity_id: "activity-1".to_string(),
        turn_id: "turn-1".to_string(),
        prompt: "what's the meta-goal?".to_string(),
        requested_at_ms: now_ms().expect("clock"),
    };
    let (headers, body) =
        encode_persona_event(&PersonaEvent::TurnRequested(request.clone())).expect("encode");
    parent
        .send(body, headers)
        .await
        .expect("parent sends TurnRequested");

    // Persona side: serve exactly one turn. The subscription was
    // installed during spawn, so the request is already buffered even
    // though we only poll now.
    let served = persona
        .serve_next_turn(Duration::from_secs(10))
        .await
        .expect("persona turn loop must not error")
        .expect("persona must see the TurnRequested within the deadline");
    assert_eq!(served.persona_id, "skylar-smoke");
    assert_eq!(served.activity_id, request.activity_id);
    assert_eq!(served.turn_id, request.turn_id);
    assert_eq!(served.text, "echo: what's the meta-goal?");

    // Parent side: the TurnEmitted arrives over the wire, decodes
    // through the shared codec, and correlates by activity/turn id.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    let received = loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        assert!(
            remaining > Duration::ZERO,
            "parent never received TurnEmitted within the deadline"
        );
        let event = tokio::time::timeout(remaining, parent_events.next())
            .await
            .expect("parent subscription stalled past the deadline")
            .expect("parent subscription closed before the reply")
            .expect("parent subscription lagged");
        // Skip the parent's own TurnRequested echo.
        if event.peer_id == parent.peer_id() && event.client_id == parent.client_id() {
            continue;
        }
        let decoded =
            decode_persona_event(&event.headers, event.body.as_ref()).expect("decode reply");
        match decoded {
            PersonaEvent::TurnEmitted(emitted) => break emitted,
            PersonaEvent::TurnRequested(_)
            | PersonaEvent::ActivityStarted(_)
            | PersonaEvent::ActivityEnded(_) => continue,
        }
    };

    assert_eq!(received.persona_id, "skylar-smoke");
    assert_eq!(received.activity_id, request.activity_id);
    assert_eq!(received.turn_id, request.turn_id);
    assert_eq!(received.text, "echo: what's the meta-goal?");
    assert_eq!(
        received, served,
        "the reply the parent received must be byte-for-byte the one the persona sent",
    );
}
