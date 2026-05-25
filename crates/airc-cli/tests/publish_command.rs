//! End-to-end coverage for `airc publish`.
//!
//! Closes work card a0d740fa: the CLI surface must emit a typed
//! JSON receipt on stdout that downstream consumers (Continuum,
//! shell scripts) can jq-parse. No human prose.

use std::path::Path;
use std::process::{Command, Stdio};

use tempfile::TempDir;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn publish_emits_json_receipt_with_event_id_and_channel() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let stdout = run_ok(
        &home,
        &[
            "publish",
            "--body-json",
            "-",
            "--header",
            "airc.continuum.kind=chat_transcript",
        ],
    );

    // Single line of JSON, parseable.
    let trimmed = stdout.trim();
    let receipt: serde_json::Value = serde_json::from_str(trimmed)
        .unwrap_or_else(|error| panic!("stdout is not JSON: {error}; stdout={stdout}"));
    assert!(
        receipt["event_id"].is_string(),
        "missing event_id: {receipt}"
    );
    assert!(receipt["lamport"].is_number(), "missing lamport: {receipt}");
    assert!(
        receipt["occurred_at_ms"].is_number(),
        "missing occurred_at_ms: {receipt}"
    );
    assert!(
        receipt["channel_id"].is_string(),
        "missing channel_id: {receipt}"
    );
    assert!(
        receipt["channel_name"].is_string(),
        "missing channel_name: {receipt}"
    );
}

#[test]
fn publish_refuses_unsubscribed_room_with_clear_error() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let stderr = run_expect_failure(
        &home,
        &[
            "publish",
            "--room",
            "definitely-not-joined",
            "--body-text",
            "should fail",
        ],
    );

    assert!(
        stderr.contains("definitely-not-joined"),
        "stderr should name the channel, got: {stderr}"
    );
    assert!(
        stderr.contains("not subscribed") || stderr.contains("join the room first"),
        "stderr should explain remedy, got: {stderr}"
    );
}

#[test]
fn publish_requires_one_body_source() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    let stderr = run_expect_failure(&home, &["publish"]);
    assert!(
        stderr.contains("--body-text")
            || stderr.contains("--body-json")
            || stderr.contains("required"),
        "stderr should explain that a body source is required, got: {stderr}"
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
        .env("USERPROFILE", account_home);

    // The publish JSON test needs to feed a JSON body via stdin.
    // We do that by detecting `--body-json` with `-` and piping a
    // small object in. Other invocations get a closed stdin.
    let needs_stdin = args.windows(2).any(|w| w == ["--body-json", "-"]);
    if needs_stdin {
        command.stdin(Stdio::piped());
    }
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc-core command must spawn");

    if needs_stdin {
        use std::io::Write;
        let stdin = child.stdin.as_mut().expect("piped stdin");
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

fn run_expect_failure(home: &Path, args: &[&str]) -> String {
    let account_home = home.parent().unwrap_or(home);
    let output = Command::new(airc_core())
        .arg("--home")
        .arg(home)
        .args(args)
        .env("HOME", account_home)
        .env("USERPROFILE", account_home)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        !output.status.success(),
        "expected airc-core {:?} to fail, but it succeeded: stdout={}",
        args,
        String::from_utf8_lossy(&output.stdout),
    );
    String::from_utf8(output.stderr).expect("stderr utf-8")
}
