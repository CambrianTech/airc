//! airc-embedding-bridge — the 5090 embedding facility's presence on the grid.
//!
//! Adapter sibling to `integrations/acp`: a standalone Rust bin that is both an
//! airc citizen (join / publish_identity / advertise a capability / subscribe)
//! and the client of a local compute backend — here llama.cpp's `--embedding`
//! server (the `docker-compose.yml` in the parent dir). See ../README.md for
//! the facility architecture and the one-vector-space invariant.
//!
//! ## Slices
//! - **Slice 2a:** the citizen + capability-advertisement half. Joins a room,
//!   grounds by name, advertises the `ai/embedding` capability (re-advertised on
//!   a cadence so it never ages out of peers' registries — the staleness-flap is
//!   a known routing P0), and answers an `/embed <text>` PROBE by round-tripping
//!   the local llama.cpp server (the live smoke, mirrors acp's `/acp-ping`).
//! - **Slice 2b (this file, with 2a):** the MACHINE path. Answers a typed
//!   `EmbeddingRequested` command-bus request (`consumer_shapes::continuum`) by
//!   embedding via llama.cpp and replying `EmbeddingEmitted` — the same
//!   request/reply carriage `TurnRequested`/`TurnEmitted` ride. This is what
//!   slice 3's `GridEmbeddingProvider` drives via `request_embedding_remote`;
//!   the `/embed` probe stays as the human smoke. Refuses (loud) a request for a
//!   model it does not host — never embeds in the wrong vector space.
//! - **Slice 3:** continuum's neural `EmbeddingProvider` grows a
//!   `GridEmbeddingProvider` that embeds locally for the hot path and escalates
//!   batch jobs to this facility via the capability registry.

mod llamacpp;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use airc_core::PersonaCapabilities;
use airc_core::{PeerId, TranscriptEvent, TranscriptKind};
use airc_lib::Airc;
use consumer_shapes::continuum::{
    decode_embedding_event, encode_capability_offer, reply_embedding_emitted, CapabilityOffer,
    EmbeddingEmitted, EmbeddingEvent, HEADER_FORGE_AI_EMBEDDING_KIND,
};
use futures::StreamExt;

/// How often to re-publish the capability offer. Kept well under the registry's
/// freshness TTL (cross-grid spine uses 180s) so a live facility never flaps to
/// "stale" and gets skipped by `resolve_inference_target` — the cadence-flap
/// lesson, applied at the supply side.
const READVERTISE_EVERY: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::var("EMBED_BRIDGE_NAME").unwrap_or_else(|_| "embedding-facility".into());
    let room = std::env::var("EMBED_BRIDGE_ROOM").unwrap_or_else(|_| "general".into());
    let base_url = std::env::var("EMBED_BRIDGE_LLAMACPP_URL")
        .unwrap_or_else(|_| "http://127.0.0.1:8080".into());
    let model =
        std::env::var("EMBED_BRIDGE_MODEL").unwrap_or_else(|_| "Qwen3-Embedding-0.6B".into());
    let home = std::env::var("AIRC_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".into());
            std::path::Path::new(&base).join(".airc")
        });

    let airc = match std::env::var("AIRC_SOCKET").ok() {
        Some(socket) => Airc::attach_as(home, &name, socket).await?,
        None => Airc::open_as(home, &name).await?,
    };
    airc.publish_identity().await?; // grounded by name (room_roster + whois)
    airc.join(&room).await?;
    let me = airc.peer_id();

    let offer = capability_offer(me, &name, &model);
    advertise(&airc, &offer).await?;
    eprintln!(
        "airc-embedding-bridge: '{name}' joined #{room} as {me}; advertising {:?} (model={model}, llama.cpp={base_url})",
        offer.capabilities.capability_tags,
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;

    run_bridge(&airc, me, &offer, &http, &base_url, &model).await
}

/// Build the standing capability advert for this facility. `persona_id` carries
/// the facility name (the WHO); the tags are the routable capability (the
/// WHAT). Both a coarse `ai/embedding` tag (any embedder) and a model-qualified
/// `ai/embedding/<model>` tag (the one-vector-space contract — a peer can
/// demand ITS embedder) are advertised.
fn capability_offer(me: PeerId, name: &str, model: &str) -> CapabilityOffer {
    CapabilityOffer {
        peer_id: me,
        capabilities: PersonaCapabilities {
            persona_id: name.to_string(),
            capability_tags: vec!["ai/embedding".to_string(), model_tag(model)],
            model: model.to_string(),
            context_window_tokens: 8192,
        },
        offered_at_ms: now_ms(),
    }
}

/// `ai/embedding/<model-slug>` — lowercased, spaces/slashes to dashes, so the
/// tag is a stable routable token regardless of how the model name is cased.
fn model_tag(model: &str) -> String {
    let slug: String = model
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' {
                c
            } else {
                '-'
            }
        })
        .collect();
    format!("ai/embedding/{slug}")
}

/// Publish the capability offer to the room. Refreshes `offered_at_ms` each call
/// so the re-advertisement keeps the offer fresh in peers' registries.
async fn advertise(airc: &Airc, offer: &CapabilityOffer) -> Result<(), Box<dyn std::error::Error>> {
    let mut fresh = offer.clone();
    fresh.offered_at_ms = now_ms();
    let (headers, body) = encode_capability_offer(&fresh)?;
    airc.send(body, headers).await?;
    Ok(())
}

/// The citizen loop: re-advertise on a cadence, and for each inbound event
/// either (a) answer a typed `EmbeddingRequested` command-bus request — the
/// MACHINE path that slice 3's `GridEmbeddingProvider` drives — or (b) answer an
/// `/embed <text>` human PROBE. Anything else is left alone (no chatty echo).
async fn run_bridge(
    airc: &Airc,
    me: PeerId,
    offer: &CapabilityOffer,
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe().await?;
    let mut readvertise = tokio::time::interval(READVERTISE_EVERY);
    readvertise.tick().await; // first tick fires immediately; we already advertised at startup

    loop {
        tokio::select! {
            _ = readvertise.tick() => {
                if let Err(e) = advertise(airc, offer).await {
                    eprintln!("airc-embedding-bridge: re-advertise failed: {e}");
                }
            }
            item = stream.next() => {
                let Some(item) = item else { break }; // stream ended
                match item {
                    Ok(event) => {
                        // Machine path (slice 2b): typed command-bus embedding request.
                        if is_embedding_request(&event, me) {
                            handle_embedding_request(airc, &event, http, base_url, model).await;
                            continue;
                        }
                        // Human path (slice 2a): the /embed live smoke probe.
                        let Some(text) = probe_input(&event, me) else { continue };
                        let reply = match llamacpp::embed(http, base_url, &[text.to_string()], Some(model)).await {
                            Ok(vectors) => format_probe_reply(&vectors, model),
                            Err(e) => format!("embed probe failed: {e}"),
                        };
                        airc.say(&reply).await?;
                    }
                    Err(lag) => eprintln!("airc-embedding-bridge: live stream lagged: {lag}"),
                }
            }
        }
    }
    Ok(())
}

/// True iff this event is a typed embedding REQUEST from another peer. Cheap:
/// keys off the projected `forge.ai.embedding.kind = requested` header, no body
/// decode. (Our own posts and `emitted` replies are not requests.)
fn is_embedding_request(event: &TranscriptEvent, me: PeerId) -> bool {
    event.peer_id != me
        && event
            .headers
            .get(HEADER_FORGE_AI_EMBEDDING_KIND)
            .map(String::as_str)
            == Some("requested")
}

/// Answer a typed `EmbeddingRequested`: decode, refuse loudly if it asks for a
/// model we do not host (never embed in the wrong vector space), embed via
/// llama.cpp, and reply `EmbeddingEmitted` correlating the command bus. Errors
/// are logged, not propagated — one bad request must not kill the facility loop;
/// the requester's `await_reply` deadlines loudly on a non-reply.
async fn handle_embedding_request(
    airc: &Airc,
    event: &TranscriptEvent,
    http: &reqwest::Client,
    base_url: &str,
    model: &str,
) {
    let req = match decode_embedding_event(&event.headers, event.body.as_ref()) {
        Ok(EmbeddingEvent::Requested(r)) => r,
        Ok(EmbeddingEvent::Emitted(_)) => return, // we answer requests, not replies
        Err(e) => {
            eprintln!("airc-embedding-bridge: undecodable embedding request: {e}");
            return;
        }
    };
    if req.model != model {
        // Routing should prevent this (the model-qualified capability tag), but
        // refuse defensively rather than serve a wrong-space vector.
        eprintln!(
            "airc-embedding-bridge: refusing request {} for model '{}' — this facility serves '{}'",
            req.request_id, req.model, model
        );
        return;
    }
    let vectors = match llamacpp::embed(http, base_url, &req.inputs, Some(model)).await {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "airc-embedding-bridge: embed failed for {}: {e}",
                req.request_id
            );
            return;
        }
    };
    let dim = vectors.first().map(|v| v.len() as u32).unwrap_or(0);
    let emitted = EmbeddingEmitted {
        request_id: req.request_id.clone(),
        model: model.to_string(),
        vectors,
        dim,
        emitted_at_ms: now_ms(),
    };
    if let Err(e) = reply_embedding_emitted(airc, &event.headers, &emitted).await {
        eprintln!(
            "airc-embedding-bridge: reply failed for {}: {e}",
            req.request_id
        );
    }
}

/// Pure probe filter: returns the text to embed iff this event is a chat
/// MESSAGE from ANOTHER peer of the form `/embed <text>` with non-empty text.
/// `None` for our own posts, lifecycle events, and non-probe messages — so the
/// bridge never replies to itself or to ordinary chatter.
fn probe_input(event: &TranscriptEvent, me: PeerId) -> Option<&str> {
    if event.peer_id == me {
        return None;
    }
    if event.kind != TranscriptKind::Message {
        return None;
    }
    let text = event.body.as_ref()?.as_text()?.trim();
    let rest = text.strip_prefix("/embed ")?.trim();
    if rest.is_empty() {
        None
    } else {
        Some(rest)
    }
}

/// Human-readable probe reply: the model, the vector dimension, and a short
/// preview of the leading components — enough to confirm a real vector came
/// back from the GPU without dumping a 1024-float array into the room.
fn format_probe_reply(vectors: &[Vec<f32>], model: &str) -> String {
    let Some(first) = vectors.first() else {
        return "embed probe: server returned no vectors".to_string();
    };
    let preview: Vec<String> = first.iter().take(4).map(|f| format!("{f:.4}")).collect();
    format!(
        "embed probe ok — model={model}, dim={}, head=[{}, ...]",
        first.len(),
        preview.join(", ")
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{Body, EventId, RoomId};

    fn msg(peer: PeerId, text: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::from_u128(1),
            peer_id: peer,
            client_id: airc_core::ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target: airc_core::transcript::MentionTarget::All,
            headers: airc_core::headers::Headers::new(),
            body: Some(Body::text(text)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn probe_extracts_text_after_embed_prefix() {
        // what this catches: the probe must trigger on `/embed <text>` and
        // hand the trailing text (trimmed) to the embedder.
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert_eq!(
            probe_input(&msg(other, "/embed hello grid"), me),
            Some("hello grid")
        );
    }

    #[test]
    fn probe_ignores_ordinary_chatter() {
        // what this catches: the bridge is NOT a chatty echo — a normal message
        // (no `/embed` prefix) draws no reply.
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert_eq!(probe_input(&msg(other, "hello grid"), me), None);
    }

    #[test]
    fn probe_never_reacts_to_own_posts() {
        // The orphan of bots: embedding your own reply into a loop. Pinned shut.
        let me = PeerId::from_u128(1);
        assert_eq!(probe_input(&msg(me, "/embed my own post"), me), None);
    }

    #[test]
    fn probe_skips_non_message_kinds() {
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        let mut ev = msg(other, "/embed ignored");
        ev.kind = TranscriptKind::Presence;
        assert_eq!(probe_input(&ev, me), None);
    }

    #[test]
    fn probe_rejects_empty_payload() {
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert_eq!(probe_input(&msg(other, "/embed    "), me), None);
    }

    #[test]
    fn capability_offer_advertises_coarse_and_model_qualified_tags() {
        // what this catches: a peer must be able to demand EITHER any embedder
        // (`ai/embedding`) or specifically ITS model (`ai/embedding/<model>`) —
        // the model-qualified tag is the one-vector-space routing contract.
        let me = PeerId::from_u128(7);
        let offer = capability_offer(me, "embedding-facility", "Qwen3-Embedding-0.6B");
        assert_eq!(offer.peer_id, me);
        assert!(offer
            .capabilities
            .capability_tags
            .contains(&"ai/embedding".to_string()));
        assert!(offer
            .capabilities
            .capability_tags
            .contains(&"ai/embedding/qwen3-embedding-0.6b".to_string()));
        assert_eq!(offer.capabilities.model, "Qwen3-Embedding-0.6B");
    }

    #[test]
    fn model_tag_slugifies_punctuation_and_case() {
        assert_eq!(
            model_tag("Qwen3-Embedding-0.6B"),
            "ai/embedding/qwen3-embedding-0.6b"
        );
        assert_eq!(model_tag("bge/small en"), "ai/embedding/bge-small-en");
    }

    #[test]
    fn probe_reply_reports_dim_and_preview() {
        let reply = format_probe_reply(&[vec![0.1, 0.2, 0.3, 0.4, 0.5]], "m");
        assert!(
            reply.contains("dim=5"),
            "reply states the dimension: {reply}"
        );
        assert!(
            reply.contains("0.1000"),
            "reply previews leading components: {reply}"
        );
    }

    fn with_kind(peer: PeerId, kind: &str) -> TranscriptEvent {
        let mut ev = msg(peer, "{}");
        ev.headers
            .insert(HEADER_FORGE_AI_EMBEDDING_KIND.to_string(), kind.to_string());
        ev
    }

    #[test]
    fn detects_typed_embedding_request_from_another_peer() {
        // what this catches: the MACHINE path (slice 2b) must fire on a typed
        // `requested` frame from another peer — that's the command-bus request
        // slice 3's GridEmbeddingProvider sends.
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert!(is_embedding_request(&with_kind(other, "requested"), me));
    }

    #[test]
    fn emitted_frame_is_not_a_request() {
        // what this catches: the bridge answers REQUESTS, never replies — an
        // `emitted` frame must not be treated as inbound work (would loop).
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert!(!is_embedding_request(&with_kind(other, "emitted"), me));
    }

    #[test]
    fn own_embedding_request_is_ignored() {
        let me = PeerId::from_u128(1);
        assert!(!is_embedding_request(&with_kind(me, "requested"), me));
    }

    #[test]
    fn plain_message_is_not_an_embedding_request() {
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert!(!is_embedding_request(&msg(other, "hello"), me));
    }
}
