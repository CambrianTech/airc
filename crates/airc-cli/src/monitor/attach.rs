use std::error::Error;
use std::io::Write;
use std::path::Path;

use std::time::Duration;

use airc_core::{Body, MentionTarget, RoomId, TranscriptEvent, TranscriptKind};
use airc_ipc::{codec::read_frame, AttachRequest, DaemonClient, IpcCursor, Response};
use airc_lib::{decode_wire_event_with_cursor, Airc};
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
    /// Card 16bd4d71 slice 1: emitted by the per-channel attach loop
    /// when it detects the IPC stream went away (daemon stop, socket
    /// gone, EOF). The main rendering loop surfaces it as ONE
    /// "airc: daemon disconnected — reconnecting..." stdout line. The
    /// per-channel task then enters exponential backoff (1s → 2s →
    /// 5s → 10s → 30s cap) and re-attaches with `from=last-seen-cursor`
    /// AND `coalesce_backlog=true` so the gap replays as a single
    /// summary frame, not a flood of historical events.
    Disconnected {
        channel: RoomId,
    },
    /// Card 16bd4d71 slice 1: emitted after a successful re-attach
    /// when the per-channel task has just resumed from a saved
    /// cursor. The main rendering loop surfaces it as ONE
    /// "airc: reconnected (resumed from cursor X)" stdout line.
    Reconnected {
        channel: RoomId,
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
            run_channel_attach_loop(socket, channel, tx, from_now, coalesce_backlog).await;
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
            MonitorFrame::Disconnected { channel } => {
                // Card 16bd4d71 slice 1: ONE stdout line per channel
                // when the daemon stream EOFs. The per-channel task
                // is already in exponential backoff trying to reconnect.
                println!(
                    "airc: daemon disconnected — reconnecting (channel {})",
                    channel.0
                );
                std::io::stdout().flush()?;
            }
            MonitorFrame::Reconnected { channel } => {
                // Card 16bd4d71 slice 1: ONE stdout line per channel
                // after the auto-reconnect re-attaches. The cursor-resume
                // means the gap that arrived during the disconnect
                // surfaces as a single `CatchUpSummary` frame
                // immediately after — operators see "disconnected →
                // reconnected → caught up N events" in three lines
                // regardless of how many events accumulated in the gap.
                println!("airc: reconnected (channel {})", channel.0);
                std::io::stdout().flush()?;
            }
        }
    }
    Ok(())
}

/// Card 16bd4d71 slice 1: the per-channel attach loop with auto-reconnect
/// on daemon EOF + cursor-resume so the gap during disconnect replays
/// without loss or duplicate.
///
/// First connect uses the caller's `from_now` / `coalesce_backlog`
/// flags. On EOF, the loop tracks `last_cursor` from each delivered
/// event + each `AttachCursorAdvanced` summary, then re-attaches with
/// `from=last_cursor` + `coalesce_backlog=true` so the daemon's
/// `subscribe_with_lag` resumes strictly after the last-seen event
/// and the gap surfaces as ONE summary frame (not a per-event flood).
///
/// Backoff schedule: 1s → 2s → 5s → 10s → 30s cap. Reset to 0 on
/// successful attach. The `Disconnected` + `Reconnected` MonitorFrames
/// surface as one stdout line each so the operator sees the lifecycle
/// without ambiguity.
async fn run_channel_attach_loop(
    socket: std::path::PathBuf,
    channel: RoomId,
    tx: tokio::sync::mpsc::Sender<MonitorFrame>,
    initial_from_now: bool,
    initial_coalesce_backlog: bool,
) {
    let client = DaemonClient::new(socket);
    let mut last_cursor: Option<IpcCursor> = None;
    let mut backoff = Backoff::new();
    let mut is_first_connect = true;

    loop {
        if !is_first_connect {
            // Tell the main render loop we lost the daemon — emit
            // BEFORE the backoff sleep so the operator sees the
            // disconnect immediately, not after the sleep.
            if tx
                .send(MonitorFrame::Disconnected { channel })
                .await
                .is_err()
            {
                return;
            }
            tokio::time::sleep(backoff.current()).await;
        }

        // Build the attach request — first connect honors caller
        // flags; reconnects always use cursor-resume + coalesce so
        // the gap doesn't flood.
        let request = if let Some(cursor) = last_cursor {
            AttachRequest {
                channel: Some(channel),
                from: Some(cursor),
                from_now: false, // we have a cursor; resume from it
                coalesce_backlog: true,
                ..Default::default()
            }
        } else {
            AttachRequest {
                channel: Some(channel),
                from: None,
                from_now: initial_from_now,
                coalesce_backlog: initial_coalesce_backlog,
                ..Default::default()
            }
        };

        let mut stream = match client.attach(request).await {
            Ok(s) => s,
            Err(_) => {
                backoff.advance();
                continue;
            }
        };

        // Successful attach — emit Reconnected if this isn't the
        // first connect, then reset backoff.
        if !is_first_connect
            && tx
                .send(MonitorFrame::Reconnected { channel })
                .await
                .is_err()
        {
            return;
        }
        is_first_connect = false;
        backoff.reset();

        // Stream loop — read frames until EOF or send-fail.
        while let Ok(Some(response)) = read_frame::<_, Response>(&mut stream).await {
            match response {
                Response::Event { envelope } => match decode_wire_event_with_cursor(envelope) {
                    Ok((event, cursor)) => {
                        last_cursor = Some(cursor);
                        if tx.send(MonitorFrame::Event(Box::new(event))).await.is_err() {
                            return;
                        }
                    }
                    Err(_) => return,
                },
                Response::AttachCursorAdvanced {
                    skipped,
                    advanced_to,
                } => {
                    last_cursor = Some(advanced_to);
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

        // Stream ended — fall through to the top of the loop, which
        // will emit Disconnected and back off before re-attaching.
        backoff.advance();
    }
}

/// Card 16bd4d71 slice 1: exponential backoff for the per-channel
/// attach loop. Sequence: 1s → 2s → 5s → 10s → 30s cap, reset to 0
/// (immediate retry on first connect) on successful attach.
struct Backoff {
    current_ms: u64,
}

impl Backoff {
    fn new() -> Self {
        Self { current_ms: 0 }
    }

    fn current(&self) -> Duration {
        Duration::from_millis(self.current_ms)
    }

    fn advance(&mut self) {
        self.current_ms = match self.current_ms {
            0 => 1_000,
            1_000 => 2_000,
            2_000 => 5_000,
            5_000 => 10_000,
            _ => 30_000,
        };
    }

    fn reset(&mut self) {
        self.current_ms = 0;
    }
}

#[cfg(test)]
mod backoff_tests {
    use super::Backoff;
    use std::time::Duration;

    /// Card 16bd4d71 slice 1: the exponential-backoff schedule MUST
    /// stay stable — operators reading the disconnect log expect
    /// 1s → 2s → 5s → 10s → 30s and assume retry happens within
    /// 30s once the daemon comes back. Drift here changes the
    /// recovery story without a test catching it.
    #[test]
    fn backoff_schedule_is_stable() {
        let mut b = Backoff::new();
        assert_eq!(b.current(), Duration::ZERO, "first connect = no wait");
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(1));
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(2));
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(5));
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(10));
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(30));
        // Capped: subsequent advances stay at 30s, don't keep growing.
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(30));
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(30));
    }

    /// Card 16bd4d71 slice 1: reset MUST clear back to 0 so the next
    /// disconnect after a successful reconnect starts the schedule
    /// over (a transient daemon hiccup shouldn't penalize the next
    /// unrelated bounce minutes later).
    #[test]
    fn backoff_reset_returns_to_zero() {
        let mut b = Backoff::new();
        b.advance();
        b.advance();
        b.advance();
        assert_ne!(b.current(), Duration::ZERO);
        b.reset();
        assert_eq!(b.current(), Duration::ZERO);
        // Post-reset, advance restarts the schedule from 1s.
        b.advance();
        assert_eq!(b.current(), Duration::from_secs(1));
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
