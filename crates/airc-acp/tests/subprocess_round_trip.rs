//! The subprocess path: spawn a real ACP agent and talk to it over stdio.
//!
//! The in-process tests in `transport.rs` cover protocol logic but never fork.
//! Everything below crosses a real process boundary, because that is a distinct
//! risk surface — process spawn, argument parsing, and JSON-RPC framing over
//! stdin/stdout — and it is the one that fails quietly on Windows. A child that
//! never starts and an agent with nothing to say are indistinguishable from the
//! room unless something checks.
//!
//! Gated on `test-fixtures` because that is what builds the agent binary these
//! tests spawn. Run with:
//!
//! ```sh
//! cargo test -p airc-acp --features test-fixtures
//! ```
#![cfg(feature = "test-fixtures")]

use std::time::Duration;

use airc_acp::{AcpBridge, AgentSpec, BridgeError, ToolPolicy};

/// Path to the fixture agent binary, resolved by cargo.
fn agent_command() -> String {
    // Quoted: on Windows this path routinely contains spaces (`C:\Users\First
    // Last\...`), and an unquoted command would be split into a nonexistent
    // program plus stray arguments.
    format!("\"{}\"", env!("CARGO_BIN_EXE_acp-echo-agent"))
}

/// Every subprocess test runs under a deadline. A deadlocked or never-spawning
/// child does not fail on its own — it hangs, and a hung CI job reads as a slow
/// one until someone kills it.
async fn within<F, T>(what: &str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    match tokio::time::timeout(Duration::from_secs(30), fut).await {
        Ok(v) => v,
        Err(_) => panic!("{what} did not complete in 30s — the subprocess exchange hung"),
    }
}

/// what this catches: the whole subprocess path. If spawn, argv handling, or
/// stdio framing is wrong on this platform, this is the test that says so —
/// rather than a room that mysteriously gets no answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spawned_agent_answers_over_stdio() {
    let bridge = AcpBridge::new(AgentSpec::talk_only(agent_command()));
    let turn = within("spawned echo", bridge.prompt("peer-a", "hello"))
        .await
        .expect("the fixture agent should answer");

    assert_eq!(turn.text, "echo: hello");
    assert!(!turn.had_refusal());
}

/// what this catches: a deny that only exists on our side of the process
/// boundary. The refusal has to actually reach the child AND come back as a
/// room-visible line.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_refusal_crosses_the_process_boundary_and_is_visible() {
    let bridge = AcpBridge::new(AgentSpec::talk_only(agent_command()));
    let turn = within("spawned refusal", bridge.prompt("peer-a", "tool:execute"))
        .await
        .expect("the fixture agent should answer");

    assert!(
        turn.had_refusal(),
        "unconfigured lane must refuse: {turn:?}"
    );
    let lines = turn.refusal_lines();
    assert_eq!(lines.len(), 1, "{lines:?}");
    assert!(lines[0].contains("toolsAllow"), "{lines:?}");
    // The child must have been told "no", not merely had a refusal logged here.
    assert!(turn.text.contains("rej-once"), "child saw: {}", turn.text);
}

/// what this catches: an allow-list that works in-process but not across a
/// spawn — e.g. if identity extraction were reading a field that survives one
/// path and not the other.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_allow_listed_kind_is_approved_across_the_boundary() {
    let spec = AgentSpec::talk_only(agent_command()).with_tools(ToolPolicy::allow(["execute"]));
    let turn = within(
        "spawned approval",
        AcpBridge::new(spec).prompt("peer-a", "tool:execute"),
    )
    .await
    .expect("the fixture agent should answer");

    assert!(!turn.had_refusal(), "{turn:?}");
    assert!(turn.refusal_lines().is_empty(), "approvals stay quiet");
    assert!(turn.text.contains("once"), "child saw: {}", turn.text);
}

/// what this catches: the failure mode this whole project keeps relearning —
/// something stops working and says nothing. An agent that dies mid-turn must
/// produce an ERROR, never an empty-but-successful turn, because the room
/// cannot tell empty-success apart from a corpse.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_agent_that_dies_mid_turn_is_an_error_not_silence() {
    let bridge = AcpBridge::new(AgentSpec::talk_only(agent_command()));
    let result = within("dying agent", bridge.prompt("peer-a", "die")).await;

    let err = match result {
        Err(e) => e,
        Ok(turn) => panic!("a dead agent must not look like a successful turn: {turn:?}"),
    };
    // It was reached and then failed — that is an Exchange failure, not a
    // "could not start". The distinction is what tells an operator whether to
    // install something or debug something.
    assert!(
        matches!(err, BridgeError::Exchange { .. }),
        "expected an exchange failure, got {err:?}"
    );
    assert!(!err.to_string().is_empty(), "the error must say something");
}

/// what this catches: a missing agent being reported as a protocol problem.
/// "Install the agent" and "debug the protocol" are different days of work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_missing_agent_binary_is_reported_as_unavailable() {
    let bridge = AcpBridge::new(AgentSpec::talk_only(
        "definitely-not-an-installed-program-9c1f",
    ));
    let err = within("missing agent", bridge.prompt("peer-a", "hello"))
        .await
        .expect_err("a missing binary cannot produce a turn");

    // regression: `AcpAgent::from_str` only PARSES the command, so a missing
    // program used to sail through and surface as
    // "ACP exchange failed: Internal error: … program not found" — sending the
    // reader to debug the protocol when the fix is `pip install`. The classifier
    // is now structural: nothing spoke ACP, therefore the agent is unavailable.
    assert!(
        matches!(err, BridgeError::AgentUnavailable { .. }),
        "a missing binary is an install problem, not a protocol problem: {err:?}"
    );
    let msg = err.to_string();
    assert!(msg.contains("could not start"), "unhelpful error: {msg}");
    // It must name the command, or the operator cannot tell WHAT to install.
    assert!(
        msg.contains("definitely-not-an-installed-program-9c1f"),
        "error must name the command: {msg}"
    );
}
