//! End-to-end coverage of the runtime-neutral hook core, through `airc codex-hook`.

use std::io::Read;
use std::process::Stdio;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
mod common;
mod hook_support;
use hook_support::*;

/// Regression for airc#1097 + BIGMAMA review BLOCK#1 on PR #1197:
/// the original Windows hang was a 5+ hour deadlock because
/// `read_to_string` waited forever for an EOF that never arrived
/// (Stdio::piped() parent→daemon handle leak kept the pipe alive).
/// The 5s deadline in `drain_stdin` (`hook.rs:224`) is the fix.
///
/// Every other test in this file writes to stdin and drops the pipe
/// — EOF always arrives, so the new timeout branch
/// (`hook.rs:233-246`) is *never executed* by the rest of the suite.
/// That left the fix shipping unguarded: "a fix for a hang with no
/// hang-regression test cannot prove it fixes the hang or guard
/// against a future refactor reintroducing it."
///
/// This test pins the deadline behavior end-to-end:
///   - spawn `codex-hook user-prompt-submit` with `Stdio::piped()`
///     stdin and NEVER write or close it (the pipe-open-forever
///     shape that hung Windows),
///   - hold the stdin handle so it remains open during `child.wait()`
///     — `wait_with_output` would close stdin on us (per std docs:
///     "stdin handle will be closed before waiting"), defeating the
///     test by delivering EOF before the deadline fires,
///   - assert the child exits success() within the deadline budget
///     (5s deadline + slop) instead of hanging,
///   - assert the deadline-hit diagnostic is emitted on stderr so
///     operators can see why the hook ran with empty payload.
///
/// If a future refactor reverts to unbounded `read_to_string`, this
/// test hangs out to the test runner timeout — exactly the failure
/// shape the original bug had on Windows CI. The 15s ceiling here is
/// the canary: long enough to absorb spawn jitter on slow runners,
/// short enough that a true regression is caught in the first failed
/// run rather than after a 5-hour wait.
#[test]
fn drain_stdin_timeout_proceeds_when_eof_never_arrives() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    // The deadline belongs to stdin reading, not first daemon startup/schema
    // initialization. Exercise that setup through the normal hook before timing
    // the held-open pipe, using the same RAII-owned home and runtime consumer.
    run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");

    let mut child = command_for_home(&home)
        .args(["--home"])
        .arg(&home)
        .args(["codex-hook", "user-prompt-submit"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("hook must spawn");

    // Take ownership of stdin and HOLD it — we deliberately want the
    // pipe to remain open with no data through the entire wait so the
    // hook's `read_to_string` blocks indefinitely. Dropping it (or
    // letting wait_with_output close it) would deliver EOF and the
    // timeout branch we're testing would never run.
    let stdin = child.stdin.take().expect("stdin pipe");

    // Bound the wait ourselves: std::process::Child has no
    // wait_timeout (and pulling a crate in for one test is
    // disproportionate). Poll try_wait against a wall-clock budget
    // so a deadline regression manifests as a *failed* test rather
    // than a hung run that waits for the test runner's own timeout
    // to kill it. The 15s budget = the 5s deadline + spawn jitter
    // headroom; a true regression (read_to_string back to
    // unbounded) means try_wait never observes exit and the kill
    // path fires — the assertion message names the regression.
    let timeout = Duration::from_secs(15);
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if started.elapsed() > timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    panic!(
                        "hook did not exit within {timeout:?} — the \
                         airc#1097 5s stdin deadline regressed. \
                         read_to_string is back to blocking on an EOF \
                         that never arrives."
                    );
                }
                thread::sleep(Duration::from_millis(100));
            }
            Err(e) => panic!("try_wait error: {e}"),
        }
    };
    let elapsed = started.elapsed();
    drop(stdin);

    let mut stderr_buf = Vec::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = stderr.read_to_end(&mut stderr_buf);
    }
    let stderr_text = String::from_utf8_lossy(&stderr_buf);

    assert!(
        status.success(),
        "hook must exit success on stdin timeout (airc#1097 regression): \
         elapsed={elapsed:?} stderr={stderr_text}"
    );
    // The loop above owns the deadline and kills/reaps a still-running child.
    // Do not apply a second strict wall-clock assertion after try_wait observed
    // success: a 100ms polling/scheduling boundary can cross the same deadline
    // after the process has already exited (Windows CI: 15.048s, exit success).
    assert!(
        stderr_text.contains("stdin EOF not received") || stderr_text.contains("airc#1097"),
        "hook must emit the deadline-hit diagnostic on stderr so an \
         operator sees WHY the hook ran with empty payload: \
         stderr={stderr_text}"
    );
}

#[test]
fn post_tool_hook_frames_context_and_shares_prompt_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    attach(&home, None, "codex-hook");
    run_ok(&home, &["send", "arrived during work"]);
    let output = run_hook(
        &home,
        &["codex-hook", "post-tool-use", "--include-self"],
        "{}",
    );
    let payload: Value = serde_json::from_str(&output).unwrap();
    assert_eq!(
        payload["hookSpecificOutput"]["hookEventName"],
        "PostToolUse"
    );
    assert!(additional_context(&output).contains("arrived during work"));
    assert_eq!(
        run_hook(
            &home,
            &["codex-hook", "user-prompt-submit", "--include-self"],
            "{}"
        ),
        ""
    );
}

#[test]
fn post_tool_hook_session_input_isolates_shared_home_cursors() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    let args = &["codex-hook", "post-tool-use"];
    // Empty identity overrides make the test independent of the test runner's
    // inherited Codex task identity (Windows has no process-walk fallback).
    for session in [r#"{"session_id":"first"}"#, r#"{"session_id":"second"}"#] {
        run_hook_with_client(&home, "", args, session);
    }
    run_ok_with_client(&home, "claude:peer", &["send", "message for both tasks"]);
    let first = run_hook_with_client(&home, "", args, r#"{"session_id":"first"}"#);
    let second = run_hook_with_client(&home, "", args, r#"{"session_id":"second"}"#);
    assert!(additional_context(&first).contains("message for both tasks"));
    assert!(additional_context(&second).contains("message for both tasks"));
    assert_eq!(
        run_hook_with_client(&home, "", args, r#"{"session_id":"first"}"#),
        ""
    );
}

#[test]
fn hook_reads_current_message_after_typed_legacy_chunk_backlog() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    // Establish this consumer's checkpoint BEFORE the backlog.
    run_hook_with_client(
        &home,
        "codex:backlog-reader",
        &["codex-hook", "user-prompt-submit", "--raw"],
        "{}",
    );
    for _ in 0..55 {
        run_ok_with_client(
            &home,
            "claude:backlog-peer",
            &[
                "publish",
                "--body-text",
                "legacy-token-fragment",
                "--header",
                "airc.stream.id=legacy",
                "--header",
                "airc.client=claude:backlog-peer",
                "--kind",
                "event",
            ],
        );
    }
    run_ok_with_client(
        &home,
        "claude:backlog-peer",
        &["send", "current-peer-request"],
    );
    run_ok_with_client(
        &home,
        "claude:backlog-peer",
        &[
            "publish",
            "--body-text",
            "ordinary-system-warning",
            "--header",
            "airc.client=claude:backlog-peer",
            "--kind",
            "event",
        ],
    );
    let output = run_hook_with_client(
        &home,
        "codex:backlog-reader",
        &["codex-hook", "user-prompt-submit", "--raw", "--count", "50"],
        "{}",
    );
    let text = additional_context(&output);
    assert!(text.contains("current-peer-request"));
    assert!(text.contains("ordinary-system-warning"));
    assert!(!text.contains("legacy-token-fragment"));
}

// what this catches: card ba22c72c. A scope with a long unread backlog yields ONE
// summary line (count, original times) on the first call, never the old messages;
// every later call carries only what arrived after it, and the summary never repeats.
#[test]
fn backlog_is_summarized_once_then_only_new_messages_deliver() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");
    run_ok(&home, &["init"]);
    for n in 0..12 {
        run_ok(&home, &["send", &format!("retained-old-{n}")]);
    }
    let args = &[
        "codex-hook",
        "user-prompt-submit",
        "--include-self",
        "--count",
        "4",
        "--max-items",
        "2",
    ];
    let first = additional_context(&run_hook(&home, args, "{}"));
    assert!(
        first.contains("earlier events from before this session attached"),
        "{first}"
    );
    assert!(!first.contains("retained-old-"), "{first}");
    assert!(
        run_hook(&home, args, "{}").trim().is_empty(),
        "the summary is shown once"
    );
    run_ok(&home, &["send", "new-directed-request"]);
    let next = additional_context(&run_hook(&home, args, "{}"));
    assert!(next.contains("new-directed-request"));
    assert!(!next.contains("earlier events") && !next.contains("retained-old-"));
    for _ in 0..3 {
        assert!(run_hook(&home, args, "{}").trim().is_empty());
    }
}
