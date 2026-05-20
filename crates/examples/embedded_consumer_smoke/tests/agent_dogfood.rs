//! Dogfood proof for Codex/Claude-style agent runtimes.
//!
//! The consumer here behaves like an agent hook/monitor loop: subscribe
//! first, receive inbound typed events live, persist a cursor, and
//! resume after restart without processing its own sends as inbound
//! work. It depends only on `embedded_consumer_smoke`, whose runtime
//! AIRC dependency is only `airc-lib`.

use std::time::Duration;

use airc_lib::Body;
use embedded_consumer_smoke::agent::{
    AgentConsumer, AgentProfile, HEADER_AGENT_KIND, HEADER_AGENT_NAME,
};
use tempfile::TempDir;

#[tokio::test]
async fn codex_and_claude_style_agents_exchange_without_self_echo_or_polling() {
    let codex_home = TempDir::new().unwrap();
    let claude_home = TempDir::new().unwrap();
    let wire_dir = TempDir::new().unwrap();
    let wire = wire_dir.path().join("agent-wire.jsonl");

    let codex = AgentConsumer::open(
        codex_home.path(),
        AgentProfile::new("codex", "codex-run-001"),
    )
    .await
    .unwrap();
    let claude = AgentConsumer::open(
        claude_home.path(),
        AgentProfile::new("claude", "claude-run-001"),
    )
    .await
    .unwrap();

    codex.trust_peer_spec(&claude.peer_spec()).await.unwrap();
    claude.trust_peer_spec(&codex.peer_spec()).await.unwrap();

    codex
        .join_shared_wire("agent-dogfood", &wire)
        .await
        .unwrap();
    claude
        .join_shared_wire("agent-dogfood", &wire)
        .await
        .unwrap();

    let mut codex_inbox = codex.subscribe_prompts().await.unwrap();
    let mut claude_inbox = claude.subscribe_prompts().await.unwrap();

    codex
        .send_prompt("codex -> claude: can you see this through airc-lib?")
        .await
        .unwrap();

    let claude_seen = claude_inbox
        .next_inbound(Duration::from_secs(3))
        .await
        .unwrap()
        .expect("claude must receive codex prompt live");
    assert_eq!(
        claude_seen.body.as_ref().and_then(Body::as_text),
        Some("codex -> claude: can you see this through airc-lib?")
    );
    assert_eq!(
        claude_seen
            .headers
            .get(HEADER_AGENT_KIND)
            .map(String::as_str),
        Some("prompt")
    );
    assert_eq!(
        claude_seen
            .headers
            .get(HEADER_AGENT_NAME)
            .map(String::as_str),
        Some("codex")
    );

    let codex_self_echo = codex_inbox
        .next_inbound(Duration::from_millis(150))
        .await
        .unwrap();
    assert!(
        codex_self_echo.is_none(),
        "an agent monitor must not process its own send as inbound work"
    );

    let cursor_after_first = claude_seen.cursor();

    claude
        .send_prompt("claude -> codex: yes, live subscribe works")
        .await
        .unwrap();

    let codex_seen = codex_inbox
        .next_inbound(Duration::from_secs(3))
        .await
        .unwrap()
        .expect("codex must receive claude prompt live");
    assert_eq!(
        codex_seen.body.as_ref().and_then(Body::as_text),
        Some("claude -> codex: yes, live subscribe works")
    );

    let resumed = claude
        .resume_prompts_after(&cursor_after_first, 16)
        .await
        .unwrap();
    assert!(
        resumed
            .iter()
            .all(|event| event.event_id != cursor_after_first.event_id),
        "cursor resume must not return the event at the cursor"
    );
    assert!(
        resumed.is_empty(),
        "claude's own prompt after the cursor is filtered from inbound replay"
    );
}
