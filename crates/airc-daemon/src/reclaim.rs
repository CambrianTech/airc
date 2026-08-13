//! #355: daemon start RECLAIMS a wedged predecessor instead of giving up.
//!
//! A contended bind lock proves a process is ALIVE and holding it — but
//! alive is not serving. The live failure this closes (#277, and every
//! "airc keeps going down" night since): a wedged daemon holds the lock
//! forever, `airc join`/start bows out with `AlreadyRunning`, and the
//! only recovery is a human force-kill — which on one node got splinted
//! with a hand-registered OS scheduled task, the exact manual-ops class
//! the partition-tolerance contract forbids.
//!
//! The discriminator is REQUEST-RESPONSE, never connection existence
//! (#280): on contention we ping the socket with a deadline —
//! - a RESPONSIVE holder is a genuinely running daemon: we bow out and
//!   `AlreadyRunning` stands, exactly as before;
//! - an UNRESPONSIVE holder is wedged: reclaim via the pidfile
//!   kill-handle (SIGTERM → poll-gone → SIGKILL on unix; `taskkill /F`
//!   on windows — the same escalation the test reaper proved), then
//!   re-acquire the lock. Loud at every step; any doubt (no pidfile,
//!   pid is self/0, lock still contended after the kill) falls back to
//!   the honest `AlreadyRunning` error rather than guessing.

use std::path::Path;
use std::time::Duration;
#[cfg(unix)]
use std::time::Instant;

use airc_ipc::DaemonClient;

/// How long a lock-holder gets to answer a ping before it is judged
/// wedged. Generous: a healthy daemon answers in single-digit ms; a
/// busy one in well under a second. Only a wedge sits past this.
const PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// Grace between SIGTERM and SIGKILL — long enough for the daemon's
/// graceful path (socket + pidfile removal), short enough that start
/// doesn't hang on a process that stopped listening to signals.
const TERM_GRACE: Duration = Duration::from_secs(5);

/// The lock came back contended: decide whether the holder deserves it.
/// `Some(())` = the holder was wedged and has been reclaimed — retry the
/// acquire. `None` = a responsive daemon holds it (or we could not
/// safely reclaim) — the caller's `AlreadyRunning` stands.
pub(crate) async fn reclaim_wedged_holder(home: &Path, socket_path: &Path) -> Option<()> {
    if DaemonClient::new(socket_path.to_path_buf())
        .ping_with_timeout(PROBE_DEADLINE)
        .await
        .is_ok()
    {
        // Alive AND serving — the one case where bowing out is correct.
        return None;
    }
    eprintln!(
        "airc daemon: bind lock is held but {} did not answer a ping within {}s — \
         the holder is wedged, reclaiming via the pidfile kill-handle (#355)",
        socket_path.display(),
        PROBE_DEADLINE.as_secs()
    );
    let Some(pid) = read_pidfile(home) else {
        eprintln!(
            "airc daemon: no usable pid in {} — cannot name the wedged holder, \
             refusing to guess (start still fails as already-running; \
             kill the stale process by hand once, it will not recur)",
            home.join("daemon.pid").display()
        );
        return None;
    };
    eprintln!(
        "airc daemon: reclaiming wedged daemon pid {pid} (SIGTERM, then SIGKILL at {}s)",
        TERM_GRACE.as_secs()
    );
    kill_with_escalation(pid);
    Some(())
}

/// `<home>/daemon.pid`, refusing pid 0 and our own pid — a reused or
/// corrupt pidfile must never become a self-kill or an init signal.
fn read_pidfile(home: &Path) -> Option<u32> {
    let raw = std::fs::read_to_string(home.join("daemon.pid")).ok()?;
    let pid: u32 = raw.trim().parse().ok()?;
    if pid == 0 || pid == std::process::id() {
        return None;
    }
    Some(pid)
}

#[cfg(unix)]
fn kill_with_escalation(pid: u32) {
    // This crate denies `unsafe`, so signals go through the POSIX
    // `kill` binary instead of libc — same semantics, no unsafe block.
    // `kill -0` is the liveness probe (no side effects).
    let signal = |sig: &str| {
        std::process::Command::new("kill")
            .args([sig, &pid.to_string()])
            .output()
    };
    let alive = || matches!(signal("-0"), Ok(out) if out.status.success());
    let _ = signal("-TERM");
    let deadline = Instant::now() + TERM_GRACE;
    while Instant::now() < deadline {
        if !alive() {
            return; // graceful exit — lock released by the OS
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    eprintln!(
        "airc daemon: pid {pid} ignored SIGTERM for {}s — SIGKILL",
        TERM_GRACE.as_secs()
    );
    let _ = signal("-KILL");
    // A killed process releases its flock immediately, but give the OS
    // a beat before the caller retries the acquire.
    std::thread::sleep(Duration::from_millis(100));
}

#[cfg(windows)]
fn kill_with_escalation(pid: u32) {
    // taskkill is present on every supported Windows; /T takes the
    // daemon's children, /F forces — the wedged holder already proved
    // it won't exit politely (#277: `airc stop` leaves it standing).
    let _ = std::process::Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .output();
    std::thread::sleep(Duration::from_millis(500));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// what this catches (#355 safety line): the kill-handle read must
    /// refuse pid 0 (signals the whole process group) and our OWN pid
    /// (a stale pidfile reused by the new daemon would self-kill on
    /// start) — either would turn the reclaim into the outage.
    #[test]
    fn pidfile_read_refuses_zero_and_self() {
        let dir = tempfile::tempdir().expect("tempdir");
        let pidfile = dir.path().join("daemon.pid");

        std::fs::write(&pidfile, "0\n").unwrap();
        assert_eq!(read_pidfile(dir.path()), None, "pid 0 must be refused");

        std::fs::write(&pidfile, format!("{}\n", std::process::id())).unwrap();
        assert_eq!(read_pidfile(dir.path()), None, "own pid must be refused");

        std::fs::write(&pidfile, "not-a-pid\n").unwrap();
        assert_eq!(read_pidfile(dir.path()), None, "garbage must be refused");

        std::fs::write(&pidfile, "40001\n").unwrap();
        assert_eq!(
            read_pidfile(dir.path()),
            Some(40001),
            "a real foreign pid reads"
        );
    }
}
