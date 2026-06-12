//! Comprehensive owner-core daemon lifecycle + multi-tab integration.
//!
//! The owner-core promise, exercised through the real `airc` binary the
//! way a machine actually runs it (the `dockerd` model): ONE daemon per
//! machine account, spawned once and shared by every tab. This walks a
//! full real-world session in one story — launch, many contending tabs,
//! rooms made/joined/left, agents talking and converging — then a
//! second test proves the daemon survives shutdown + restart with the
//! durable transcript intact.
//!
//! No `frames.jsonl` is ever written: same-machine delivery is the
//! daemon's in-memory router over its one SQLite ORM.

use std::path::Path;
use std::process::Command;
use std::thread;

use tempfile::TempDir;

fn airc() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// One "tab": an `airc` invocation for `scope` under the shared machine
/// account `account` (`HOME=account`), so every tab resolves the same
/// machine-account daemon. `client` is the stable agent id (the
/// participant identity the daemon attributes events to).
fn tab(account: &Path, scope: &str, client: &str, args: &[&str]) -> std::process::Output {
    Command::new(airc())
        .arg("--home")
        .arg(account.join(scope))
        .args(args)
        .env("HOME", account)
        .env("USERPROFILE", account)
        .env("AIRC_CLIENT_ID", client)
        // Hermetic gate (card d793c242): tabs spawn-or-connect the
        // daemon, which inherits this env — the spawned daemon must
        // never touch the operator's real gh account rendezvous.
        .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
        .output()
        .expect("airc must spawn")
}

/// Run a tab and require success, returning stdout.
fn ok(account: &Path, scope: &str, client: &str, args: &[&str]) -> String {
    let out = tab(account, scope, client, args);
    assert!(
        out.status.success(),
        "airc {args:?} (scope={scope}) failed: stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

/// The live daemon's identity, as seen by `airc status`. `peer_id` is
/// the machine account (stable for the daemon's life); `uptime` resets
/// to ~0 only when a NEW daemon is spawned — so it's our "did it
/// respawn?" probe.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonId {
    peer_id: String,
    uptime: u64,
}

fn daemon_id(account: &Path, scope: &str) -> DaemonId {
    let status = ok(account, scope, "claude:probe", &["status"]);
    let field = |key: &str| {
        status
            .lines()
            .find_map(|l| l.trim().strip_prefix(key))
            .map(|v| v.trim().to_string())
            .unwrap_or_else(|| panic!("status missing {key}: {status}"))
    };
    DaemonId {
        peer_id: field("peer_id:"),
        uptime: field("uptime_seconds:")
            .parse()
            .expect("uptime is a number"),
    }
}

/// Walk `dir` and fail if any `frames.jsonl` exists — the legacy file
/// wire must never reappear.
fn assert_no_frames_jsonl(dir: &Path) {
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
                panic!("a frames.jsonl was written ({}); same-machine delivery must be the daemon router", p.display());
            }
        }
    }
}

fn stop_daemon(account: &Path) {
    let _ = tab(account, "claude", "claude:stop", &["stop"]);
}

#[test]
fn one_daemon_serves_many_tabs_through_a_full_room_lifecycle() {
    let account = TempDir::new().expect("account tempdir");
    let acct = account.path();

    // --- Launch: the first tab brings up exactly one daemon. ---
    ok(acct, "claude", "claude:main", &["init"]);
    ok(acct, "claude", "claude:main", &["room", "general"]);
    ok(
        acct,
        "claude",
        "claude:main",
        &["send", "hello from claude"],
    );
    let launched = daemon_id(acct, "claude");

    // --- Convergence: a different tab on the same machine shares the
    // one daemon and sees the first tab's message. ---
    let codex_inbox = ok(acct, "codex", "codex:main", &["inbox", "--limit", "16"]);
    assert!(
        codex_inbox.contains("hello from claude"),
        "second tab must see the first tab's message via the shared daemon: {codex_inbox}"
    );

    // --- Many tabs contending: a burst of concurrent invocations must
    // all attach to the SAME daemon, never spawn a second. ---
    thread::scope(|s| {
        for i in 0..8 {
            let acct = acct.to_path_buf();
            s.spawn(move || {
                let scope = format!("tab{i}");
                let client = format!("claude:tab{i}");
                ok(&acct, &scope, &client, &["room", "general"]);
                ok(
                    &acct,
                    &scope,
                    &client,
                    &["send", &format!("ping from {scope}")],
                );
            });
        }
    });
    let after_contention = daemon_id(acct, "claude");
    assert_eq!(
        after_contention.peer_id, launched.peer_id,
        "every contending tab must share ONE daemon — peer identity must not change"
    );
    assert!(
        after_contention.uptime >= launched.uptime,
        "the daemon must not have respawned under contention (uptime reset): {launched:?} -> {after_contention:?}"
    );

    // All 8 contending tabs' messages landed on the one daemon.
    let general = ok(acct, "claude", "claude:main", &["inbox", "--limit", "64"]);
    for i in 0..8 {
        assert!(
            general.contains(&format!("ping from tab{i}")),
            "message from tab{i} must have reached the shared daemon: {general}"
        );
    }

    // --- Rooms: make + join a second room; traffic stays isolated. ---
    ok(acct, "codex", "codex:main", &["room", "review"]);
    ok(acct, "codex", "codex:main", &["send", "review thread open"]);
    ok(acct, "claude", "claude:main", &["room", "review"]);
    let review = ok(acct, "claude", "claude:main", &["inbox", "--limit", "16"]);
    assert!(
        review.contains("review thread open"),
        "claude joined 'review' and must see codex's message there: {review}"
    );
    assert!(
        !review.contains("hello from claude"),
        "the 'general' chat must not leak into 'review': {review}"
    );

    // --- Leave a room without tearing down identity or the daemon. ---
    ok(acct, "claude", "claude:main", &["part", "general"]);
    let still_one = daemon_id(acct, "claude");
    assert_eq!(
        still_one.peer_id, launched.peer_id,
        "leaving a room must not change the daemon"
    );

    // The cancer stays gone: no file wire anywhere under the account.
    assert_no_frames_jsonl(acct);

    stop_daemon(acct);
}

#[test]
fn daemon_survives_shutdown_and_restart_with_durable_history_intact() {
    let account = TempDir::new().expect("account tempdir");
    let acct = account.path();

    // Durable chat into a room.
    ok(acct, "claude", "claude:main", &["init"]);
    ok(acct, "claude", "claude:main", &["room", "standup"]);
    ok(acct, "claude", "claude:main", &["send", "durable line one"]);
    ok(acct, "claude", "claude:main", &["send", "durable line two"]);
    let before = daemon_id(acct, "claude");

    // Shut the daemon down completely.
    stop_daemon(acct);

    // The next command respawns the daemon (spawn-or-connect) AND the
    // durable transcript replays from the persistent ORM — proof the
    // owner-core survives a full restart, not just a reconnect.
    let replayed = ok(acct, "claude", "claude:main", &["inbox", "--limit", "16"]);
    assert!(
        replayed.contains("durable line one") && replayed.contains("durable line two"),
        "durable history must survive a daemon restart: {replayed}"
    );

    let after = daemon_id(acct, "claude");
    assert!(
        after.uptime <= before.uptime,
        "a restart means a fresh daemon (uptime reset): before={before:?} after={after:?}"
    );

    assert_no_frames_jsonl(acct);
    stop_daemon(acct);
}
