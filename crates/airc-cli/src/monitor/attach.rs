use std::error::Error;
use std::io::Write;
use std::path::Path;

use airc_core::{Body, MentionTarget, RoomId, TranscriptEvent, TranscriptKind};
use airc_ipc::{codec::read_frame, AttachRequest, DaemonClient, Response};
use airc_lib::{decode_wire_event, Airc};
use airc_protocol::HEADER_AIRC_CLIENT;

use super::render::{normalize_channel, xml_escape, Sandbox};
use crate::client_id::current_client_id;
use crate::work_suggestions::render_claimable_work_for_event;

/// One frame to surface on stdout — either a decoded transcript event
/// or a one-shot summary line (the card-7d5b6a65 catch-up coalesce).
///
/// `Event` is boxed because `TranscriptEvent` is ~400 bytes (headers,
/// body, signature, etc.) while `CatchUpSummary` is ~24 bytes; without
/// the box every enum slot is sized for the largest variant and the
/// channel buffer balloons. Box-on-the-heavy-variant is the standard
/// fix and matches clippy's `large_enum_variant` lint guidance.
enum MonitorFrame {
    Event(Box<TranscriptEvent>),
    /// Card 7d5b6a65 summary frame. Carries the daemon's catch-up
    /// summary so the merger that joins multiple per-channel streams
    /// can render it on the main thread without extra plumbing.
    CatchUpSummary {
        channel: RoomId,
        skipped: u64,
    },
}

pub(crate) async fn run(
    home: &Path,
    _my_name: &str,
    from_now: bool,
    coalesce_backlog: bool,
) -> Result<(), Box<dyn Error>> {
    let airc = Airc::open(home).await?;
    let client_id = current_client_id().ok().flatten();
    let socket = crate::cli::default_socket_path_in(home);
    let set = airc.subscription_set().await?;
    let channels: Vec<RoomId> = set.all().map(|sub| sub.as_room().channel).collect();

    // The owner-core router subscribes per channel; the monitor opens one
    // attach stream per subscribed room and merges them into a single
    // feed. Each daemon `Event` carries airc-wire bytes — decoded once
    // here via the shared projection.
    //
    // Card 7d5b6a65: `from_now=true` (the default) asks the daemon to
    // skip transcript replay and start at the live edge — no backlog
    // flood. `coalesce_backlog=true` (only meaningful when `from_now`
    // is false) asks the daemon to collapse the catch-up phase into a
    // single `AttachCursorAdvanced` summary frame which the renderer
    // surfaces as ONE stdout line instead of N historical events.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MonitorFrame>(1024);
    for channel in channels {
        let socket = socket.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let client = DaemonClient::new(socket);
            let mut stream = match client
                .attach(AttachRequest {
                    channel: Some(channel),
                    from: None,
                    from_now,
                    coalesce_backlog,
                    ..Default::default()
                })
                .await
            {
                Ok(stream) => stream,
                Err(_) => return,
            };
            while let Ok(Some(response)) = read_frame::<_, Response>(&mut stream).await {
                match response {
                    Response::Event { envelope } => match decode_wire_event(envelope) {
                        Ok(event) => {
                            if tx.send(MonitorFrame::Event(Box::new(event))).await.is_err() {
                                return;
                            }
                        }
                        Err(_) => return,
                    },
                    Response::AttachCursorAdvanced { skipped, .. }
                        if tx
                            .send(MonitorFrame::CatchUpSummary { channel, skipped })
                            .await
                            .is_err() =>
                    {
                        return;
                    }
                    _ => {}
                }
            }
        });
    }
    drop(tx);

    let mut sandbox = Sandbox::new();
    println!("airc: attached to Rust event stream for subscribed channels");
    std::io::stdout().flush()?;

    while let Some(frame) = rx.recv().await {
        match frame {
            MonitorFrame::Event(event) => {
                let event = *event;
                if let Some(text) = render_claimable_work_for_event(&airc, &event).await? {
                    sandbox.emit_contract_once();
                    render_text_event(&event, client_id.as_deref(), &text, &mut sandbox);
                } else if matches!(event.kind, TranscriptKind::Message | TranscriptKind::System) {
                    render_event(&event, client_id.as_deref(), &mut sandbox);
                }
            }
            MonitorFrame::CatchUpSummary { channel, skipped } => {
                // ONE stdout line per channel that had backlog to
                // catch up on. Card 7d5b6a65: the substrate
                // contribution to keeping live-tail notification
                // cost bounded regardless of backlog depth.
                println!(
                    "airc: caught up — skipped {} event{} on channel {} during backlog catch-up",
                    skipped,
                    if skipped == 1 { "" } else { "s" },
                    channel.0
                );
                std::io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

fn render_event(event: &TranscriptEvent, client_id: Option<&str>, sandbox: &mut Sandbox) {
    if is_own_runtime_event(event, client_id) {
        return;
    }

    let Some(body) = event.body.as_ref().and_then(body_text) else {
        return;
    };

    sandbox.emit_contract_once();
    render_text_event(event, client_id, body, sandbox);
}

fn render_text_event(
    event: &TranscriptEvent,
    _client_id: Option<&str>,
    body: &str,
    sandbox: &mut Sandbox,
) {
    let channel = normalize_channel(&event.room_id.to_string());
    let mut attrs = vec![
        format!("from=\"{}\"", xml_escape(&event.peer_id.to_string())),
        format!("client=\"{}\"", xml_escape(&display_client(event))),
        format!("channel=\"{}\"", xml_escape(&channel)),
        format!("ts=\"{}\"", event.occurred_at_ms),
    ];
    match &event.target {
        MentionTarget::All => {}
        MentionTarget::Peer(peer_id) => {
            attrs.push(format!("to=\"{}\"", xml_escape(&peer_id.to_string())))
        }
        MentionTarget::Room(room_id) => {
            attrs.push(format!("to=\"{}\"", xml_escape(&room_id.to_string())))
        }
    }

    println!(
        "<pm-{nonce} {attrs}>{body}</pm-{nonce}>",
        nonce = sandbox.nonce,
        attrs = attrs.join(" "),
        body = xml_escape(body)
    );
}

fn is_own_runtime_event(event: &TranscriptEvent, client_id: Option<&str>) -> bool {
    let Some(client_id) = client_id else {
        return false;
    };
    if let Some(event_client) = event.headers.get(HEADER_AIRC_CLIENT) {
        return event_client == client_id;
    }
    event.client_id.to_string() == client_id
}

fn display_client(event: &TranscriptEvent) -> String {
    event
        .headers
        .get(HEADER_AIRC_CLIENT)
        .cloned()
        .unwrap_or_else(|| event.client_id.to_string())
}

fn body_text(body: &Body) -> Option<&str> {
    body.as_text()
}

#[cfg(test)]
mod tests {
    use airc_core::{Body, ClientId, EventId, Headers, PeerId, RoomId, TranscriptEvent};

    use super::*;

    #[test]
    fn body_text_reads_plain_chat_shape() {
        let body = Body::text("hello");

        assert_eq!(body_text(&body), Some("hello"));
    }

    #[test]
    fn render_event_filters_same_client() {
        let event = event("hello");
        let mut sandbox = Sandbox::new();

        render_event(&event, Some(&event.client_id.to_string()), &mut sandbox);

        assert!(!sandbox.has_emitted());
    }

    #[test]
    fn render_event_filters_same_runtime_client_header() {
        let mut event = event("hello");
        event
            .headers
            .insert(HEADER_AIRC_CLIENT.to_string(), "codex:thread-1".to_string());
        let mut sandbox = Sandbox::new();

        render_event(&event, Some("codex:thread-1"), &mut sandbox);

        assert!(!sandbox.has_emitted());
    }

    #[test]
    fn render_event_keeps_different_runtime_client_header() {
        let mut event = event("hello");
        event.headers.insert(
            HEADER_AIRC_CLIENT.to_string(),
            "claude:session-1".to_string(),
        );
        let mut sandbox = Sandbox::new();

        render_event(&event, Some("codex:thread-1"), &mut sandbox);

        assert!(sandbox.has_emitted());
    }

    fn event(text: &str) -> TranscriptEvent {
        TranscriptEvent {
            event_id: EventId::new(),
            room_id: RoomId::new(),
            peer_id: PeerId::new(),
            client_id: ClientId::new(),
            kind: TranscriptKind::Message,
            occurred_at_ms: 1,
            lamport: 1,
            target: MentionTarget::All,
            headers: Headers::new(),
            body: Some(Body::text(text)),
            attachment: None,
            receipt: None,
            metadata: serde_json::Value::Null,
        }
    }
}
