//! Human/agent-legible rendering of v5 daemon `TranscriptEvent`s for the
//! live feed (`airc join` / attach).
//!
//! Replaces the opaque `<non-text body>` fallback that made the work
//! board invisible in the stream (card d21d355d): structured work-domain
//! events render by kind, and pure-churn presence heartbeats (`alive`)
//! are suppressed so they don't drown real signal. Shared by both feed
//! call sites (`join_feed` and `commands`) so the two can't drift.

use airc_core::{Body, TranscriptEvent};
use airc_work::WorkEvent;
use serde_json::Value;

/// Format one transcript event for the live feed. Returns `None` for
/// events that should be suppressed entirely — currently `alive`
/// presence heartbeats, which are continuous churn (one per peer per
/// minute) and carry nothing the reader acts on.
pub(crate) fn render_feed_line(event: &TranscriptEvent) -> Option<String> {
    let detail = body_detail(event.body.as_ref())?;
    Some(format!(
        "[{kind:?}] {sender} → {channel}: {detail}",
        kind = event.kind,
        sender = event.peer_id,
        channel = event.room_id,
    ))
}

fn body_detail(body: Option<&Body>) -> Option<String> {
    let Some(body) = body else {
        return Some("<no body>".to_owned());
    };
    // Plain chat text — the common case.
    if let Some(text) = body.as_text() {
        return Some(text.to_owned());
    }
    match body {
        Body::Json(value) => json_detail(value),
        Body::Binary(bytes) => Some(format!("⟨binary {} bytes⟩", bytes.len())),
    }
}

fn json_detail(value: &Value) -> Option<String> {
    // Presence heartbeat: pure churn — suppress from the feed.
    if value.get("kind").and_then(Value::as_str) == Some("alive") {
        return None;
    }
    // Typed work-domain event: render a concise, board-legible summary.
    // Deserializing through the domain type (not hand-poking JSON) keeps
    // the schema in one place — airc-work owns it.
    if let Ok(event) = serde_json::from_value::<WorkEvent>(value.clone()) {
        return Some(work_summary(&event));
    }
    // Some other structured body — still beats "<non-text body>".
    Some(match value.get("kind").and_then(Value::as_str) {
        Some(kind) => format!("⟨{kind}⟩"),
        None => "⟨structured⟩".to_owned(),
    })
}

fn work_summary(event: &WorkEvent) -> String {
    match event {
        WorkEvent::CardCreated(e) => format!(
            "card_created [{}] {:?} \"{}\" ({})",
            short(&e.card_id),
            e.priority,
            e.title,
            e.repo,
        ),
        WorkEvent::CardClaimed(e) => {
            format!("card_claimed [{}] by {}", short(&e.card_id), e.owner)
        }
        WorkEvent::ClaimReleased(e) => format!("claim_released [{}]", short(&e.card_id)),
        WorkEvent::CardStateChanged(e) => {
            format!("card_state_changed [{}] → {:?}", short(&e.card_id), e.state)
        }
        // Lease churn — frequent, low-signal; summarize tersely.
        WorkEvent::ClaimHeartbeat(e) => format!("claim_heartbeat [{}]", short(&e.card_id)),
        // Everything else: name the kind without dumping the payload.
        other => kind_label(other),
    }
}

/// First 8 chars of an id for compact display (ids are UUIDs).
fn short(id: &impl std::fmt::Display) -> String {
    id.to_string().chars().take(8).collect()
}

/// The serde `kind` tag for variants we don't render explicitly.
fn kind_label(event: &WorkEvent) -> String {
    serde_json::to_value(event)
        .ok()
        .and_then(|v| {
            v.get("kind")
                .and_then(Value::as_str)
                .map(|k| format!("⟨{k}⟩"))
        })
        .unwrap_or_else(|| "⟨work_event⟩".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn chat_text_renders_verbatim() {
        assert_eq!(body_detail(Some(&Body::text("hello"))), Some("hello".into()));
    }

    #[test]
    fn alive_heartbeat_is_suppressed() {
        let alive = Body::Json(json!({
            "kind": "alive", "peer": "p", "runtime": "agent",
            "client_id": "claude:x", "scope": "/tmp", "build": "abc",
            "emitted_at_ms": 0
        }));
        assert_eq!(body_detail(Some(&alive)), None);
    }

    #[test]
    fn card_created_renders_title_and_priority_not_opaque() {
        let card = Body::Json(json!({
            "kind": "card_created",
            "card_id": "8416ed7f-1e85-41bc-bcba-6f1fc0021e1e",
            "repo": "CambrianTech/airc",
            "title": "fix the thing",
            "body": null,
            "priority": "p1",
            "lane_id": null,
            "created_by": "00000000-0000-0000-0000-000000000001",
            "created_at_ms": 0
        }));
        let out = body_detail(Some(&card)).expect("renders");
        assert!(out.contains("card_created"), "got: {out}");
        assert!(out.contains("fix the thing"), "got: {out}");
        assert!(out.contains("8416ed7f"), "short id: {out}");
        assert!(!out.contains("non-text body"), "must not be opaque: {out}");
    }

    #[test]
    fn unknown_structured_body_names_its_kind() {
        let other = Body::Json(json!({ "kind": "some_future_event", "x": 1 }));
        assert_eq!(body_detail(Some(&other)), Some("⟨some_future_event⟩".into()));
    }

    #[test]
    fn binary_body_reports_length_not_opaque() {
        assert_eq!(
            body_detail(Some(&Body::Binary(vec![0u8; 12]))),
            Some("⟨binary 12 bytes⟩".into())
        );
    }
}
