//! End-to-end coverage for `airc-core codex-hook ...`.

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
mod common;

fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

#[test]
fn codex_hook_emits_context_and_advances_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "first unread"]);
    run_ok(&home, &["send", "second unread"]);

    let output = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit", "--include-self"],
        "{}",
    );
    let context = additional_context(&output);
    assert!(context.contains("AIRC: 2 unread"));
    assert!(context.contains("first unread"));
    assert!(context.contains("second unread"));

    let second = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit", "--include-self"],
        "{}",
    );
    assert_eq!(
        second, "",
        "second hook call should be silent after cursor advance"
    );
}

#[test]
fn codex_hook_excludes_self_echoes_but_still_advances_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "own message"]);

    let output = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit"],
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(output, "");
    let second = run_hook(
        &home,
        &["codex-hook", "user-prompt-submit"],
        r#"{"hook_event_name":"UserPromptSubmit"}"#,
    );
    assert_eq!(second, "", "self echoes should not replay forever");
}

#[test]
fn codex_hook_filters_by_runtime_client_header_not_persisted_identity() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok_with_client(&home, "codex:thread-1", &["send", "own runtime"]);
    run_ok_with_client(&home, "claude:session-1", &["send", "peer runtime"]);

    let output = run_hook_with_client(
        &home,
        "codex:thread-1",
        &["codex-hook", "user-prompt-submit"],
        "{}",
    );
    let context = additional_context(&output);
    assert!(!context.contains("own runtime"));
    assert!(context.contains("peer runtime"));
}

#[test]
fn codex_hook_keeps_stamped_peer_events_when_runtime_client_is_unknown() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok_with_client(
        &home,
        "claude:session-1",
        &["send", "peer despite shared home"],
    );

    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    let context = additional_context(&output);
    assert!(context.contains("peer despite shared home"));
}

#[test]
fn codex_hook_suggests_claimable_work_on_work_events() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(
        &home,
        &[
            "work",
            "create",
            "--repo",
            "CambrianTech/airc",
            "--title",
            "wire claimable work into agent feed",
            "--priority",
            "p0",
        ],
    );

    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    let context = additional_context(&output);

    assert!(context.contains("AIRC work: 1 claimable P0/P1"));
    assert!(context.contains("wire claimable work into agent feed"));
    assert!(context.contains("airc work claim"));
}

#[test]
fn codex_hook_suggests_availability_on_availability_events() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(
        &home,
        &[
            "work",
            "availability",
            "--repo",
            "CambrianTech/airc",
            "--state",
            "ready",
            "--note",
            "available for queue work",
            "--ttl-ms",
            "60000",
        ],
    );

    let output = run_hook(&home, &["codex-hook", "user-prompt-submit"], "{}");
    let context = additional_context(&output);

    assert!(context.contains("AIRC work: 0 claimable P0/P1"));
    assert!(context.contains("availability ready=1 busy=0 away=0 stale=0"));
    assert!(context.contains("publish ready/busy/away"));
}

#[test]
fn codex_hook_raw_mode_preserves_full_event_lines() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "raw line visible"]);

    let output = run_hook(
        &home,
        &[
            "codex-hook",
            "user-prompt-submit",
            "--raw",
            "--include-self",
        ],
        "{}",
    );
    let context = additional_context(&output);
    assert!(context.contains("raw line visible"));
    assert!(context.contains('['));
    assert!(context.contains(']'));
}

#[test]
fn codex_hook_poll_prints_plain_digest_and_advances_cursor() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok(&home, &["send", "poll first unread"]);
    run_ok(&home, &["send", "poll second unread"]);

    let output = run_ok(
        &home,
        &["codex-hook", "poll", "--include-self", "--max-items", "4"],
    );
    assert!(output.contains("AIRC: 2 unread"));
    assert!(output.contains("poll first unread"));
    assert!(output.contains("poll second unread"));
    assert!(
        serde_json::from_str::<Value>(&output).is_err(),
        "poll is a plain CLI feed, not hook JSON"
    );

    let second = run_ok(&home, &["codex-hook", "poll", "--include-self"]);
    assert_eq!(second, "", "poll should share the hook cursor");
}

#[test]
fn codex_hook_poll_filters_runtime_self_echoes() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);
    run_ok_with_client(&home, "codex:thread-1", &["send", "poll own runtime"]);
    run_ok_with_client(&home, "claude:session-1", &["send", "poll peer runtime"]);

    let output = run_ok_with_client(
        &home,
        "codex:thread-1",
        &["codex-hook", "poll", "--max-items", "4"],
    );
    assert!(!output.contains("poll own runtime"));
    assert!(output.contains("poll peer runtime"));

    let second = run_ok_with_client(&home, "codex:thread-1", &["codex-hook", "poll"]);
    assert_eq!(
        second, "",
        "self-filtered events should still advance the cursor"
    );
}

#[test]
fn codex_hook_poll_waits_for_one_new_event() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("agent");

    run_ok(&home, &["init"]);

    let child = command_for_home(&home)
        .args(["--home"])
        .arg(&home)
        .args(["codex-hook", "poll", "--include-self", "--wait-ms", "2000"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc poll must spawn");

    thread::sleep(Duration::from_millis(150));
    run_ok(&home, &["send", "delayed poll event"]);

    let output = child.wait_with_output().expect("wait for poll");
    assert!(
        output.status.success(),
        "poll failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout utf-8");
    assert!(stdout.contains("delayed poll event"));
}

#[test]
fn codex_hook_installer_replaces_existing_managed_hook() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "[features]\ncodex_hooks = true\nother = true\n",
    )
    .expect("write config");
    std::fs::write(
        codex_home.join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo existing"},{"type":"command","command":"airc codex-hook user-prompt-submit"}]}]}}"#,
    )
    .expect("write hooks");

    run_ok(
        &home,
        &[
            "codex-hook",
            "install-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(config.contains("hooks = true"));
    assert!(config.contains("other = true"));
    assert!(!config.contains("codex_hooks"));

    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap())
            .expect("hooks json");
    let commands = hook_commands(&hooks);
    assert!(commands.contains(&"echo existing".to_string()));
    let managed = managed_hook_commands(&commands);
    assert_eq!(
        managed.len(),
        1,
        "expected one managed hook, got {commands:?}"
    );
    assert_source_command(&managed[0]);
}

#[test]
fn codex_hook_installer_replaces_existing_managed_hook_command() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(codex_home.join("config.toml"), "[features]\nhooks = true\n")
        .expect("write config");
    std::fs::write(
        codex_home.join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"airc codex-hook user-prompt-submit"}]}]}}"#,
    )
    .expect("write hooks");

    run_ok(
        &home,
        &[
            "codex-hook",
            "install-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap())
            .expect("hooks json");
    let commands = hook_commands(&hooks);
    assert_eq!(
        commands
            .iter()
            .filter(|command| command.ends_with("codex-hook user-prompt-submit"))
            .count(),
        1,
        "install must replace existing managed hook commands, got {commands:?}"
    );
    assert_eq!(commands.len(), 1, "expected one command, got {commands:?}");
    assert_source_command(&commands[0]);
}

#[test]
fn codex_hook_installer_adds_turn_contract_when_unset() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(codex_home.join("config.toml"), "[features]\n").expect("write config");

    run_ok(
        &home,
        &[
            "codex-hook",
            "install-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(config.contains("AIRC-CODEX-INSTRUCTIONS-START"));
    assert!(config.contains("developer_instructions"));
    assert!(config.contains("airc codex-hook poll --wait-ms 1000"));
    assert!(config.contains("hooks = true"));
}

#[test]
fn codex_hook_installer_preserves_custom_developer_instructions() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "developer_instructions = \"custom contract\"\n",
    )
    .expect("write config");

    run_ok(
        &home,
        &[
            "codex-hook",
            "install-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(config.contains("developer_instructions = \"custom contract\""));
    assert!(!config.contains("AIRC-CODEX-INSTRUCTIONS-START"));
    assert!(!config.contains("airc codex-hook poll --wait-ms 1000"));
    assert!(config.contains("hooks = true"));
}

#[test]
fn codex_hook_installer_removes_managed_filesystem_profile() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        r#"
[features]
codex_hooks = true

# airc filesystem permissions
[permissions.airc.filesystem]
enabled = true

[permissions.airc.filesystem.write]
paths = ["/tmp"]

[permissions.airc.network]
enabled = true
"#,
    )
    .expect("write config");

    run_ok(
        &home,
        &[
            "codex-hook",
            "install-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(!config.contains("permissions.airc.filesystem"));
    assert!(!config.contains("paths = [\"/tmp\"]"));
    assert!(config.contains("[permissions.airc.network]"));
    assert!(config.contains("hooks = true"));
}

#[test]
fn codex_hook_uninstaller_removes_managed_hooks_only() {
    let workspace = common::daemon_tempdir();
    let home = workspace.path().join("airc");
    let codex_home = workspace.path().join("codex");
    std::fs::create_dir_all(&codex_home).expect("codex home");
    std::fs::write(
        codex_home.join("config.toml"),
        "[features]\nhooks = true\nother = true\n",
    )
    .expect("write config");
    std::fs::write(
        codex_home.join("hooks.json"),
        r#"{"hooks":{"UserPromptSubmit":[{"hooks":[{"type":"command","command":"echo existing"},{"type":"command","command":"airc-core codex-hook user-prompt-submit"}]}]}}"#,
    )
    .expect("write hooks");

    run_ok(
        &home,
        &[
            "codex-hook",
            "uninstall-hooks",
            "--codex-home",
            codex_home.to_str().unwrap(),
        ],
    );

    let config = std::fs::read_to_string(codex_home.join("config.toml")).expect("read config");
    assert!(!config.contains("hooks = true"));
    assert!(config.contains("other = true"));

    let hooks: Value =
        serde_json::from_str(&std::fs::read_to_string(codex_home.join("hooks.json")).unwrap())
            .expect("hooks json");
    let commands = hook_commands(&hooks);
    assert!(commands.contains(&"echo existing".to_string()));
    assert!(!commands.contains(&"airc-core codex-hook user-prompt-submit".to_string()));
}

fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = command_for_home(home)
        .args(["--home"])
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn run_ok_with_client(home: &Path, client_id: &str, args: &[&str]) -> String {
    let output = command_for_home(home)
        .env("AIRC_CLIENT_ID", client_id)
        .args(["--home"])
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn run_hook(home: &Path, args: &[&str], stdin: &str) -> String {
    let mut child = command_for_home(home)
        .args(["--home"])
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc-core hook must spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write hook stdin");
    let output = child.wait_with_output().expect("wait for hook");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn run_hook_with_client(home: &Path, client_id: &str, args: &[&str], stdin: &str) -> String {
    let mut child = command_for_home(home)
        .env("AIRC_CLIENT_ID", client_id)
        .args(["--home"])
        .arg(home)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("airc-core hook must spawn");
    child
        .stdin
        .as_mut()
        .expect("stdin pipe")
        .write_all(stdin.as_bytes())
        .expect("write hook stdin");
    let output = child.wait_with_output().expect("wait for hook");
    assert!(
        output.status.success(),
        "airc-core {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn command_for_home(home: &Path) -> Command {
    let mut command = Command::new(airc_core());
    let account_home = home.parent().unwrap_or(home);
    command.env("HOME", account_home);
    command.env("USERPROFILE", account_home);
    // Hermetic: these tests run against a throwaway $HOME. Forbid the
    // daemon from reaching the host's real `gh` token for the optional
    // account rendezvous — on a gh-authed runner that token + the
    // throwaway home are mismatched, the rendezvous fails, and the
    // command exits non-zero. See `inject_gh_token` in commands.rs.
    command.env("AIRC_NO_GH_TOKEN_INJECT", "1");
    command
}

fn additional_context(output: &str) -> String {
    let value: Value = serde_json::from_str(output).expect("hook JSON");
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext string")
        .to_string()
}

fn hook_commands(hooks: &Value) -> Vec<String> {
    hooks["hooks"]["UserPromptSubmit"]
        .as_array()
        .expect("UserPromptSubmit array")
        .iter()
        .flat_map(|group| group["hooks"].as_array().into_iter().flatten())
        .filter_map(|hook| hook["command"].as_str().map(ToString::to_string))
        .collect()
}

fn managed_hook_commands(commands: &[String]) -> Vec<String> {
    commands
        .iter()
        .filter(|command| command.ends_with("codex-hook user-prompt-submit"))
        .cloned()
        .collect()
}

fn assert_source_command(command: &str) {
    // Post-demolition contract (PR D): the public command IS the
    // Rust binary on PATH at `airc`, not a source-tree wrapper
    // script. The managed hook command resolves to PATH `airc`,
    // exactly the command a stranger reading the README would type.
    assert_eq!(
        command, "airc codex-hook user-prompt-submit",
        "managed hook must be the PATH `airc` command, got {command:?}"
    );
    assert!(
        !command.starts_with("airc-core "),
        "managed hook must not call legacy airc-core suffix, got {command:?}"
    );
}

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
    assert!(
        elapsed < Duration::from_secs(15),
        "hook must complete inside the deadline budget (5s + slop). \
         If this fails the deadline regressed and Windows CI will \
         hang for hours again. Took {elapsed:?}; stderr={stderr_text}"
    );
    assert!(
        stderr_text.contains("stdin EOF not received") || stderr_text.contains("airc#1097"),
        "hook must emit the deadline-hit diagnostic on stderr so an \
         operator sees WHY the hook ran with empty payload: \
         stderr={stderr_text}"
    );
}
