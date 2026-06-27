//! `ai/embedding` request/reply wire contract — the BigMama 5090 facility
//! (airc PR #1239 / slice 2b).
//!
//! Asserts the embedding codec projects the right headers and roundtrips, the
//! filter admits only embedding frames, and `EmbeddingRequested` /
//! `EmbeddingEmitted` ride the command bus as a request/reply pair (the same
//! carriage `TurnRequested`/`TurnEmitted` use) so slice 3's
//! `GridEmbeddingProvider` can `request_embedding_remote` against the facility
//! and get its vectors back before the deadline. These fixtures are the
//! source-of-truth for the embedding wire shape — any change coordinates with
//! the facility bridge AND the continuum consumer that share the contract.
//!
//! "identity is the model, transport is the policy": the model SLUG rides as a
//! filterable header so a responder refuses a wrong-space request without
//! decoding the body.

use std::net::SocketAddr;
use std::time::Duration;

use airc_lib::{Airc, Headers, MentionTarget};
use consumer_shapes::continuum::{
    any_embedding_event_filter, decode_embedding_event, encode_embedding_event,
    reply_embedding_emitted, request_embedding, EmbeddingEmitted, EmbeddingEvent,
    EmbeddingRequested, PersonaCodecError, BODY_HINT_FORGE_AI_EMBEDDING,
    HEADER_FORGE_AI_EMBEDDING_KIND, HEADER_FORGE_AI_EMBEDDING_MODEL,
    HEADER_FORGE_AI_EMBEDDING_REQUEST_ID,
};
use futures::StreamExt;
use tempfile::TempDir;

const MODEL: &str = "qwen3-embedding-0.6b";

fn request() -> EmbeddingRequested {
    EmbeddingRequested {
        request_id: "req-1".to_string(),
        model: MODEL.to_string(),
        inputs: vec!["hello grid".to_string(), "second input".to_string()],
        requested_at_ms: 1_700_000_000_000,
    }
}

fn emitted() -> EmbeddingEmitted {
    EmbeddingEmitted {
        request_id: "req-1".to_string(),
        model: MODEL.to_string(),
        vectors: vec![vec![0.1, 0.2, 0.3], vec![0.4, 0.5, 0.6]],
        dim: 3,
        emitted_at_ms: 1_700_000_000_100,
    }
}

#[test]
fn request_roundtrips_and_projects_headers() {
    let (headers, body) = encode_embedding_event(&EmbeddingEvent::Requested(request())).unwrap();

    assert_eq!(
        headers.get("forge.body_hint").map(String::as_str),
        Some(BODY_HINT_FORGE_AI_EMBEDDING),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_AI_EMBEDDING_KIND)
            .map(String::as_str),
        Some("requested"),
    );
    // The model slug rides on the envelope — the vector-space identity, the
    // same slug as the routing URI fragment and the cache provider_id.
    assert_eq!(
        headers
            .get(HEADER_FORGE_AI_EMBEDDING_MODEL)
            .map(String::as_str),
        Some(MODEL),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_AI_EMBEDDING_REQUEST_ID)
            .map(String::as_str),
        Some("req-1"),
    );

    let decoded = decode_embedding_event(&headers, Some(&body)).unwrap();
    assert_eq!(decoded, EmbeddingEvent::Requested(request()));
}

#[test]
fn emitted_roundtrips_carrying_vectors() {
    // what this catches: the reply must carry the f32 vectors + dim + model
    // intact through encode/decode (no Eq on f32, so PartialEq round-trip).
    let (headers, body) = encode_embedding_event(&EmbeddingEvent::Emitted(emitted())).unwrap();
    assert_eq!(
        headers
            .get(HEADER_FORGE_AI_EMBEDDING_KIND)
            .map(String::as_str),
        Some("emitted"),
    );
    let decoded = decode_embedding_event(&headers, Some(&body)).unwrap();
    assert_eq!(decoded, EmbeddingEvent::Emitted(emitted()));
}

#[test]
fn decode_rejects_wrong_body_hint() {
    // what this catches: an embedding frame mis-read as a persona event (or vice
    // versa) must fail LOUD — a silently-dropped reply would hang the requester.
    let (mut headers, body) =
        encode_embedding_event(&EmbeddingEvent::Requested(request())).unwrap();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.persona.event.v1".to_string(),
    );
    let err = decode_embedding_event(&headers, Some(&body)).unwrap_err();
    assert!(matches!(err, PersonaCodecError::BodyHintMismatch { .. }));
}

#[test]
fn filter_admits_only_embedding_frames() {
    let (headers, _) = encode_embedding_event(&EmbeddingEvent::Requested(request())).unwrap();
    let filter = any_embedding_event_filter();
    assert!(filter.headers_filter.matches(&headers));

    let mut other = Headers::new();
    other.insert(
        "forge.body_hint".to_string(),
        "forge.persona.event.v1".to_string(),
    );
    assert!(!filter.headers_filter.matches(&other));
}

#[test]
fn missing_body_is_a_loud_error() {
    let (headers, _) = encode_embedding_event(&EmbeddingEvent::Requested(request())).unwrap();
    let err = decode_embedding_event(&headers, None).unwrap_err();
    assert!(matches!(err, PersonaCodecError::MissingBody));
}

/// The headline: an `EmbeddingRequested` rides the command bus and the
/// requester's `await_reply` resolves with the facility's `EmbeddingEmitted`
/// before the deadline — proving the embedding pair uses the same carriage as
/// turns. LAN-TCP with separate homes (no shared local-fs).
#[tokio::test]
async fn embedding_request_reply_resolves_before_deadline() {
    let requester_home = TempDir::new().expect("requester home");
    let facility_home = TempDir::new().expect("facility home");

    let requester = Airc::open(requester_home.path())
        .await
        .expect("requester opens");
    let facility = Airc::open(facility_home.path())
        .await
        .expect("facility opens");

    let requester_spec = requester.peer_spec().parse().expect("requester peer spec");
    let facility_spec = facility.peer_spec().parse().expect("facility peer spec");
    requester
        .add_peer(facility_spec)
        .await
        .expect("requester trusts facility");
    facility
        .add_peer(requester_spec)
        .await
        .expect("facility trusts requester");

    requester.join("embedding-bus").await.unwrap();
    facility.join("embedding-bus").await.unwrap();

    let facility_addr = facility
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .expect("facility listens on LAN");
    requester
        .connect_lan(facility_addr, facility.peer_id())
        .await
        .expect("requester connects to facility over LAN");

    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let responder = tokio::spawn(facility_answers_one_embedding(facility.clone(), ready_tx));
    ready_rx.await.expect("facility subscriber is ready");

    let pending = request_embedding(
        &requester,
        MentionTarget::All,
        &request(),
        Duration::from_secs(5),
    )
    .await
    .expect("embedding request emits");

    let reply = requester
        .await_reply(pending)
        .await
        .expect("await_reply resolves with the EmbeddingEmitted reply");
    let decoded =
        decode_embedding_event(&reply.headers, reply.body.as_ref()).expect("reply decodes");
    assert_eq!(decoded, EmbeddingEvent::Emitted(emitted()));

    responder.await.expect("responder completes");
}

/// The facility side: subscribe, decode the typed `EmbeddingRequested`, assert
/// the model slug rode as a header (the routing/space identity), and reply
/// `EmbeddingEmitted` correlating the command bus.
async fn facility_answers_one_embedding(facility: Airc, ready: tokio::sync::oneshot::Sender<()>) {
    let mut stream = facility.subscribe().await.unwrap();
    let _ = ready.send(());
    loop {
        let Some(Ok(event)) = stream.next().await else {
            continue;
        };
        if event.peer_id == facility.peer_id() {
            continue;
        }
        if event
            .headers
            .get(HEADER_FORGE_AI_EMBEDDING_KIND)
            .map(String::as_str)
            != Some("requested")
        {
            continue;
        }
        // The model slug rides as a filterable header — a facility refuses a
        // wrong-space request off this without decoding the body.
        assert_eq!(
            event
                .headers
                .get(HEADER_FORGE_AI_EMBEDDING_MODEL)
                .map(String::as_str),
            Some(MODEL),
        );
        let EmbeddingEvent::Requested(req) =
            decode_embedding_event(&event.headers, event.body.as_ref()).unwrap()
        else {
            continue;
        };
        assert_eq!(req.inputs.len(), 2, "both inputs rode the request");

        reply_embedding_emitted(&facility, &event.headers, &emitted())
            .await
            .expect("embedding reply emits");
        return;
    }
}
