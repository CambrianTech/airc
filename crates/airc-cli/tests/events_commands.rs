//! End-to-end coverage for `airc-core events ...`.

use std::path::Path;
use std::process::{Command, Stdio};

mod common;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn events_list_filters_by_kind_and_header_prefix() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "plain chat"]);
    run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "subscription filter proof",
        ],
    );

    let output = run_ok(
        &home,
        &[
            "events",
            "list",
            "--kind",
            "system",
            "--header-prefix",
            "forge.body_hint=forge.work.",
        ],
    );

    assert!(output.contains("events: 1"));
    assert!(output.contains("System"));
    assert!(output.contains("subscription filter proof"));
    assert!(!output.contains("plain chat"));
}

#[test]
fn events_list_json_emits_machine_readable_contract() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "plain chat"]);
    run_ok(
        &home,
        &[
            "publish",
            "--kind",
            "message",
            "--body-json",
            "-",
            "--header",
            "forge.body_hint=continuum.chat_transcript",
            "--header",
            "continuum.trace_id=smoke-trace",
        ],
    );

    let output = run_ok(
        &home,
        &[
            "events",
            "list",
            "--json",
            "--kind",
            "message",
            "--header",
            "forge.body_hint=continuum.chat_transcript",
        ],
    );
    let value: serde_json::Value = serde_json::from_str(&output)
        .unwrap_or_else(|error| panic!("events list --json stdout is not JSON: {error}; {output}"));

    assert_eq!(value["count"], 1);
    let event = &value["events"][0];
    assert!(event["event_id"].is_string(), "missing event_id: {event}");
    assert_eq!(event["kind"], "message");
    assert_eq!(
        event["headers"]["forge.body_hint"],
        "continuum.chat_transcript"
    );
    assert_eq!(event["headers"]["continuum.trace_id"], "smoke-trace");
    assert_eq!(event["body"]["kind"], "json");
    assert_eq!(event["body"]["value"]["text"], "hello structured");
    assert!(
        !output.contains("events: 1"),
        "json mode must not include human prose"
    );
}

#[test]
fn send_receipt_distinguishes_zero_enrolled_peers_without_lying_about_delivery() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let output = run_ok(&home, &["send", "wire-only send"]);

    // With zero enrolled remote peers, the message IS still delivered
    // to any same-machine scope tailing the channel (post-#857
    // cross-process broadcast fix). The receipt must be HONEST: it must
    // not imply confirmed remote delivery, and it must not claim
    // non-delivery either (local tailers still receive it). This mirrors
    // the canonical `format_send_receipt` invariants unit-tested in
    // `commands.rs` — the verb is "queued to" (not the old delivery-
    // implying "sent to"), the count is framed as "enrolled" (not
    // "paired"), and the local-tailer fan-out is surfaced.
    assert!(
        output.contains("queued to"),
        "receipt must use the honest verb 'queued to' — not the old delivery-implying 'sent to', got: {output}"
    );
    assert!(
        output.contains("0 enrolled remote peer(s)"),
        "receipt must surface zero enrolled remote peers without claiming non-delivery, got: {output}"
    );
    assert!(
        !output.contains("sent to"),
        "receipt must NOT imply confirmed delivery via 'sent to', got: {output}"
    );
    assert!(
        output.contains("tailing this channel on this machine"),
        "receipt must surface that same-machine tailers still receive it, got: {output}"
    );
    assert!(
        !output.contains("not delivered"),
        "receipt must NOT claim non-delivery — same-machine tailers will receive it, got: {output}"
    );
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let account_home = home.parent().unwrap_or(home);
    let mut command = Command::new(airc_core());
    command
        .arg("--home")
        .arg(home)
        .args(args)
        .env("HOME", account_home)
        .env("USERPROFILE", account_home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    let needs_stdin = args.windows(2).any(|w| w == ["--body-json", "-"]);
    if needs_stdin {
        command.stdin(Stdio::piped());
    }

    let mut child = command.spawn().expect("airc-core command must spawn");
    if needs_stdin {
        use std::io::Write;
        let mut stdin = child.stdin.take().expect("piped stdin");
        stdin
            .write_all(br#"{"kind":"chat","text":"hello structured"}"#)
            .expect("write stdin");
    }

    let output = child.wait_with_output().expect("airc-core command output");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}
