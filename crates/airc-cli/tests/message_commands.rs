//! End-to-end coverage for `airc-core message ...`.

use std::process::Command;

use serde_json::Value;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc-core")
}

#[test]
fn message_build_legacy_alias_matches_build_output() {
    let build = run_ok(&["message", "build"]);
    let legacy = run_ok(&["message", "build-legacy"]);

    assert_eq!(legacy, build);

    let parsed: Value = serde_json::from_str(&legacy).expect("json payload");
    assert_eq!(parsed["from"], "codex");
    assert_eq!(parsed["to"], "all");
    assert_eq!(parsed["channel"], "airc");
    assert_eq!(parsed["msg"], "hello from test");
}

fn run_ok(prefix: &[&str]) -> String {
    let output = Command::new(airc_core())
        .args(prefix)
        .args([
            "--from",
            "codex",
            "--to",
            "all",
            "--ts",
            "2026-05-20T00:00:00Z",
            "--channel",
            "airc",
            "--msg",
            "hello from test",
            "--client-id",
            "client-1",
            "--kind",
            "chat",
        ])
        .output()
        .expect("run airc-core");

    assert!(
        output.status.success(),
        "command failed\nstatus: {:?}\nstdout: {}\nstderr: {}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    String::from_utf8(output.stdout).expect("utf8 stdout")
}
