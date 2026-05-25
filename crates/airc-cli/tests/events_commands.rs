//! End-to-end coverage for `airc-core events ...`.

use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn events_list_filters_by_kind_and_header_prefix() {
    let workspace = TempDir::new().expect("tempdir");
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
    let workspace = TempDir::new().expect("tempdir");
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
fn send_receipt_distinguishes_zero_paired_peers_without_lying_about_delivery() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let output = run_ok(&home, &["send", "wire-only send"]);

    // With zero paired remote peers, the message IS still delivered
    // to any same-machine scope tailing the channel (post-#857
    // cross-process broadcast fix). The receipt must not lie about
    // non-delivery.
    assert!(
        output.contains("sent to"),
        "receipt must lead with 'sent to' — not the old 'stored locally' wording, got: {output}"
    );
    assert!(
        output.contains("0 paired remote peers"),
        "receipt must surface zero-paired-remote-peers without claiming non-delivery, got: {output}"
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
