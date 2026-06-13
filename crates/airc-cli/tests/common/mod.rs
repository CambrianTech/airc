//! Card f122b5b5 — RAII teardown for CLI-spawned daemons.
//!
//! THE leak: almost every `airc` verb spawn-or-connects a DETACHED
//! daemon (`ensure_daemon_running` → `setsid`/`DETACHED_PROCESS`), so
//! any integration test that runs the binary against a tempdir home
//! strands that daemon when the test ends — it outlives the test
//! binary BY DESIGN (it's a daemon). Measured in one session: ~40,
//! 131, 405+86, 154 leaked temp-home daemons across `cargo test`
//! runs; 800+ killed by hand.
//!
//! The fix shape: tests never hold a raw `TempDir` for a home a daemon
//! may serve — they hold a [`DaemonTempDir`], whose Drop walks the
//! tree for the `daemon.pid` files every daemon now writes
//! (airc-daemon `PidFileGuard`) and reaps those processes
//! (SIGTERM → poll-gone → SIGKILL). Because the helper ONLY returns
//! the guarded form, a test cannot forget teardown; the guard runs on
//! every exit path, including assert panics.
//!
//! Shared by multiple test binaries; not every helper is used by each,
//! hence `dead_code` is allowed.
#![allow(dead_code)]

use std::path::Path;
use std::time::{Duration, Instant};

/// A tempdir that reaps every daemon whose home lives under it when
/// dropped. Drop-in replacement for the `TempDir` the tests used to
/// hold: same `path()` surface.
pub struct DaemonTempDir {
    // Our Drop impl runs BEFORE the field's Drop (Rust drop order), so
    // daemons are reaped while their home still exists, then TempDir
    // deletes the tree.
    dir: tempfile::TempDir,
}

/// The ONLY way tests get a home-bearing tempdir — returns the guarded
/// form so teardown cannot be forgotten (card f122b5b5).
pub fn daemon_tempdir() -> DaemonTempDir {
    DaemonTempDir {
        dir: tempfile::TempDir::new().expect("create guarded tempdir"),
    }
}

impl DaemonTempDir {
    pub fn path(&self) -> &Path {
        self.dir.path()
    }
}

impl Drop for DaemonTempDir {
    fn drop(&mut self) {
        reap_daemons_under(self.dir.path());
    }
}

/// Reap every airc daemon belonging to `root`: SIGTERM, poll until
/// gone (the daemon's graceful path removes its socket + pidfile),
/// escalate to SIGKILL at the deadline. Pid 0 / our own pid are never
/// signalled.
///
/// TWO discovery sources, unioned, because a daemon's `daemon.pid`
/// lands in `machine_account_home(home)` — which, depending on whether
/// the spawning process had `HOME` pointed at the tempdir, may be
/// `<root>/.airc`, `<scope>/.airc`, or the isolated scope itself. The
/// pid-file walk catches the common cases, but a `ps`-scan for any
/// `airc … daemon` whose `--home`/`--socket` argument lives under
/// `root` is the belt-and-braces that catches the rest (the macOS-only
/// isolated-`codex`-scope leak the CI guard found). Process scan is
/// Unix-only; Windows relies on the pid-file walk + the daemon's own
/// temp-home self-exit.
pub fn reap_daemons_under(root: &Path) {
    // A few bounded sweeps, because a daemon spawned by a DETACHED
    // grandchild (`ensure_daemon_running` → `setsid`) can appear a beat
    // AFTER the test body returns — on a loaded CI runner it may not
    // exist yet at the first sweep (the macOS race the zero-leak guard
    // caught). Re-scan a couple times with a short settle so a
    // late-arriving daemon is still reaped. Stops early once a sweep
    // finds nothing.
    for sweep in 0..4 {
        let mut pids = pids_from_pidfiles(root);
        pids.extend(pids_from_process_scan(root));
        pids.sort_unstable();
        pids.dedup();
        if pids.is_empty() {
            if sweep > 0 {
                return; // a clean follow-up sweep — nothing late arrived
            }
        } else {
            for pid in pids {
                reap_pid(pid);
            }
        }
        // Brief settle for a detached daemon mid-spawn to become visible.
        std::thread::sleep(Duration::from_millis(150));
    }
}

/// `daemon.pid` files anywhere under `root`.
fn pids_from_pidfiles(root: &Path) -> Vec<u32> {
    let mut pids = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("daemon.pid") {
                if let Some(pid) = read_pid(&path) {
                    pids.push(pid);
                }
            }
        }
    }
    pids
}

/// Running `airc … daemon …` processes whose command line references a
/// path under `root` (its `--home` or `--socket`). Catches daemons
/// whose pid file landed outside the walked tree, or never got written.
#[cfg(unix)]
fn pids_from_process_scan(root: &Path) -> Vec<u32> {
    let Some(root_str) = root.to_str() else {
        return Vec::new();
    };
    let output = match std::process::Command::new("ps")
        .args(["-Ao", "pid=,args="])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        _ => return Vec::new(),
    };
    let me = std::process::id();
    let mut pids = Vec::new();
    for line in String::from_utf8_lossy(&output).lines() {
        let line = line.trim_start();
        let Some((pid_str, args)) = line.split_once(char::is_whitespace) else {
            continue;
        };
        // An airc daemon process whose args mention a path under root.
        if !(args.contains("airc") && args.contains(" daemon") && args.contains(root_str)) {
            continue;
        }
        if let Ok(pid) = pid_str.trim().parse::<u32>() {
            if pid != 0 && pid != me {
                pids.push(pid);
            }
        }
    }
    pids
}

#[cfg(not(unix))]
fn pids_from_process_scan(_root: &Path) -> Vec<u32> {
    Vec::new()
}

fn read_pid(path: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(path).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    Some(pid)
}

#[cfg(unix)]
fn reap_pid(pid: u32) {
    let pid = pid as libc::pid_t;
    // SAFETY: kill with a valid signal number; worst case ESRCH.
    unsafe {
        libc::kill(pid, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        // SAFETY: signal 0 = existence probe, no side effects.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return; // gone
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    // SAFETY: as above; the daemon ignored SIGTERM long enough.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
    }
}

#[cfg(windows)]
fn reap_pid(pid: u32) {
    // Best-effort forced kill; `taskkill` is present on every
    // supported Windows. /T takes the daemon's children with it.
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
}

/// Find the pid of a daemon serving a home under `root`, if any —
/// lets tests assert both directions (daemon IS up; daemon is GONE).
pub fn find_daemon_pid_under(root: &Path) -> Option<u32> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) == Some("daemon.pid") {
                if let Some(pid) = read_pid(&path) {
                    return Some(pid);
                }
            }
        }
    }
    None
}

/// True while `pid` names a live process.
#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 = existence probe, no side effects.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(windows)]
pub fn pid_alive(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

/// Poll until `pid` is gone, up to `budget`. Returns true if it exited.
pub fn wait_pid_gone(pid: u32, budget: Duration) -> bool {
    let deadline = Instant::now() + budget;
    while Instant::now() < deadline {
        if !pid_alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    !pid_alive(pid)
}
