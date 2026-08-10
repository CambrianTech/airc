//! A real ACP agent, over real stdio — the fixture that proves the SUBPROCESS
//! path.
//!
//! The in-process round-trip tests in `transport.rs` cover the protocol logic,
//! but they never fork anything. Spawning a child and speaking JSON-RPC over its
//! stdin/stdout is a separate risk surface, and on Windows it is the one that
//! historically breaks quietly: a child that fails to start, or a stdout that
//! isn't clean UTF-8, presents as an agent that simply never answers.
//!
//! So this binary exists to be spawned. It is deliberately tiny and has no
//! model, no network, and no configuration — if a test using it fails, the
//! failure is in spawn/framing, not in someone's inference backend.
//!
//! Gated behind the `test-fixtures` feature so a production build physically
//! cannot link it.
//!
//! ## Behaviour
//!
//! - Any prompt → replies `echo: <prompt>`.
//! - A prompt starting with `tool:<kind>` → first asks the client for permission
//!   to run a tool of that kind, then reports the verdict it got back. This is
//!   what lets a test assert that a REFUSAL crossed a process boundary.
//! - A prompt of exactly `die` → exits the process mid-turn, so the "a killed
//!   agent is visible, not silent" case can be tested rather than asserted.
//!
//! ## stdout is the protocol
//!
//! ACP agents reserve stdout for JSON-RPC framing. Anything this binary prints
//! for humans MUST go to stderr — a stray `println!` here would corrupt the
//! stream and produce exactly the confusing "agent went quiet" symptom this
//! fixture is meant to catch.

use agent_client_protocol::schema::v1::{
    AgentCapabilities, ContentBlock, ContentChunk, InitializeRequest, InitializeResponse,
    NewSessionRequest, NewSessionResponse, PermissionOption, PermissionOptionId,
    PermissionOptionKind, PromptRequest, PromptResponse, RequestPermissionRequest, SessionId,
    SessionNotification, SessionUpdate, StopReason, TextContent, ToolCallId, ToolCallUpdate,
    ToolCallUpdateFields, ToolKind,
};
use agent_client_protocol::{Agent, Client, ConnectionTo, Responder, Stdio};

fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read" => ToolKind::Read,
        "edit" => ToolKind::Edit,
        "delete" => ToolKind::Delete,
        "move" => ToolKind::Move,
        "search" => ToolKind::Search,
        "execute" => ToolKind::Execute,
        "think" => ToolKind::Think,
        "fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

fn option(id: &str, kind: PermissionOptionKind) -> PermissionOption {
    PermissionOption::new(
        PermissionOptionId::from(id.to_string()),
        id.to_string(),
        kind,
    )
}

/// Pull the text out of a prompt request's content blocks.
fn prompt_text(request: &PromptRequest) -> String {
    request
        .prompt
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text(t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::main]
async fn main() -> agent_client_protocol::Result<()> {
    Agent
        .builder()
        .name("acp-echo-agent")
        .on_receive_request(
            async move |req: InitializeRequest,
                        responder: Responder<InitializeResponse>,
                        _cx: ConnectionTo<Client>| {
                responder.respond(
                    InitializeResponse::new(req.protocol_version)
                        .agent_capabilities(AgentCapabilities::new()),
                )
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            async move |_req: NewSessionRequest,
                        responder: Responder<NewSessionResponse>,
                        _cx: ConnectionTo<Client>| {
                responder.respond(NewSessionResponse::new(SessionId::new("echo-session")))
            },
            agent_client_protocol::on_receive_request!(),
        )
        .on_receive_request(
            move |req: PromptRequest,
                  responder: Responder<PromptResponse>,
                  cx: ConnectionTo<Client>| {
                let cx2 = cx.clone();
                // Spawned, not inline: an `on_receive_request` callback holds the
                // dispatch loop, so awaiting the permission reply inline would
                // deadlock waiting on the loop it is holding. (The SDK documents
                // this under "Deadlock Risk".)
                let turn = async move {
                    let asked = prompt_text(&req);

                    if asked.trim() == "die" {
                        // Vanish mid-turn on purpose. The client must surface this
                        // as a failure rather than an empty reply.
                        eprintln!("acp-echo-agent: exiting mid-turn by request");
                        std::process::exit(9);
                    }

                    let mut text = format!("echo: {asked}");

                    if let Some(kind) = asked.trim().strip_prefix("tool:") {
                        let fields = ToolCallUpdateFields::new()
                            .kind(tool_kind(kind.trim()))
                            .title(format!("fixture tool ({})", kind.trim()));
                        let verdict = cx2
                            .send_request(RequestPermissionRequest::new(
                                req.session_id.clone(),
                                ToolCallUpdate::new(ToolCallId::new("fixture-call"), fields),
                                vec![
                                    option("once", PermissionOptionKind::AllowOnce),
                                    option("rej-once", PermissionOptionKind::RejectOnce),
                                ],
                            ))
                            .block_task()
                            .await?;
                        text = format!("verdict: {:?}", verdict.outcome);
                    }

                    cx2.send_notification(SessionNotification::new(
                        req.session_id,
                        SessionUpdate::AgentMessageChunk(ContentChunk::new(ContentBlock::Text(
                            TextContent::new(text),
                        ))),
                    ))?;
                    responder.respond(PromptResponse::new(StopReason::EndTurn))
                };
                async move { cx.spawn(turn) }
            },
            agent_client_protocol::on_receive_request!(),
        )
        .connect_to(Stdio::new())
        .await
}
