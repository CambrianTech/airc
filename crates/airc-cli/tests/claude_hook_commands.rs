//! End-to-end coverage for `airc claude-hook ...` (card b00d2724).

use serde_json::Value;
mod common;
mod hook_support;
use hook_support::*;

/// Claude Code's hook stdin: session_id, cwd and hook_event_name, plus fields airc ignores.
fn claude_stdin(session: &str, event: &str) -> String {
    serde_json::json!({
        "session_id": session,
        "transcript_path": "/tmp/transcript.jsonl",
        "cwd": "/tmp",
        "hook_event_name": event,
        "prompt": "carry on",
    })
    .to_string()
}

// what this catches: the Claude adapter emits the exact JSON Claude Code reads for
// both events, keyed by the stdin session, and says in one line that no `airc join`
// reader is beating for that session (the 9 h deaf session of 2026-10-26).
#[test]
fn claude_hook_emits_claude_valid_json_for_both_events() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    // An empty AIRC_CLIENT_ID leaves the stdin session as this hook's identity.
    let prompt = claude_stdin("s-1", "UserPromptSubmit");
    run_hook_with_client(&home, "", &["claude-hook", "user-prompt-submit"], &prompt);
    run_ok_with_client(&home, "codex:peer", &["send", "review requested"]);
    let output = run_hook_with_client(&home, "", &["claude-hook", "user-prompt-submit"], &prompt);
    let payload: Value = serde_json::from_str(&output).expect("one JSON object");
    let specific = payload["hookSpecificOutput"]
        .as_object()
        .expect("hookSpecificOutput");
    assert_eq!(payload.as_object().unwrap().len(), 1, "{payload}");
    assert_eq!(specific.len(), 2, "{payload}");
    assert_eq!(specific["hookEventName"], "UserPromptSubmit");
    let context = specific["additionalContext"].as_str().unwrap();
    assert!(context.contains("review requested"));
    assert!(
        context.contains("no live `airc join` reader for claude:s-1"),
        "{context}"
    );
    run_ok_with_client(&home, "codex:peer", &["send", "arrived during a tool"]);
    let tool = claude_stdin("s-1", "PostToolUse");
    let output = run_hook_with_client(&home, "", &["claude-hook", "post-tool-use"], &tool);
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        payload["hookSpecificOutput"]["hookEventName"],
        "PostToolUse"
    );
    let context = additional_context(&output);
    assert!(context.contains("arrived during a tool"));
    assert!(!context.contains("review requested"), "the cursor advanced");
    assert_eq!(
        run_hook_with_client(&home, "", &["claude-hook", "post-tool-use"], &tool),
        ""
    );
}

// what this catches: install/uninstall edit the settings file it is given, keep
// every unrelated key (and a user's own hook in the same event), are idempotent,
// and round-trip to the original content.
#[test]
fn claude_install_uninstall_round_trips_settings_with_unrelated_keys() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    let settings = workspace.path().join("claude").join("settings.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    let original = serde_json::json!({
        "model": "opus",
        "permissions": {"allow": ["Bash(git status)"], "deny": []},
        "hooks": {"PostToolUse": [{"matcher": "Edit", "hooks": [{"type": "command", "command": "user-formatter"}]}]},
        "statusLine": {"type": "command", "command": "status.sh"}
    });
    std::fs::write(&settings, serde_json::to_string_pretty(&original).unwrap()).unwrap();
    let path = settings.to_str().unwrap();
    let installed = run_ok(&home, &["claude-hook", "install-hooks", "--settings", path]);
    assert!(installed.contains("installed AIRC"));
    assert_eq!(
        run_ok(&home, &["claude-hook", "install-hooks", "--settings", path]),
        "",
        "idempotent"
    );
    let value: Value = serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(value["model"], "opus");
    assert_eq!(value["statusLine"], original["statusLine"]);
    assert_eq!(
        value["hooks"]["PostToolUse"][0],
        original["hooks"]["PostToolUse"][0]
    );
    assert_eq!(value["hooks"]["PostToolUse"][1]["matcher"], "*");
    assert_eq!(
        value["hooks"]["PostToolUse"][1]["hooks"][0]["command"],
        "airc claude-hook post-tool-use"
    );
    assert_eq!(
        value["hooks"]["UserPromptSubmit"][0]["hooks"][0]["command"],
        "airc claude-hook user-prompt-submit"
    );
    run_ok(
        &home,
        &["claude-hook", "uninstall-hooks", "--settings", path],
    );
    assert_eq!(
        run_ok(
            &home,
            &["claude-hook", "uninstall-hooks", "--settings", path]
        ),
        ""
    );
    let removed: Value =
        serde_json::from_str(&std::fs::read_to_string(&settings).unwrap()).unwrap();
    assert_eq!(removed, original);
}
