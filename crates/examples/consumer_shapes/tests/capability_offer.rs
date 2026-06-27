//! Capability-offer contract + registry-routing fixtures — card a9580f9d
//! (persona-peer 4/8).
//!
//! Asserts the offer codec projects the right headers and roundtrips,
//! the wire offer feeds the airc-lib [`CapabilityRegistry`] projection,
//! and a [`TurnRequested`]'s `model_hint` (card ee2a339f) drives an
//! actual candidate selection. These fixtures are the source-of-truth
//! for the offer wire shape — any change coordinates with the Continuum
//! scheduler that subscribes against the same filter.

use airc_lib::{Body, CapabilityRegistry, Headers, PeerId, PersonaCapabilities, TrustTier};
use consumer_shapes::continuum::{
    capability_offer_filter, decode_capability_offer, encode_capability_offer,
    ingest_capability_offer, select_candidate_for_turn, CapabilityOffer, PersonaCodecError,
    TurnRequested, BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER, HEADER_FORGE_PERSONA_ID,
    HEADER_FORGE_PERSONA_PEER_ID,
};

const TTL_MS: u64 = 180_000;

fn caps(persona: &str, tags: &[&str], model: &str, ctx: u32) -> PersonaCapabilities {
    PersonaCapabilities {
        persona_id: persona.to_string(),
        capability_tags: tags.iter().map(|t| t.to_string()).collect(),
        model: model.to_string(),
        context_window_tokens: ctx,
    }
}

fn offer(peer: PeerId, persona: &str, tags: &[&str], model: &str, ctx: u32) -> CapabilityOffer {
    CapabilityOffer {
        peer_id: peer,
        capabilities: caps(persona, tags, model, ctx),
        offered_at_ms: 1_000,
    }
}

fn turn_with_hint(hint: Option<&str>) -> TurnRequested {
    TurnRequested {
        persona_id: "requester".to_string(),
        activity_id: "session-1".to_string(),
        turn_id: "turn-1".to_string(),
        prompt: "route me".to_string(),
        model_hint: hint.map(str::to_string),
        requested_at_ms: 1_000,
    }
}

#[test]
fn offer_roundtrips_and_projects_headers() {
    let peer = PeerId::new();
    let original = offer(
        peer,
        "skylar",
        &["code", "long-context"],
        "fable-5",
        200_000,
    );
    let (headers, body) = encode_capability_offer(&original).unwrap();

    assert_eq!(
        headers.get("forge.body_hint").map(String::as_str),
        Some(BODY_HINT_FORGE_PERSONA_CAPABILITY_OFFER),
    );
    assert_eq!(
        headers
            .get(HEADER_FORGE_PERSONA_PEER_ID)
            .map(String::as_str),
        Some(peer.to_string().as_str()),
    );
    assert_eq!(
        headers.get(HEADER_FORGE_PERSONA_ID).map(String::as_str),
        Some("skylar"),
    );

    let decoded = decode_capability_offer(&headers, Some(&body)).unwrap();
    assert_eq!(decoded, original, "offer roundtrip diverged");
}

#[test]
fn offer_filter_admits_offers_only() {
    let (headers, _) = encode_capability_offer(&offer(
        PeerId::new(),
        "skylar",
        &["code"],
        "fable-5",
        200_000,
    ))
    .unwrap();
    let filter = capability_offer_filter();
    assert!(filter.headers_filter.matches(&headers));

    // A non-offer header set (no offer body hint) is rejected.
    let mut other = Headers::new();
    other.insert(
        "forge.body_hint".to_string(),
        "forge.work.event.v1".to_string(),
    );
    assert!(!filter.headers_filter.matches(&other));
}

#[test]
fn decode_rejects_wrong_body_hint() {
    let (mut headers, body) = encode_capability_offer(&offer(
        PeerId::new(),
        "skylar",
        &["code"],
        "fable-5",
        200_000,
    ))
    .unwrap();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.persona.event.v1".to_string(),
    );
    let err = decode_capability_offer(&headers, Some(&body)).unwrap_err();
    assert!(matches!(err, PersonaCodecError::BodyHintMismatch { .. }));
}

#[test]
fn legacy_offer_body_without_future_fields_still_decodes() {
    // Additive/versioned contract: an offer body encoded by a v1 writer
    // that omits capability_tags (the one defaulting field on
    // PersonaCapabilities) must still decode.
    let peer = PeerId::from_u128(5);
    let (headers, _) =
        encode_capability_offer(&offer(peer, "skylar", &["code"], "fable-5", 200_000)).unwrap();
    let legacy_body = Body::Json(serde_json::json!({
        "peer_id": peer.to_string(),
        "capabilities": {
            "persona_id": "skylar",
            "model": "fable-5",
            "context_window_tokens": 200_000u32,
        },
        "offered_at_ms": 1_000u64,
    }));
    let decoded = decode_capability_offer(&headers, Some(&legacy_body)).unwrap();
    assert!(decoded.capabilities.capability_tags.is_empty());
    assert_eq!(decoded.peer_id, peer);
}

#[test]
fn wire_offer_feeds_registry_and_matches() {
    // The end-to-end consumer path: decode a wire offer, ingest it into
    // the airc-lib registry, query a need.
    let peer = PeerId::from_u128(42);
    let (headers, body) =
        encode_capability_offer(&offer(peer, "skylar", &["code"], "fable-5", 200_000)).unwrap();
    let decoded = decode_capability_offer(&headers, Some(&body)).unwrap();

    let mut registry = CapabilityRegistry::new();
    ingest_capability_offer(&mut registry, &decoded);
    assert_eq!(registry.len(), 1);

    let selected = select_candidate_for_turn(
        &registry,
        &turn_with_hint(None),
        &["code"],
        1_500,
        TTL_MS,
        |_| TrustTier::OwnMachine,
    );
    let candidate = selected.expect("a node advertising `code` must be selected");
    assert_eq!(candidate.peer_id, peer);
}

#[test]
fn model_hint_selects_a_node_advertising_that_model_tag() {
    // 3/8's model_hint header → 4/8 selection. The hint is treated as a
    // required capability tag: the node advertising it wins over one
    // that does not, even at a lower trust tier (capability gates first).
    let plain = PeerId::from_u128(1); // OwnMachine, but no clio tag
    let clio = PeerId::from_u128(2); // Untrusted, advertises clio

    let mut registry = CapabilityRegistry::new();
    ingest_capability_offer(
        &mut registry,
        &offer(plain, "plain", &["code"], "fable-5", 200_000),
    );
    ingest_capability_offer(
        &mut registry,
        &offer(clio, "clio", &["code", "lora-clio-v3"], "fable-5", 200_000),
    );

    let trust = move |p: PeerId| {
        if p == plain {
            TrustTier::OwnMachine
        } else {
            TrustTier::Untrusted
        }
    };
    let selected = select_candidate_for_turn(
        &registry,
        &turn_with_hint(Some("lora-clio-v3")),
        &[],
        1_500,
        TTL_MS,
        trust,
    );
    let candidate = selected.expect("hinted model must select the advertising node");
    assert_eq!(
        candidate.peer_id, clio,
        "model_hint routes to the node advertising it, even at lower trust",
    );
}

#[test]
fn unmatched_model_hint_selects_nothing_not_an_error() {
    // Grid behaviour: a hint no live node advertises yields None — the
    // caller runs locally / queues. Never an error.
    let peer = PeerId::from_u128(9);
    let mut registry = CapabilityRegistry::new();
    ingest_capability_offer(
        &mut registry,
        &offer(peer, "skylar", &["code"], "fable-5", 200_000),
    );
    let selected = select_candidate_for_turn(
        &registry,
        &turn_with_hint(Some("nonexistent-model")),
        &[],
        1_500,
        TTL_MS,
        |_| TrustTier::OwnMachine,
    );
    assert!(
        selected.is_none(),
        "unmatched hint selects nothing, not error"
    );
}

#[test]
fn empty_registry_selects_nothing_grid_of_one() {
    let registry = CapabilityRegistry::new();
    let selected = select_candidate_for_turn(
        &registry,
        &turn_with_hint(Some("anything")),
        &["code"],
        1_500,
        TTL_MS,
        |_| TrustTier::OwnMachine,
    );
    assert!(
        selected.is_none(),
        "grid-of-one: empty registry selects nothing"
    );
}
