//! Owner-core CLI dogfood: same-machine delivery is the daemon's
//! in-memory router, NOT a `frames.jsonl` file wire.
//!
//! Replaces the legacy two-homes-shared-wire `operational_dogfood`. The
//! owner-core model is ONE machine account → ONE daemon: two scopes
//! under the same `$HOME` converge through that daemon, and **no
//! `frames.jsonl` is ever written**. This test fails the moment any
//! same-machine path falls back to the file wire.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;

fn airc() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// Run `airc --home <account>/<scope> <args>` with `HOME=<account>` so
/// every scope resolves the same machine-account daemon.
fn run(account: &Path, scope: &str, client: &str, args: &[&str]) -> String {
    let home = account.join(scope);
    let output = Command::new(airc())
        .arg("--home")
        .arg(&home)
        .args(args)
        .env("HOME", account)
        .env("USERPROFILE", account)
        .env("AIRC_CLIENT_ID", client)
        .output()
        .expect("airc command must spawn");
    assert!(
        output.status.success(),
        "airc {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

fn any_frames_jsonl(dir: &Path) -> Option<PathBuf> {
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&path) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().and_then(|n| n.to_str()) == Some("frames.jsonl") {
                return Some(p);
            }
        }
    }
    None
}

#[test]
fn same_machine_round_trips_via_daemon_with_no_frames_jsonl() {
    let account = common::daemon_tempdir();

    // One scope sends; another scope under the SAME $HOME reads it back —
    // they share the one machine daemon, so the message converges.
    run(account.path(), "agent", "claude:dogfood", &["init"]);
    run(
        account.path(),
        "agent",
        "claude:dogfood",
        &["room", "general"],
    );
    run(
        account.path(),
        "agent",
        "claude:dogfood",
        &["send", "owner-core hello via the daemon router"],
    );

    let inbox = run(
        account.path(),
        "agent",
        "codex:dogfood",
        &["inbox", "--limit", "16"],
    );
    assert!(
        inbox.contains("owner-core hello via the daemon router"),
        "inbox must replay the send through the daemon: {inbox}"
    );

    // The cancer is gone: same-machine delivery never touched a file wire.
    if let Some(path) = any_frames_jsonl(account.path()) {
        panic!("a frames.jsonl was written ({}); same-machine delivery must be the daemon router, not a file wire", path.display());
    }

    // Reap the machine daemon so the test doesn't leak it.
    let _ = Command::new(airc())
        .arg("--home")
        .arg(account.path().join("agent"))
        .arg("stop")
        .env("HOME", account.path())
        .env("USERPROFILE", account.path())
        .output();
}
