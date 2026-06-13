//! Card f122b5b5 — the daemon-leak fix, pinned at every layer.
//!
//! The recurring bug: `cargo test --workspace` left airc daemon
//! processes alive with `--home` under the OS temp dir. Each held RAM
//! plus an event loop; one session leaked 800+, killed by hand. This
//! suite pins the three defenses so a regression FAILS here instead of
//! shipping.
//!
//! Layer 1 — RAII teardown: a daemon spawned under a guarded temp home
//! is reaped when the guard drops (the helper only returns the guarded
//! form, so a test can't forget). Mutation: delete the guard's Drop and
//! `leaked_daemon_is_reaped_by_guard_drop` fails.
//!
//! Layer 2 — belt-and-braces self-exit: a temp-home daemon with NO
//! client exits BY ITSELF after the idle window, catching kill-escapes
//! (SIGKILLed runner). Mutation: disable the watchdog and
//! `temp_home_daemon_self_exits_when_idle` fails.
//!
//! Layer 3 — zero-leak guard bite: the reaper finds a spawned daemon
//! and leaves zero behind. Mutation: make the daemon skip its pidfile
//! and `reaper_finds_and_kills_a_spawned_daemon` fails to find it.
//!
//! Unix-only: the reaper uses `kill(2)`; the hosted matrix runs
//! ubuntu + macos, so all three are pinned on both.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn airc() -> &'static str {
    env!("CARGO_BIN_EXE_airc")
}

/// Spawn the real `airc daemon` against `home` on `socket`, hermetic
/// (no gh rendezvous). Returns the child so the test can reason about
/// the process even though teardown is the guard's job.
fn spawn_daemon(home: &Path, socket: &Path, idle_ms: Option<u64>) -> Child {
    std::fs::create_dir_all(home).expect("daemon home");
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.join("daemon-test.log"))
        .expect("daemon log");
    let stderr = log.try_clone().expect("clone log");
    let mut command = Command::new(airc());
    command
        .arg("--home")
        .arg(home)
        .arg("daemon")
        .arg("--socket")
        .arg(socket)
        .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr));
    if let Some(ms) = idle_ms {
        command.env("AIRC_TEMP_HOME_IDLE_EXIT_MS", ms.to_string());
    }
    command.spawn().expect("daemon must spawn")
}

/// Poll an OWNED child until it exits, up to `budget`. Returns true if
/// it terminated. Unlike `kill(pid, 0)`, this reaps the zombie a killed
/// child leaves behind (the in-test daemon is our child; a *real*
/// leaked daemon is detached + reparented to init, where the guard's
/// `kill`-based reaper is correct). So tests confirm death through the
/// `Child` handle; the reaper itself is still exercised for the signal.
fn wait_child_gone(child: &mut Child, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return true,
            Ok(None) => {}
            Err(_) => return false,
        }
        if Instant::now() >= deadline {
            return matches!(child.try_wait(), Ok(Some(_)));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Wait until `<home>/daemon.pid` names a live process, or panic.
fn wait_for_pidfile(root: &Path, budget: Duration) -> u32 {
    let deadline = Instant::now() + budget;
    loop {
        if let Some(pid) = common::find_daemon_pid_under(root) {
            if common::pid_alive(pid) {
                return pid;
            }
        }
        if Instant::now() >= deadline {
            let log = std::fs::read_to_string(root.join(".airc").join("daemon-test.log"))
                .unwrap_or_default();
            panic!("daemon never wrote a live pidfile under {root:?}; log:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Layer 1 — RAII teardown. A daemon spawned under a guarded temp home
/// is alive while the guard lives and gone once it drops. This is THE
/// leak the card measured.
///
/// MUTATION PIN: delete `impl Drop for DaemonTempDir` (or its
/// `reap_daemons_under` call) and this test fails — the daemon
/// survives the guard drop.
#[test]
fn leaked_daemon_is_reaped_by_guard_drop() {
    let guard = common::daemon_tempdir();
    let root = guard.path().to_path_buf();
    let home = root.join(".airc");
    let socket = home.join("daemon.sock");

    let mut child = spawn_daemon(&home, &socket, None);
    let pid = wait_for_pidfile(&root, Duration::from_secs(10));
    assert!(common::pid_alive(pid), "daemon must be live before drop");

    // Drop the guard — its Drop reaps every daemon under the tree.
    drop(guard);

    assert!(
        wait_child_gone(&mut child, Duration::from_secs(8)),
        "daemon pid {pid} must be reaped when the guarded tempdir drops"
    );
}

/// Layer 3 bite — the reaper actively finds and kills a spawned daemon
/// (not just "drop cleaned up"): pin that `find_daemon_pid_under`
/// locates it and `reap_daemons_under` leaves zero behind. This is the
/// mechanism the CI zero-leak guard relies on.
///
/// MUTATION PIN: make the daemon skip writing its pidfile and the
/// reaper can't find it — `find_daemon_pid_under` returns None and the
/// first assert fails.
#[test]
fn reaper_finds_and_kills_a_spawned_daemon() {
    let guard = common::daemon_tempdir();
    let root = guard.path().to_path_buf();
    let home = root.join(".airc");
    let socket = home.join("daemon.sock");

    let mut child = spawn_daemon(&home, &socket, None);
    let pid = wait_for_pidfile(&root, Duration::from_secs(10));

    assert_eq!(
        common::find_daemon_pid_under(&root),
        Some(pid),
        "the reaper must locate the spawned daemon by its pidfile"
    );

    common::reap_daemons_under(&root);

    assert!(
        wait_child_gone(&mut child, Duration::from_secs(8)),
        "reap_daemons_under must leave zero temp-home daemons alive (pid {pid})"
    );
    // guard drop is now a no-op (already reaped) — proves idempotence.
}

/// ps-scan layer — the reaper kills a daemon whose `daemon.pid` is NOT
/// discoverable under `root` (its home derivation put the pidfile
/// elsewhere, or it never wrote one) purely from its `--home` argument
/// being under `root`. This is the belt-and-braces that fixed the
/// macOS-only leak the CI zero-leak guard caught: a daemon whose pidfile
/// landed outside the walked tree.
///
/// MUTATION PIN: delete `pids_from_process_scan` (or its union in
/// `reap_daemons_under`) and, with the pidfile removed, the reaper can
/// no longer find the daemon — `wait_child_gone` times out and this
/// fails.
#[cfg(unix)]
#[test]
fn reaper_ps_scan_kills_daemon_with_no_discoverable_pidfile() {
    let guard = common::daemon_tempdir();
    let root = guard.path().to_path_buf();
    let home = root.join(".airc");
    let socket = home.join("daemon.sock");

    let mut child = spawn_daemon(&home, &socket, None);
    let pid = wait_for_pidfile(&root, Duration::from_secs(10));

    // Simulate the failure mode: the pidfile is not where the reaper's
    // tree-walk looks (deleted here). Only the ps-scan over `--home`
    // can still find the live daemon.
    let _ = std::fs::remove_file(home.join("daemon.pid"));
    assert_eq!(
        common::find_daemon_pid_under(&root),
        None,
        "precondition: no pidfile is discoverable under root"
    );

    common::reap_daemons_under(&root);

    assert!(
        wait_child_gone(&mut child, Duration::from_secs(8)),
        "the ps-scan must reap a daemon whose --home is under root even \
         with no discoverable pidfile (pid {pid})"
    );
}

/// Layer 2 — belt-and-braces self-exit. A temp-home daemon with no
/// connected client must exit BY ITSELF after the (test-shortened)
/// idle window, WITHOUT any teardown. This is what catches a SIGKILLed
/// test runner whose Drop guards never ran.
///
/// MUTATION PIN: disable `spawn_temp_home_idle_watchdog` (return None
/// unconditionally, or never fire the notifier) and this test fails —
/// the daemon runs forever and `wait_pid_gone` times out.
#[test]
fn temp_home_daemon_self_exits_when_idle() {
    let guard = common::daemon_tempdir();
    let root = guard.path().to_path_buf();
    let home = root.join(".airc");
    let socket = home.join("daemon.sock");

    // 800ms idle window — long enough the daemon binds and settles,
    // short enough the test is quick. No client ever connects.
    let mut child = spawn_daemon(&home, &socket, Some(800));
    let pid = wait_for_pidfile(&root, Duration::from_secs(10));

    assert!(
        wait_child_gone(&mut child, Duration::from_secs(15)),
        "a temp-home daemon with no client must self-exit after the idle \
         window (card f122b5b5 belt-and-braces); pid {pid} still alive"
    );

    // And the policy named itself loudly in the log.
    let log = std::fs::read_to_string(home.join("daemon-test.log")).unwrap_or_default();
    assert!(
        log.contains("temp-home idle self-exit"),
        "the self-exit must be loud and name the policy; log:\n{log}"
    );
}

/// Layer 4 — socket-path derivation. A daemon given an EXPLICIT temp
/// home must place its socket UNDER that home, never under the
/// operator's `~/.airc/runtime`. The spawn above passes an explicit
/// `--socket` under the home; this test instead lets the CLI DERIVE
/// the socket (the production path) and asserts where it lands.
///
/// MUTATION PIN: revert `resolve_socket_path`'s isolated branch to the
/// runtime-dir placement and the derived socket escapes the home →
/// this fails. (The pure-function form is also pinned in cli.rs unit
/// tests; this is the end-to-end witness.)
#[test]
fn explicit_temp_home_keeps_its_socket_under_the_home() {
    let guard = common::daemon_tempdir();
    let root = guard.path().to_path_buf();
    // An isolated home OUTSIDE any user account — `airc status`
    // spawn-or-connects a daemon and derives its socket.
    let home = root.join(".airc");

    let out = Command::new(airc())
        .arg("--home")
        .arg(&home)
        .arg("status")
        .env("AIRC_DISABLE_ACCOUNT_REGISTRY", "1")
        .output()
        .expect("airc status runs");
    assert!(
        out.status.success(),
        "airc status failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    // Whatever socket got created, it must be under the temp root —
    // not under any `.airc/runtime` outside it. Scan for *.sock files.
    let socks = find_sockets_under(&root);
    assert!(
        !socks.is_empty(),
        "the daemon must have created a socket under the temp home"
    );
    for sock in &socks {
        assert!(
            sock.starts_with(&root),
            "an explicit temp home's socket must stay under the home \
             (card f122b5b5 socket-path bug), found {sock:?} outside {root:?}"
        );
    }
}

fn find_sockets_under(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|e| e.to_str()) == Some("sock") {
                out.push(path);
            }
        }
    }
    out
}
