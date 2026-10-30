//! Live feed rendering. Typed event selection owns visibility; bodies are content.

use airc_core::{Body, MentionTarget, TranscriptEvent};

/// Render an event admitted by the shared agent selection without inspecting its payload.
pub(crate) fn render_feed_line(event: &TranscriptEvent) -> Option<String> {
    if !airc_lib::agent_event_selection().matches(event.class) {
        return None;
    }
    // Keep one event on one line, with its retrieval handle before the body.
    let detail = body_detail(event.body.as_ref()).replace('\n', " ¶ ");
    Some(format!(
        "[{kind:?}] {sender} → {address} e={handle}: {detail}",
        kind = event.kind,
        sender = short(&event.peer_id),
        address = address(event),
        handle = short(&event.event_id),
    ))
}

/// Where the event was addressed, in the feed's short-id form.
fn address(event: &TranscriptEvent) -> String {
    let room = short(&event.room_id);
    match &event.target {
        MentionTarget::All => room,
        MentionTarget::Peer(peer) => format!("@{}/{room}", short(peer)),
        MentionTarget::Room(other) => format!("{room}→{}", short(other)),
    }
}

fn body_detail(body: Option<&Body>) -> String {
    let Some(body) = body else {
        return "<no body>".to_owned();
    };
    if let Some(text) = body.as_text() {
        return text.to_owned();
    }
    match body {
        Body::Json(value) => value.to_string(),
        Body::Binary(bytes) => format!("⟨binary {} bytes⟩", bytes.len()),
    }
}

/// First 8 chars of an id for compact display (ids are UUIDs).
fn short(id: &impl std::fmt::Display) -> String {
    id.to_string().chars().take(8).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{ActivityKind, ControlKind, CorrespondenceKind, EventClass};
    use serde_json::json;

    fn event(class: EventClass, body: Body) -> TranscriptEvent {
        TranscriptEvent {
            class,
            event_id: airc_core::EventId::new(),
            room_id: airc_core::RoomId::new(),
            peer_id: airc_core::PeerId::new(),
            client_id: airc_core::ClientId::new(),
            kind: airc_core::TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers: airc_core::Headers::new(),
            body: Some(body),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }

    #[test]
    fn correspondence_preserves_body_and_ignores_stream_labels() {
        for payload in [
            json!({"kind": "alive", "detail": "please investigate"}),
            json!({"gpuTotalBytes": 1, "atMs": 2}),
            json!({"kind": "claim_heartbeat", "detail": "quoted evidence"}),
        ] {
            let mut message = event(
                EventClass::Correspondence(CorrespondenceKind::Message),
                Body::Json(payload.clone()),
            );
            message
                .headers
                .insert(airc_lib::HEADER_STREAM_ID.into(), "stream-1".into());
            let line = render_feed_line(&message).expect("correspondence is admitted");
            assert!(line.ends_with(&payload.to_string()), "{line}");
        }
    }

    #[test]
    fn visibility_depends_only_on_typed_class() {
        let payload = Body::text("important chat-looking content");
        for class in [
            EventClass::Activity(ActivityKind::Presence),
            EventClass::Activity(ActivityKind::WorkPresence),
            EventClass::Activity(ActivityKind::WorkReceipt),
            EventClass::Control(ControlKind::Stream),
            EventClass::Control(ControlKind::Receipt),
        ] {
            assert!(render_feed_line(&event(class, payload.clone())).is_none());
        }
        let state = event(
            EventClass::Activity(ActivityKind::WorkState),
            Body::Json(json!({"lane": "coordination", "detail": "changed"})),
        );
        assert!(render_feed_line(&state)
            .expect("work state is admitted")
            .contains("changed"));
        let coordination = event(
            EventClass::Activity(ActivityKind::Coordination),
            Body::text("lane ownership changed"),
        );
        assert!(render_feed_line(&coordination).is_some());
    }

    #[test]
    fn chat_text_renders_verbatim() {
        assert_eq!(body_detail(Some(&Body::text("hello"))), "hello");
    }

    #[test]
    fn opaque_correspondence_is_not_dropped() {
        let binary = event(
            EventClass::Correspondence(CorrespondenceKind::Message),
            Body::Binary(vec![0; 12]),
        );
        assert!(render_feed_line(&binary)
            .expect("binary correspondence is admitted")
            .ends_with("⟨binary 12 bytes⟩"));
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
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
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

    // what this catches (card d61513e4): the feed line spending its width on full
    // UUIDs. A Claude Code monitor surfaces ONE line per event and cuts it at 500
    // characters INCLUDING the prefix (measured: four truncated notifications all at
    // exactly 500). A full sender + room is 87 of those before any content — 17% of
    // the reader's view spent on two identifiers nobody reads digit-by-digit, and the
    // shortening moves surviving message from 413 characters to 469.
    //
    // Reader-dependent: a peer on a different runtime reported all markers of a
    // 1,661-char probe visible, so her cut is far wider. Nobody is made worse off by a
    // shorter prefix, which is why this lands on its own merits; the line's STRUCTURE
    // should not be redesigned around one reader's budget.
    //
    // Why it matters, measured 2026-09-21: a 922-char peer message whose ASK began at
    // char 830 was truncated three separate times and the answer never reached the
    // person who asked. Everyone writes context first and the request last, so a
    // prefix tax is paid precisely in the half that carries the point.
    //
    // Mutation check: restoring `event.peer_id` / `event.room_id` fails the bound.
    #[test]
    fn the_feed_line_prefix_stays_short_so_the_message_survives_a_cut() {
        use airc_core::{
            ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
        };
        let event = TranscriptEvent {
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text("the ask lives at the end of a long message")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        let line = render_feed_line(&event).expect("message renders");

        let prefix = line
            .find("the ask lives")
            .expect("the body is present in the line");
        assert!(
            prefix <= 48,
            "the prefix before the message is {prefix} chars; it must stay short so a \
             truncating reader still sees what was said: {line}"
        );

        // The ids are still there — shortened, not dropped. A reader must be able to
        // tell two peers apart in the feed; they just do not need 36 characters to.
        let sender = event.peer_id.to_string();
        let room = event.room_id.to_string();
        assert!(line.contains(&sender[..8]), "sender is identified: {line}");
        assert!(line.contains(&room[..8]), "room is identified: {line}");
        assert!(
            !line.contains(&sender) && !line.contains(&room),
            "no full UUID belongs in a width-budgeted line: {line}"
        );
    }

    // Card d61513e4, second half: wherever a reader cuts this line, the handle must already
    // be in hand. The bound above keeps the whole prefix under the measured 500-char cut;
    // this pins that the handle sits INSIDE that prefix - before the first character of what
    // was said - so a notification truncated to 300 or 400 chars still names how to fetch
    // the rest.
    // what this catches (card 17db7a1a): a message addressed to one peer rendered as room
    // traffic, so its reader could not tell "to me", "to someone else" and "to everyone"
    // apart. Each address renders as itself; a broadcast is unchanged.
    #[test]
    fn a_feed_line_shows_who_the_event_was_addressed_to() {
        use airc_core::{ClientId, EventId, Headers, PeerId, RoomId, TranscriptKind};
        let room = RoomId::new();
        let event = |target: MentionTarget| TranscriptEvent {
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            event_id: EventId::new(),
            room_id: room,
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target,
            headers: Headers::new(),
            body: Some(Body::text("hi")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        let r = short(&room);
        let all = render_feed_line(&event(MentionTarget::All)).expect("renders");
        assert!(all.contains(&format!(" → {r} e=")), "{all}");
        let kimi = PeerId::new();
        let dm = render_feed_line(&event(MentionTarget::Peer(kimi))).expect("renders");
        assert!(dm.contains(&format!(" → @{}/{r} e=", short(&kimi))), "{dm}");
        let other = RoomId::new();
        let cross = render_feed_line(&event(MentionTarget::Room(other))).expect("renders");
        assert!(
            cross.contains(&format!(" → {r}→{} e=", short(&other))),
            "{cross}"
        );
    }

    #[test]
    fn truncated_line_keeps_its_event_handle() {
        use airc_core::{
            ClientId, EventId, Headers, MentionTarget, PeerId, RoomId, TranscriptKind,
        };
        let event = TranscriptEvent {
            class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
            // Deterministic id whose first 8 hex chars are "0badcafe".
            event_id: EventId::from_u128(0x0bad_cafe_0000_0000_dead_beef_0000_0000),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 0,
            lamport: 0,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text("the ask lives at the end of a long message")),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        };
        let line = render_feed_line(&event).expect("message renders");
        let body_start = line
            .find("the ask lives")
            .expect("body present in the line");

        // The handle is before the body - a cut anywhere in the message leaves it intact.
        let prefix = &line[..body_start];
        assert!(
            prefix.contains("e=0badcafe"),
            "handle precedes the body: {line}"
        );

        // And it stays short-form: no full UUID belongs in a width-budgeted line.
        let full = event.event_id.to_string();
        assert!(
            !line.contains(&full),
            "no full event id in the line: {line}"
        );
        assert!(
            prefix.len() <= 48,
            "prefix with handle stays under the cut: {prefix}"
        );
    }
}
