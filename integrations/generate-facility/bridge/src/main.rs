//! airc-generate-bridge — the 5090 generation facility's presence on the grid.
//!
//! The compute-lease provider: a GPU node that serves text generation to the
//! mesh as the `ai/generate` capability, so a GPU-less persona can keep its
//! cognition LOCAL and escalate only the model call ("remote grid inference").
//! Sibling to `integrations/embedding-facility` — same pattern (airc citizen +
//! capability advert + a thin llama.cpp HTTP client), different capability.
//!
//! Unlike the embedding facility, this needs NO new wire types: it answers the
//! EXISTING `TurnRequested` → `TurnEmitted` command-bus pair
//! (`consumer_shapes::continuum`), the same one `request_inference_remote`
//! already drives. A consumer (continuum's `AircRemoteInferenceAdapter` /
//! `GridInferenceProvider`) routes a turn to this facility by `model_hint`; the
//! facility runs the model and replies the text.
//!
//! Refuses (loud) a turn whose `model_hint` names a model it does not host —
//! never silently answers with a different model's output.

mod llamacpp;

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use airc_core::PersonaCapabilities;
use airc_core::{PeerId, TranscriptEvent};
use airc_lib::Airc;
use consumer_shapes::continuum::{
    decode_persona_event, encode_capability_offer, reply_turn_emitted, CapabilityOffer,
    PersonaEvent, TurnEmitted, HEADER_FORGE_PERSONA_KIND,
};
use futures::StreamExt;

/// Re-advertise cadence — under the registry freshness TTL (cross-grid spine
/// uses 180s) so a live facility never flaps stale and gets skipped by
/// `resolve_inference_target` (the cadence-flap lesson, supply side).
const READVERTISE_EVERY: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::var("GEN_BRIDGE_NAME").unwrap_or_else(|_| "generate-facility".into());
    let room = std::env::var("GEN_BRIDGE_ROOM").unwrap_or_else(|_| "general".into());
    let base_url =
        std::env::var("GEN_BRIDGE_LLAMACPP_URL").unwrap_or_else(|_| "http://127.0.0.1:8081".into());
    let model = std::env::var("GEN_BRIDGE_MODEL").unwrap_or_else(|_| "qwen3-coder-30b".into());
    let max_tokens: u32 = std::env::var("GEN_BRIDGE_MAX_TOKENS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(512);
    let temperature: f32 = std::env::var("GEN_BRIDGE_TEMPERATURE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.7);
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
    airc.publish_identity().await?;
    airc.join(&room).await?;
    let me = airc.peer_id();

    let offer = capability_offer(me, &name, &model);
    advertise(&airc, &offer).await?;
    eprintln!(
        "airc-generate-bridge: '{name}' joined #{room} as {me}; advertising {:?} (model={model}, llama.cpp={base_url})",
        offer.capabilities.capability_tags,
    );

    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(120)) // generation is slower than embedding
        .build()?;

    let cfg = GenConfig {
        model,
        max_tokens,
        temperature,
    };
    run_bridge(&airc, me, &offer, &http, &base_url, &cfg).await
}

/// Generation knobs resolved once at startup and threaded into the handler.
struct GenConfig {
    model: String,
    max_tokens: u32,
    temperature: f32,
}

/// Build the standing capability advert. Advertises `ai/generate` (coarse),
/// `ai/generate/<slug>` (parallel to the embedding facility), AND the raw model
/// string — because a `TurnRequested.model_hint` is matched against
/// `capability_tags` verbatim by `resolve_inference_target`, so the raw model
/// name must be a tag for hint-routing to find this facility.
fn capability_offer(me: PeerId, name: &str, model: &str) -> CapabilityOffer {
    CapabilityOffer {
        peer_id: me,
        capabilities: PersonaCapabilities {
            persona_id: name.to_string(),
            capability_tags: vec![
                "ai/generate".to_string(),
                model_tag(model),
                model.to_string(),
            ],
            model: model.to_string(),
            context_window_tokens: 32768,
        },
        offered_at_ms: now_ms(),
    }
}

/// `ai/generate/<model-slug>` — lowercased, non-alphanumeric → `-`, `.` kept.
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
    format!("ai/generate/{slug}")
}

async fn advertise(airc: &Airc, offer: &CapabilityOffer) -> Result<(), Box<dyn std::error::Error>> {
    let mut fresh = offer.clone();
    fresh.offered_at_ms = now_ms();
    let (headers, body) = encode_capability_offer(&fresh)?;
    airc.send(body, headers).await?;
    Ok(())
}

/// The citizen loop: re-advertise on a cadence, and answer each inbound
/// `TurnRequested` from another peer by running the model and replying
/// `TurnEmitted`. Everything else is left alone.
async fn run_bridge(
    airc: &Airc,
    me: PeerId,
    offer: &CapabilityOffer,
    http: &reqwest::Client,
    base_url: &str,
    cfg: &GenConfig,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe().await?;
    let mut readvertise = tokio::time::interval(READVERTISE_EVERY);
    readvertise.tick().await; // first tick is immediate; already advertised at startup

    loop {
        tokio::select! {
            _ = readvertise.tick() => {
                if let Err(e) = advertise(airc, offer).await {
                    eprintln!("airc-generate-bridge: re-advertise failed: {e}");
                }
            }
            item = stream.next() => {
                let Some(item) = item else { break };
                match item {
                    Ok(event) => {
                        if is_turn_request(&event, me) {
                            handle_turn_request(airc, &event, http, base_url, cfg).await;
                        }
                    }
                    Err(lag) => eprintln!("airc-generate-bridge: live stream lagged: {lag}"),
                }
            }
        }
    }
    Ok(())
}

/// True iff this event is a persona `TurnRequested` from another peer. Cheap:
/// keys off the projected `forge.persona.kind = turn_requested` header (the
/// wire kind matching `PersonaEvent::TurnRequested`), no body decode.
fn is_turn_request(event: &TranscriptEvent, me: PeerId) -> bool {
    event.peer_id != me
        && event
            .headers
            .get(HEADER_FORGE_PERSONA_KIND)
            .map(String::as_str)
            == Some("turn_requested")
}

/// Answer a `TurnRequested`: decode, refuse loud if it names a model we do not
/// host, run the model, reply `TurnEmitted` correlating the command bus. Errors
/// are logged not propagated — one bad turn must not kill the facility; the
/// requester's `await_reply` deadlines loudly on a non-reply.
async fn handle_turn_request(
    airc: &Airc,
    event: &TranscriptEvent,
    http: &reqwest::Client,
    base_url: &str,
    cfg: &GenConfig,
) {
    let req = match decode_persona_event(&event.headers, event.body.as_ref()) {
        Ok(PersonaEvent::TurnRequested(r)) => r,
        Ok(_) => return, // some other persona event; not ours to answer
        Err(e) => {
            eprintln!("airc-generate-bridge: undecodable turn request: {e}");
            return;
        }
    };
    if let Some(hint) = req.model_hint.as_deref() {
        if hint != cfg.model {
            eprintln!(
                "airc-generate-bridge: refusing turn {} for model '{}' — this facility serves '{}'",
                req.turn_id, hint, cfg.model
            );
            return;
        }
    }
    let text = match llamacpp::complete(
        http,
        base_url,
        &req.prompt,
        Some(&cfg.model),
        cfg.max_tokens,
        cfg.temperature,
    )
    .await
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!(
                "airc-generate-bridge: generate failed for {}: {e}",
                req.turn_id
            );
            return;
        }
    };
    let emitted = TurnEmitted {
        persona_id: req.persona_id.clone(),
        activity_id: req.activity_id.clone(),
        turn_id: req.turn_id.clone(),
        text,
        emitted_at_ms: now_ms(),
    };
    if let Err(e) = reply_turn_emitted(airc, &event.headers, &emitted).await {
        eprintln!(
            "airc-generate-bridge: reply failed for {}: {e}",
            req.turn_id
        );
    }
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
    use airc_core::{Body, EventId, RoomId, TranscriptKind};

    fn turn_event(peer: PeerId, kind: &str) -> TranscriptEvent {
        let mut headers = airc_core::headers::Headers::new();
        headers.insert(HEADER_FORGE_PERSONA_KIND.to_string(), kind.to_string());
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::from_u128(1),
            peer_id: peer,
            client_id: airc_core::ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target: airc_core::transcript::MentionTarget::All,
            headers,
            body: Some(Body::text("{}")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn detects_turn_request_from_another_peer() {
        // what this catches: the facility answers TurnRequested from other peers
        // — the command-bus turn a GridInferenceProvider escalates.
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert!(is_turn_request(&turn_event(other, "turn_requested"), me));
    }

    #[test]
    fn turn_emitted_is_not_a_request() {
        // what this catches: the facility answers requests, not replies — a
        // turn_emitted (its own or another's) must not be treated as inbound work.
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert!(!is_turn_request(&turn_event(other, "turn_emitted"), me));
    }

    #[test]
    fn own_turn_request_is_ignored() {
        let me = PeerId::from_u128(1);
        assert!(!is_turn_request(&turn_event(me, "turn_requested"), me));
    }

    #[test]
    fn capability_offer_advertises_coarse_slug_and_raw_model() {
        // what this catches: model_hint routing matches the RAW model string as
        // a tag, so the raw model must be advertised alongside the coarse +
        // slug tags or hint-routed turns never find this facility.
        let me = PeerId::from_u128(9);
        let offer = capability_offer(me, "generate-facility", "qwen3-coder-30b");
        let tags = &offer.capabilities.capability_tags;
        assert!(tags.contains(&"ai/generate".to_string()));
        assert!(tags.contains(&"ai/generate/qwen3-coder-30b".to_string()));
        assert!(tags.contains(&"qwen3-coder-30b".to_string()));
        assert_eq!(offer.capabilities.model, "qwen3-coder-30b");
    }

    #[test]
    fn model_tag_slugifies() {
        assert_eq!(model_tag("Qwen3-Coder-30B"), "ai/generate/qwen3-coder-30b");
        assert_eq!(model_tag("llama 3.1 8b"), "ai/generate/llama-3.1-8b");
    }
}
