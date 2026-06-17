//! airc-acp-bridge — make any ACP-speaking agent a citizen on the airc grid.
//!
//! Adapter outlier #2 (see ../README.md). The bridge is simultaneously an
//! airc citizen (links `airc-lib`: join / subscribe / publish, grounded by
//! `publish_identity`) and — in later slices — an ACP client driving the agent
//! over JSON-RPC/stdio.
//!
//! ## Slices
//! - **Slice 1 (this file):** the airc-citizen loop. Joins a room, subscribes,
//!   and for each inbound message calls a TURN HANDLER, posting any reply. The
//!   handler is a closure so slice 3 swaps the stub for the real
//!   `ai/should-respond` handler (delegating to the ACP agent) with zero loop
//!   change. The stub is conservative — it PASSES on everything except an
//!   explicit `/acp-ping` (so even slice 1 is not a chatty echo-bot, honouring
//!   no-rust-gates: the decision to speak is the handler's, not the loop's).
//! - **Slice 2:** spawn the ACP agent subprocess; JSON-RPC `initialize` /
//!   `session/new` / `session/prompt` / stream `session/update`.
//! - **Slice 3:** the turn handler becomes the registered `ai/should-respond`
//!   handler for this ACP citizen's lane, returning the `Decision` wire enum.

use std::sync::Arc;

use airc_core::{PeerId, TranscriptEvent, TranscriptKind};
use airc_lib::Airc;
use futures::StreamExt;

/// A turn handler: given the inbound message text, decide what (if anything)
/// to say back. `None` = PASS (stay quiet) — the decision lives HERE, never in
/// the loop. Slice 3 replaces the stub with the ACP-delegating
/// `ai/should-respond` handler.
type TurnHandler = dyn Fn(&str) -> Option<String> + Send + Sync;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent = std::env::var("ACP_BRIDGE_AGENT").unwrap_or_else(|_| "acp-agent".to_string());
    let room = std::env::var("ACP_BRIDGE_ROOM").unwrap_or_else(|_| "general".to_string());
    let home = std::env::var("AIRC_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".to_string());
            std::path::Path::new(&base).join(".airc")
        });

    // Attach to a running daemon when a socket is given (live grid); otherwise
    // open an in-process scope. Named so the bridge is a grounded citizen.
    let airc = match std::env::var("AIRC_SOCKET").ok() {
        Some(socket) => Airc::attach_as(home, &agent, socket).await?,
        None => Airc::open_as(home, &agent).await?,
    };
    airc.publish_identity().await?; // ground by name (room_roster + whois see it)
    airc.join(&room).await?;
    let me = airc.peer_id();
    eprintln!("airc-acp-bridge: '{agent}' joined #{room} as {me} (slice 1: stub turn handler)");

    // Slice-1 stub: conservative PASS-everything except an explicit ping, so the
    // round-trip is smoke-testable without the bridge being a chatty echo.
    let turn: Arc<TurnHandler> = Arc::new(|incoming: &str| {
        if incoming.trim() == "/acp-ping" {
            Some("acp-bridge alive (slice 1 stub — ACP client lands in slice 2)".to_string())
        } else {
            None
        }
    });

    run_bridge(&airc, me, turn.as_ref()).await
}

/// The airc-citizen loop: subscribe to the current room and, for each inbound
/// message from another peer, run `turn` and post any reply. Factored out of
/// `main` so the turn handler is injectable (tests, and the slice-3 handler
/// swap).
async fn run_bridge(
    airc: &Airc,
    me: PeerId,
    turn: &TurnHandler,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut stream = airc.subscribe().await?;
    while let Some(item) = stream.next().await {
        match item {
            Ok(event) => {
                let Some(text) = inbound_text(&event, me) else {
                    continue;
                };
                if let Some(reply) = turn(text) {
                    airc.say(&reply).await?;
                }
            }
            Err(lag) => eprintln!("airc-acp-bridge: live stream lagged: {lag}"),
        }
    }
    Ok(())
}

/// Pure inbound filter: the message text iff this event is a chat MESSAGE from
/// ANOTHER peer with non-empty text. `None` for our own echoes, lifecycle
/// events, and empty bodies — so the bridge never replies to itself or to
/// substrate noise.
fn inbound_text(event: &TranscriptEvent, me: PeerId) -> Option<&str> {
    if event.peer_id == me {
        return None; // never react to our own posts
    }
    if event.kind != TranscriptKind::Message {
        return None; // chat only; skip lifecycle/presence/etc.
    }
    let text = event.body.as_ref()?.as_text()?.trim();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
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
    fn extracts_text_from_another_peers_message() {
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert_eq!(inbound_text(&msg(other, "hi"), me), Some("hi"));
    }

    #[test]
    fn never_reacts_to_own_posts() {
        // The orphan of bots: echoing yourself into a loop. Pinned shut.
        let me = PeerId::from_u128(1);
        assert_eq!(inbound_text(&msg(me, "my own post"), me), None);
    }

    #[test]
    fn skips_non_message_kinds() {
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        let mut ev = msg(other, "ignored");
        ev.kind = TranscriptKind::Presence;
        assert_eq!(inbound_text(&ev, me), None);
    }

    #[test]
    fn skips_empty_or_whitespace_bodies() {
        let me = PeerId::from_u128(1);
        let other = PeerId::from_u128(2);
        assert_eq!(inbound_text(&msg(other, "   "), me), None);
    }
}
