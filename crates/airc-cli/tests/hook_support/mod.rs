//! Shared process helpers for the hook command tests (codex, claude, delivery).
#![allow(dead_code)] // each test binary uses a different subset

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use serde_json::Value;

use super::common;

pub fn airc_core() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

pub fn run_ok(home: &Path, args: &[&str]) -> String {
    let output = command_for_home(home)
        .args(["--home"])
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    common::assert_success(&output, &format!("airc-core {:?}", args));
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

pub fn run_ok_with_client(home: &Path, client_id: &str, args: &[&str]) -> String {
    let output = command_for_home(home)
        .env("AIRC_CLIENT_ID", client_id)
        .args(["--home"])
        .arg(home)
        .args(args)
        .output()
        .expect("airc-core command must spawn");
    common::assert_success(&output, &format!("airc-core {:?}", args));
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

pub fn run_hook(home: &Path, args: &[&str], stdin: &str) -> String {
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
    common::assert_success(&output, &format!("airc-core {:?}", args));
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

pub fn run_hook_with_client(home: &Path, client_id: &str, args: &[&str], stdin: &str) -> String {
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
    common::assert_success(&output, &format!("airc-core {:?}", args));
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

pub fn command_for_home(home: &Path) -> Command {
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
    // Hermetic identity: a test run from inside an agent session must resolve the
    // runtime client exactly as CI does, not through the agent it was launched by.
    command.env("AIRC_NO_AGENT_ANCESTRY", "1");
    for name in [
        "AIRC_CLIENT_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
    ] {
        command.env_remove(name);
    }
    command
}

pub fn additional_context(output: &str) -> String {
    let value: Value = serde_json::from_str(output).expect("hook JSON");
    value["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .expect("additionalContext string")
        .to_string()
}

/// Run one hook call so this runtime's consumer exists: what arrives after it is
/// live, what came before is history (card ba22c72c).
pub fn attach(home: &Path, client: Option<&str>, verb: &str) {
    let args = [verb, "user-prompt-submit"];
    match client {
        Some(client) => run_hook_with_client(home, client, &args, "{}"),
        None => run_hook(home, &args, "{}"),
    };
}
