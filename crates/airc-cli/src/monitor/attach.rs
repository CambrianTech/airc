use std::error::Error;
use std::path::Path;

use airc_core::{Body, MentionTarget, TranscriptEvent, TranscriptKind};
use airc_daemon::{AttachRequest, DaemonClient, Response, SubscribeRequest};
use airc_lib::Airc;
use airc_protocol::HEADER_AIRC_CLIENT;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::render::{normalize_channel, xml_escape, Sandbox};
use crate::client_id::current_client_id;

pub(crate) async fn run(home: &Path, _my_name: &str) -> Result<(), Box<dyn Error>> {
    let airc = Airc::open(home).await?;
    let client_id = current_client_id().ok().flatten();
    let socket = crate::cli::default_socket_path_in(home);
    let client = DaemonClient::new(socket);
    let set = airc.subscription_set().await?;
    for subscription in set.all() {
        client
            .subscribe(SubscribeRequest {
                wire: subscription.as_room().wire,
            })
            .await?;
    }
    let stream = client.attach(AttachRequest::default()).await?;
    let mut reader = BufReader::new(stream);
    let mut sandbox = Sandbox::new();

    println!("airc: attached to Rust event stream for subscribed channels");
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).await? == 0 {
            return Ok(());
        }
        let response: Response = serde_json::from_str(line.trim_end_matches('\n'))?;
        match response {
            Response::Ok => {}
            Response::Event { event } => {
                if matches!(event.kind, TranscriptKind::Message | TranscriptKind::System) {
                    render_event(event.as_ref(), client_id.as_deref(), &mut sandbox);
                }
            }
            Response::Error { message } => return Err(message.into()),
            Response::Pong | Response::Status(_) | Response::Inbox(_) | Response::Peers(_) => {}
        }
    }
}

fn render_event(event: &TranscriptEvent, client_id: Option<&str>, sandbox: &mut Sandbox) {
    if is_own_runtime_event(event, client_id) {
        return;
    }

    let Some(body) = event.body.as_ref().and_then(body_text) else {
        return;
    };

    sandbox.emit_contract_once();
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
