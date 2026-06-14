use std::error::Error;
use std::io::Write;
use std::path::Path;

use airc_core::{Body, MentionTarget, RoomId, TranscriptEvent, TranscriptKind};
use airc_ipc::{codec::read_frame, AttachRequest, AttachStart, DaemonClient, Response};
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

/// Receive-resilience (2026-06-14 disconnect): the first re-attach after
/// a healthy stream drops waits this long. Short so a transient IPC blip
/// or a fast daemon restart recovers near-instantly.
const RECONNECT_INITIAL_BACKOFF_MS: u64 = 250;

/// Cap on the reconnect backoff: a daemon that stays down is retried at
/// most this often, so the monitor neither hammers a dead socket nor
/// goes permanently dark.
const RECONNECT_MAX_BACKOFF_MS: u64 = 5_000;

/// One exponential-backoff step for the attach reconnect loop: double,
/// capped at [`RECONNECT_MAX_BACKOFF_MS`]. `saturating_mul` so a runaway
/// value can never overflow-panic.
fn next_reconnect_backoff_ms(current: u64) -> u64 {
    current.saturating_mul(2).min(RECONNECT_MAX_BACKOFF_MS)
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
    // Card 7d5b6a65: `from_now` (the CLI default) maps to
    // `AttachStart::Live` — no backlog flood. Without it the monitor
    // replays the transcript (`FromTranscriptStart`), where
    // `coalesce_backlog` collapses the catch-up phase into a single
    // `AttachCursorAdvanced` summary frame which the renderer surfaces
    // as ONE stdout line instead of N historical events.
    let start = if from_now {
        AttachStart::Live
    } else {
        AttachStart::FromTranscriptStart
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel::<MonitorFrame>(1024);
    for channel in channels {
        let socket = socket.clone();
        let tx = tx.clone();
        tokio::spawn(async move {
            let client = DaemonClient::new(socket);
            // Receive-resilience (2026-06-14 disconnect): re-attach when the
            // stream drops instead of giving up. The original was a single
            // attach that died PERMANENTLY when its daemon was killed/
            // restarted (cargo/test churn, idle self-exit), taking the whole
            // live tail dark with no recovery. We now reconnect with bounded
            // backoff and only stop when the consumer (the merge channel) is
            // gone — then there is nothing left to feed. On reconnect we
            // resume the LIVE tail; events that occurred during the gap are
            // not replayed (cursor-resume is a follow-up). Same-scope daemon
            // restarts reuse the deterministic socket path, so the re-attach
            // lands on the respawned daemon; a cross-SCOPE flip is seam #1's
            // domain, out of scope here.
            let mut backoff_ms = RECONNECT_INITIAL_BACKOFF_MS;
            // The configured `start` (Live or FromTranscriptStart) applies to
            // the FIRST attach only. After we've attached once, reconnects
            // resume LIVE — re-requesting the full transcript on every
            // reconnect would re-flood it event-by-event, and (combined with
            // the decode-error drop below) a single poisoned historical frame
            // would wedge the loop into a permanent re-replay that never
            // advances (sentinel BLOCK on #1203). Trade-off: a first attach
            // that drops mid-backlog forfeits the un-replayed remainder rather
            // than re-flooding; per-attach cursor-resume (replay only the gap)
            // is the follow-up.
            let mut attach_start = start;
            loop {
                let mut request = AttachRequest::new(channel, attach_start);
                if coalesce_backlog {
                    request = request.with_coalesced_backlog();
                }
                if let Ok(mut stream) = client.attach(request).await {
                    let mut got_frame = false;
                    while let Ok(Some(response)) = read_frame::<_, Response>(&mut stream).await {
                        match response {
                            Response::Event { envelope } => match decode_wire_event(envelope) {
                                Ok(event) => {
                                    got_frame = true;
                                    if tx.send(MonitorFrame::Event(Box::new(event))).await.is_err()
                                    {
                                        return;
                                    }
                                }
                                // A single undecodable frame must not kill
                                // the tail — drop the stream and re-attach.
                                Err(_) => break,
                            },
                            Response::AttachCursorAdvanced { skipped, .. } => {
                                got_frame = true;
                                if tx
                                    .send(MonitorFrame::CatchUpSummary { channel, skipped })
                                    .await
                                    .is_err()
                                {
                                    return;
                                }
                            }
                            _ => {}
                        }
                    }
                    // Reset backoff only when this attach actually delivered a
                    // frame — a daemon that accepts then immediately closes
                    // (flap) must keep escalating, not pin at the 250ms floor.
                    if got_frame {
                        backoff_ms = RECONNECT_INITIAL_BACKOFF_MS;
                    }
                    // One attach done → never re-request the backlog.
                    attach_start = AttachStart::Live;
                    // Stream ended (EOF / read error) — fall through to
                    // reconnect.
                }
                // Consumer gone → nothing to feed → stop. Otherwise back off
                // and re-attach (covers both attach-failed and stream-dropped).
                if tx.is_closed() {
                    return;
                }
                eprintln!(
                    "airc: event stream for channel {} dropped — reconnecting in {}ms",
                    channel.0, backoff_ms
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
                backoff_ms = next_reconnect_backoff_ms(backoff_ms);
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

    /// what this catches: the reconnect backoff must double from the
    /// initial step and saturate at the cap — never overflow-panic and
    /// never exceed the cap (which would let a dead daemon go un-retried
    /// for an unbounded time, the exact "permanent dark" this card ends).
    #[test]
    fn reconnect_backoff_doubles_then_caps() {
        assert_eq!(next_reconnect_backoff_ms(RECONNECT_INITIAL_BACKOFF_MS), 500);
        assert_eq!(next_reconnect_backoff_ms(500), 1_000);
        assert_eq!(next_reconnect_backoff_ms(2_000), 4_000);
        // 4_000 * 2 = 8_000, capped to the 5_000 max.
        assert_eq!(next_reconnect_backoff_ms(4_000), RECONNECT_MAX_BACKOFF_MS);
        // At/above the cap it stays pinned.
        assert_eq!(
            next_reconnect_backoff_ms(RECONNECT_MAX_BACKOFF_MS),
            RECONNECT_MAX_BACKOFF_MS
        );
        // Saturating: an absurd value can't overflow-panic.
        assert_eq!(
            next_reconnect_backoff_ms(u64::MAX),
            RECONNECT_MAX_BACKOFF_MS
        );
    }

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
