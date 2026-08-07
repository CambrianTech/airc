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
    // Live stream chunks are EPHEMERAL by the substrate's own contract —
    // `Airc::publish_stream_chunk` documents them as "typing-indicator class
    // traffic, NOT durable transcript content", and the settled utterance is
    // published separately as a `Message`. Rendering them put one feed line on
    // the wire per TOKEN (~4/sec per talking persona), so a single paragraph
    // arrived as dozens of unattributable fragments ("I", " see that my",
    // " recent messages have been") and then AGAIN in full as the Message —
    // pure duplication that drowns real signal and rate-limits agent monitors
    // into suppressing the channel entirely.
    //
    // Discriminated by the authoritative marker, not a heuristic: the
    // `airc.stream.id` header is what `parse_stream_chunk` itself keys on, so
    // this can never accidentally swallow a real System message (host eviction,
    // error) — those carry no stream header. Same suppression `alive` already
    // gets below, for the same reason: continuous churn the reader never acts on.
    if event.headers.contains_key(airc_lib::HEADER_STREAM_ID) {
        return None;
    }
    let detail = body_detail(event.body.as_ref())?;
    // ONE event = ONE line, unconditionally. A multi-line message body used to
    // print raw, so every consumer that tails this feed line-by-line (agent
    // monitors, hooks, grep pipelines) saw the first line with its [kind]
    // header and then a storm of orphan fragments ("1. **", "- Investigate",
    // "Let") with no sender, no channel, no kind — hours of unattributable
    // noise per multi-paragraph message (glass-boxed 2026-07-31). Newlines
    // become a visible pilcrow so structure stays readable while the line
    // contract holds.
    let detail = detail.replace('\n', " ¶ ");
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
        // Lease KEEPALIVE is churn, exactly like `alive` above: it repeats every
        // presence pulse for every held card and says nothing that changed. One
        // citizen holding three cards emits three lines per pulse forever; an
        // agent tailing this feed sees the same burst indefinitely and starts
        // suppressing the whole channel, which is how real signal gets lost
        // (lived through it across a full session on 2026-08-07 — every flood
        // was this one line).
        //
        // The claim STATE is not lost by suppressing it: `card_claimed` and
        // `claim_released` still render because those are the transitions, and
        // the standing truth lives on `airc work board` where a lease shows as
        // live or <STALE>. Render what CHANGED, never the keepalive proving
        // nothing changed.
        if matches!(event, WorkEvent::ClaimHeartbeat(_)) {
            return None;
        }
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
        assert_eq!(
            body_detail(Some(&Body::text("hello"))),
            Some("hello".into())
        );
    }

    // what this catches: lease KEEPALIVE reaching the feed. `claim_heartbeat`
    // repeats every presence pulse for every held card and reports nothing that
    // changed — one citizen holding three cards emits three identical lines per
    // pulse, forever. Measured live 2026-08-07: an agent monitoring this feed
    // received ~30 such bursts in one session, several per minute, and the
    // rational response to an unbroken stream of no-op lines is to stop reading
    // the channel — which is exactly how real signal (a peer's question, a
    // failure) gets missed.
    //
    // The transitions still render (asserted below), and standing truth lives on
    // `airc work board`, so nothing is lost: this suppresses the proof that
    // nothing happened, not the fact that something did.
    #[test]
    fn a_claim_heartbeat_never_reaches_the_feed() {
        let heartbeat = json!({
            "kind": "claim_heartbeat",
            "card_id": "00000000-0000-0000-0000-0000000000ff",
            "claim_id": "00000000-0000-0000-0000-0000000000aa",
            "owner": "00000000-0000-0000-0000-0000000000bb",
            "ttl_ms": 1_800_000_u64,
            "heartbeat_at_ms": 1_800_000_000_000_u64,
        });
        assert_eq!(
            json_detail(&heartbeat),
            None,
            "lease keepalive is churn and must be suppressed like `alive`"
        );
    }

    // what this catches: over-suppression. The transitions a reader DOES act on
    // must survive — if a future filter widened to all work events, the board
    // would go silent and look dead rather than quiet.
    #[test]
    fn claim_transitions_still_render() {
        let claimed = json!({
            "kind": "card_claimed",
            "card_id": "00000000-0000-0000-0000-0000000000ff",
            "claim_id": "00000000-0000-0000-0000-0000000000aa",
            "owner": "00000000-0000-0000-0000-0000000000bb",
            "ttl_ms": 1_800_000_u64,
            "claimed_at_ms": 1_800_000_000_000_u64,
        });
        let rendered = json_detail(&claimed).expect("a claim is a transition worth reading");
        assert!(
            rendered.starts_with("card_claimed [") && rendered.contains(" by "),
            "expected the PARSED summary, not the generic kind fallthrough \
             (which would also contain the word) — got: {rendered}"
        );
    }

    // what this catches: a live stream chunk must NEVER reach the feed. Chunks
    // are ephemeral typing-indicator traffic (`publish_stream_chunk`'s own
    // contract) emitted ~4/sec per talking persona, and the same text arrives
    // again as a settled Message — so rendering them is pure duplication that
    // rate-limited agent monitors into suppressing the whole channel
    // (glass-boxed 2026-08-05). Regression for #275.
    #[test]
    fn live_stream_chunks_are_suppressed_but_real_system_messages_are_not() {
        use airc_core::{
            ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
        };

        let base = |headers: Headers, text: &str| TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::System,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers,
            body: Some(Body::text(text)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };

        let mut chunk_headers = Headers::new();
        chunk_headers.insert(airc_lib::HEADER_STREAM_ID.into(), "stream-1".to_string());
        chunk_headers.insert(airc_lib::HEADER_STREAM_SEQ.into(), "7".to_string());
        chunk_headers.insert(
            airc_lib::HEADER_STREAM_KIND.into(),
            "text.token".to_string(),
        );
        assert!(
            render_feed_line(&base(chunk_headers, " see that my")).is_none(),
            "a chunk carrying airc.stream.id must be suppressed from the feed"
        );

        // The discriminator is the stream header, NOT the System kind — a real
        // substrate System message carries no stream id and must still render,
        // or suppressing chunk noise would also blind the reader to evictions.
        let line = render_feed_line(&base(Headers::new(), "host evicted: quota exceeded"))
            .expect("a real System message still renders");
        assert!(
            line.contains("host evicted"),
            "system detail survives: {line}"
        );
    }

    // what this catches: the ONE-event-ONE-line feed contract (glass-boxed
    // 2026-07-31) — a multi-paragraph message must never spill raw newlines
    // into the feed, where line-tailing consumers (agent monitors, hooks,
    // grep) render every body line as an orphan fragment with no sender or
    // kind. Regression: hours of "1. **" / "- Investigate" noise per message.
    #[test]
    fn multiline_body_renders_as_exactly_one_feed_line() {
        use airc_core::{
            ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
        };
        let event = TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(
                "Focused tasks:\n1. **NVMe benchmarks**\n- run io-probe\n\n2. **Layer split**",
            )),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        let line = render_feed_line(&event).expect("message renders");
        assert!(
            !line.contains('\n'),
            "feed line must never contain a raw newline: {line:?}"
        );
        assert!(
            line.contains("NVMe") && line.contains("Layer split"),
            "content survives flattening: {line}"
        );
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
        assert_eq!(
            body_detail(Some(&other)),
            Some("⟨some_future_event⟩".into())
        );
    }

    #[test]
    fn binary_body_reports_length_not_opaque() {
        assert_eq!(
            body_detail(Some(&Body::Binary(vec![0u8; 12]))),
            Some("⟨binary 12 bytes⟩".into())
        );
    }
}
