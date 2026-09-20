//! airc-acp-bridge — make any ACP-speaking agent a citizen on the airc grid.
//!
//! Adapter outlier #2 (see ../README.md). The bridge is simultaneously an
//! airc citizen (links `airc-lib`: join / subscribe / publish, grounded by
//! `publish_identity`) and an ACP client driving the agent over JSON-RPC/stdio.
//!
//! ## Slices
//! - **Slice 1:** the airc-citizen loop. Joins a room, subscribes, and for each
//!   inbound message calls a TURN HANDLER, posting whatever it returns.
//! - **Slice 2 (this file):** the handler is backed by [`AcpBridge`], which
//!   spawns the real agent and drives initialize / session/new / session/prompt.
//!   The protocol itself comes from the published `agent-client-protocol` SDK
//!   rather than hand-written framing — see `docs/architecture/ACP-CLIENT-BRIDGE.md`
//!   for why that supersedes this README's original slice-2 plan.
//! - **Slice 3:** the trigger below becomes the registered `ai/should-respond`
//!   handler for this ACP citizen's lane, returning the `Decision` wire enum.

use std::sync::Arc;

use airc_acp::{AcpBridge, AgentSpec, BridgeError, PeerPolicy, ToolPolicy};
use airc_core::{PeerId, TranscriptEvent, TranscriptKind};
use airc_lib::Airc;
use futures::StreamExt;

/// The slice-2 trigger prefix.
///
/// This is a SCAFFOLD, not the design. A bridge that answered every message
/// would be an echo bot in a shared room, and the no-rust-gates doctrine says
/// the decision to speak belongs to cognition (slice 3's `ai/should-respond`),
/// not to an `if` in the loop. Until that lands, an explicit trigger is the
/// honest placeholder: it is obviously a stand-in rather than a gate pretending
/// to be judgment, and it makes the live round-trip deterministic to test.
///
/// ## Why `@acp` and not `/acp`
///
/// It WAS `/acp`, until a live test on Windows showed the message arriving as
/// `C:/Program Files/Git/acp hello …`. Git Bash's MSYS path conversion rewrites
/// a leading `/token` into a Windows path, silently, before airc ever sees it.
/// Every agent driving airc from a shell — which is most of them — would have
/// hit that, and the symptom is the worst kind: the bridge behaves perfectly and
/// simply never answers, because the trigger it was watching for never arrived.
///
/// `@` is not path-like on any platform, and it reads as addressing, which is
/// what this actually is. (airc has no structural addressing — every broadcast
/// is `MentionTarget::All` — so a textual convention is what we have to work
/// with either way.)
const TRIGGER: &str = "@acp";

/// A turn handler: given who spoke and what they said, produce the lines to
/// publish, in order. Empty = PASS (stay quiet) — the decision lives HERE, never
/// in the loop.
type TurnHandler =
    Arc<dyn Fn(String, String) -> futures::future::BoxFuture<'static, Vec<String>> + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let agent_name = std::env::var("ACP_BRIDGE_AGENT").unwrap_or_else(|_| "acp-agent".to_string());
    let room = std::env::var("ACP_BRIDGE_ROOM").unwrap_or_else(|_| "general".to_string());
    let home = std::env::var("AIRC_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let base = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".to_string());
            std::path::Path::new(&base).join(".airc")
        });

    // The command that launches the agent in ACP mode, e.g.
    // `uvx hermes-agent[acp] hermes-acp`. Required: a bridge with no agent is not
    // a degraded bridge, it is a misconfiguration, and it should say so at
    // startup rather than go quiet on the first message.
    let command = std::env::var("ACP_BRIDGE_COMMAND").map_err(|_| {
        "ACP_BRIDGE_COMMAND is not set — it must be the command that starts the ACP agent \
         (e.g. ACP_BRIDGE_COMMAND='uvx hermes-agent[acp] hermes-acp'). \
         Without it there is no agent to bridge to."
    })?;

    let mut spec = AgentSpec::talk_only(&command);
    // Opt-in widening, both deny-shaped by default. See ACP-CLIENT-BRIDGE.md:
    // ACP_BRIDGE_TOOLS lists KINDS (read, edit, execute, …), not tool names.
    if let Ok(tools) = std::env::var("ACP_BRIDGE_TOOLS") {
        spec = spec.with_tools(ToolPolicy::allow(tools.split(',')));
    }
    if let Ok(peers) = std::env::var("ACP_BRIDGE_ALLOW_FROM") {
        spec = spec.with_peers(PeerPolicy::only(
            peers.split(',').map(|p| p.trim().to_string()),
        ));
    }
    if let Ok(ws) = std::env::var("ACP_BRIDGE_WORKSPACE") {
        spec = spec.with_workspace(ws);
    }

    let bridge = Arc::new(AcpBridge::new(spec));

    // Attached vs isolated is the difference between "in the room" and "in a
    // room of its own", and it is invisible from the outside: both paths log a
    // successful join and hand back a peer id.
    //
    // A live test on Windows lost twenty minutes to exactly that — the bridge
    // reported joining #general, and `airc peers` on the real grid had never
    // heard of it, because `open_as` opens an isolated in-process scope. So say
    // which one this is, out loud, at startup.
    let airc = match std::env::var("AIRC_SOCKET").ok() {
        Some(socket) => {
            eprintln!("airc-acp-bridge: attaching to the running daemon at {socket}");
            Airc::attach_as(home, &agent_name, socket).await?
        }
        None => {
            eprintln!(
                "airc-acp-bridge: AIRC_SOCKET is unset — opening an ISOLATED in-process scope. \
                 This bridge will NOT see traffic from a running airc daemon, and peers on the \
                 live grid will not see it. Set AIRC_SOCKET to join the real grid."
            );
            Airc::open_as(home, &agent_name).await?
        }
    };
    airc.publish_identity().await?; // ground by name (room_roster + whois see it)
    airc.join(&room).await?;
    let me = airc.peer_id();
    eprintln!(
        "airc-acp-bridge: '{agent_name}' joined #{room} as {me}; agent = `{command}`; \
         say `{TRIGGER} <prompt>` to reach it"
    );

    let turn: TurnHandler = {
        let bridge = Arc::clone(&bridge);
        Arc::new(move |peer: String, text: String| {
            let bridge = Arc::clone(&bridge);
            Box::pin(async move { acp_turn(&bridge, &peer, &text).await })
        })
    };

    run_bridge(&airc, me, &turn).await
}

/// Run one ACP turn and render it as room lines.
///
/// The error mapping is the load-bearing part:
///
/// - `PeerRefused` → **silence**. It is a configured policy, and announcing it
///   would let an unauthorized peer make the bridge post on demand.
/// - `AgentUnavailable` / `Exchange` → **spoken**. These are failures the room
///   must see. An agent that died must produce a visible failure rather than
///   silently stopping — "up is not the same as talking", and a bridge that goes
///   quiet on a crash is indistinguishable from one with nothing to say.
async fn acp_turn(bridge: &AcpBridge, peer: &str, text: &str) -> Vec<String> {
    match bridge.prompt(peer, text).await {
        Ok(turn) => {
            let mut lines = Vec::new();
            let reply = turn.text.trim();
            if !reply.is_empty() {
                lines.push(reply.to_string());
            }
            // Refusals ride out with the reply; approvals stay quiet.
            lines.extend(turn.refusal_lines());
            if lines.is_empty() {
                // The agent completed and said nothing. Say THAT, rather than
                // leaving the room unable to tell it apart from a dead bridge.
                lines.push("(the agent completed its turn without producing any text)".to_string());
            }
            lines
        }
        Err(BridgeError::PeerRefused { .. }) => Vec::new(),
        Err(e) => vec![format!("ACP bridge could not answer: {e}")],
    }
}

/// The airc-citizen loop: subscribe to the current room and, for each inbound
/// message from another peer, run `turn` and post whatever it returns. Factored
/// out of `main` so the turn handler is injectable (tests, and the slice-3
/// handler swap).
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
                let Some(prompt) = triggered_prompt(text) else {
                    continue;
                };
                for line in turn(event.peer_id.to_string(), prompt.to_string()).await {
                    airc.say(&line).await?;
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

/// The prompt carried by a triggered message, or `None` when this message was
/// not addressed to the agent.
///
/// Pure so the trigger rule is testable without a room, and so slice 3 has one
/// obvious function to delete when `ai/should-respond` takes over.
fn triggered_prompt(text: &str) -> Option<&str> {
    let rest = text.strip_prefix(TRIGGER)?;
    // Require a separator so `/acpfoo` isn't read as a trigger for `foo`.
    let prompt = match rest.chars().next() {
        None => "",
        Some(c) if c.is_whitespace() => rest.trim(),
        Some(_) => return None,
    };
    if prompt.is_empty() {
        None
    } else {
        Some(prompt)
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

    /// what this catches: the bridge turning into an echo bot. Ordinary room
    /// chatter must NOT reach the agent — every message that did would be a
    /// subprocess spawn and, on a paid backend, a bill.
    #[test]
    fn ordinary_chatter_does_not_reach_the_agent() {
        assert_eq!(
            triggered_prompt("what do you all think about the merge?"),
            None
        );
        assert_eq!(triggered_prompt("the @acp thing is neat"), None);
    }

    #[test]
    fn a_triggered_message_yields_just_the_prompt() {
        assert_eq!(
            triggered_prompt("@acp summarize the diff"),
            Some("summarize the diff")
        );
        assert_eq!(triggered_prompt("@acp   padded  "), Some("padded"));
    }

    /// what this catches: a prefix match swallowing an unrelated word. `@acpfoo`
    /// is not the trigger, and treating it as one would make a typo silently
    /// spend an agent turn.
    #[test]
    fn the_trigger_requires_a_separator() {
        assert_eq!(triggered_prompt("@acpfoo bar"), None);
    }

    /// regression: the trigger used to be `/acp`, and a live Windows test showed
    /// Git Bash rewriting it to `C:/Program Files/Git/acp ...` via MSYS path
    /// conversion before airc ever saw it. The bridge then behaved perfectly and
    /// never answered, which is the most expensive way for this to fail.
    /// what this catches: anyone reintroducing a path-like trigger.
    #[test]
    fn the_trigger_is_not_path_like() {
        assert!(
            !TRIGGER.starts_with('/'),
            "a leading-slash trigger is rewritten by MSYS path conversion on Windows"
        );
        // And the mangled form must not accidentally still fire.
        assert_eq!(triggered_prompt("C:/Program Files/Git/acp hello"), None);
    }

    /// what this catches: a bare trigger spawning an agent with an empty prompt.
    #[test]
    fn a_bare_trigger_is_not_a_prompt() {
        assert_eq!(triggered_prompt("@acp"), None);
        assert_eq!(triggered_prompt("@acp    "), None);
    }
}
