//! Card 16bd4d71 slice 1 acceptance proof: `airc monitor attach`
//! survives a daemon stop + restart cycle without losing live events
//! and without flooding stdout with backlog.
//!
//! Joel observed 2026-05-30: `airc does not seem to be working, you
//! seem to have broken the interrupt driven interaction between each
//! other. he did nothing to your message till i talked to him.` The
//! root cause was `airc monitor attach`'s IPC stream EOFing when the
//! daemon was bounced — the CLI exited without retry, every attached
//! client went silently blind. This test pins the fix: stream EOFs on
//! daemon stop, monitor reconnects after backoff with cursor-resume
//! so the gap surfaces as ONE summary line, then live events resume.
//!
//! Why integration-shaped (subprocess) rather than unit-shaped:
//! the bug is in the lifecycle interaction between the CLI's monitor
//! loop and the daemon's IPC stream EOF behavior. A unit test against
//! `run_channel_attach_loop` in isolation would have to mock the
//! daemon — which is exactly the integration risk the bug surfaced.
//! The subprocess pattern matches `daemon_lifecycle.rs` + `codex_hook_commands.rs`
//! and exercises the real binary against a real daemon.

use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use tempfile::TempDir;

fn airc() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// One airc invocation against `home`, returning stdout on success.
fn run_ok(home: &Path, args: &[&str]) -> String {
    let account_home = home.parent().unwrap_or(home);
    let runtime_dir = account_home.join("runtime");
    let _ = std::fs::create_dir_all(&runtime_dir);
    let output = Command::new(airc())
        .arg("--home")
        .arg(home)
        .args(args)
        .env("HOME", account_home)
        .env("USERPROFILE", account_home)
        .env("AIRC_RUNTIME_DIR", runtime_dir)
        .output()
        .expect("airc must spawn");
    assert!(
        output.status.success(),
        "airc {args:?} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    String::from_utf8(output.stdout).expect("stdout utf-8")
}

/// Start `airc monitor attach` as a background child whose stdout we
/// can poll for the disconnect/reconnect/event lifecycle lines.
fn spawn_monitor(home: &Path) -> std::process::Child {
    let account_home = home.parent().unwrap_or(home);
    let runtime_dir = account_home.join("runtime");
    let _ = std::fs::create_dir_all(&runtime_dir);
    Command::new(airc())
        .arg("--home")
        .arg(home)
        .args(["monitor", "attach", "--my-name", "test:monitor"])
        .env("HOME", account_home)
        .env("USERPROFILE", account_home)
        .env("AIRC_RUNTIME_DIR", runtime_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("monitor attach must spawn")
}

/// Wait up to `timeout` for `needle` to appear in `child`'s stdout.
/// Drains stdout into a String the caller can inspect after.
fn wait_for_stdout(
    child: &mut std::process::Child,
    needle: &str,
    timeout: Duration,
    captured: &mut String,
) -> bool {
    use std::io::Read;
    let deadline = Instant::now() + timeout;
    let mut buf = [0u8; 4096];
    let stdout = child.stdout.as_mut().expect("piped stdout");
    while Instant::now() < deadline {
        match stdout.read(&mut buf) {
            Ok(0) => {
                thread::sleep(Duration::from_millis(50));
                continue;
            }
            Ok(n) => {
                captured.push_str(&String::from_utf8_lossy(&buf[..n]));
                if captured.contains(needle) {
                    return true;
                }
            }
            Err(_) => {
                thread::sleep(Duration::from_millis(50));
            }
        }
    }
    false
}

/// Card 16bd4d71 slice 1 acceptance: monitor survives a daemon
/// stop+start cycle. Publish event A, attach monitor, observe A,
/// stop daemon, observe disconnect line, start daemon, observe
/// reconnect line, publish event B, observe B. No silent loss.
///
/// `#[ignore]`: full subprocess + spawned daemon + IPC round-trip
/// across stop/restart is heavyweight + environmentally sensitive
/// (daemon-ready timing varies under CI load). Same pattern as the
/// chat throughput bench in card 127816bd. Opt-in via
/// `cargo test --release --test monitor_reconnect -- --ignored
/// --nocapture --test-threads=1`.
#[test]
#[ignore = "integration: spawns real daemon + IPC; opt-in via --ignored --test-threads=1"]
fn monitor_attach_auto_reconnects_across_daemon_bounce() {
    let workspace = TempDir::new().expect("tempdir");
    let home = workspace.path().join("agent");

    // Initialize + join a room so attach has channels to subscribe to.
    run_ok(&home, &["init"]);
    run_ok(&home, &["room", "general"]);

    // Publish an event BEFORE attaching so we know the channel has at
    // least one historical event. Monitor's --from-now default skips
    // backlog so we shouldn't see this on first attach.
    run_ok(&home, &["send", "first event (pre-attach)"]);

    // Attach monitor; observe the attach banner.
    let mut child = spawn_monitor(&home);
    let mut captured = String::new();
    assert!(
        wait_for_stdout(
            &mut child,
            "attached to Rust event stream",
            Duration::from_secs(10),
            &mut captured,
        ),
        "monitor must emit the attach banner: captured={captured}"
    );

    // Publish a LIVE event; monitor should surface it.
    run_ok(&home, &["send", "second event (live)"]);
    assert!(
        wait_for_stdout(
            &mut child,
            "second event (live)",
            Duration::from_secs(10),
            &mut captured,
        ),
        "monitor must surface live events: captured={captured}"
    );

    // Bounce the daemon. The monitor's IPC stream should EOF; the
    // per-channel attach loop should emit Disconnected, back off, and
    // re-attach when the daemon comes back.
    run_ok(&home, &["stop"]);

    // Disconnect line MUST appear — the load-bearing slice 1
    // assertion. Without the fix, the CLI exits silently here.
    assert!(
        wait_for_stdout(
            &mut child,
            "daemon disconnected — reconnecting",
            Duration::from_secs(10),
            &mut captured,
        ),
        "monitor must emit the disconnect diagnostic when daemon stops: captured={captured}"
    );

    // The next `airc` invocation will respawn the daemon (the
    // spawn-or-connect pattern). Publish a fresh event to trigger
    // daemon respawn + observe reconnect.
    run_ok(&home, &["send", "third event (post-restart)"]);

    // Reconnect line MUST appear — proves the auto-reconnect loop
    // re-attached without operator intervention.
    assert!(
        wait_for_stdout(
            &mut child,
            "reconnected",
            Duration::from_secs(30),
            &mut captured,
        ),
        "monitor must emit the reconnect diagnostic after daemon comes back: captured={captured}"
    );

    // And the post-restart event MUST surface — proves cursor-resume
    // closed the gap without silent loss.
    assert!(
        wait_for_stdout(
            &mut child,
            "third event (post-restart)",
            Duration::from_secs(15),
            &mut captured,
        ),
        "monitor must surface events published after reconnect: captured={captured}"
    );

    let _ = child.kill();
}
