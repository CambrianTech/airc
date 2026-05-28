//! Daemon discovery via a shared `/tmp/airc-discovery-<uid>/` directory.
//!
//! Card 282850c2. The daemon socket lives inside `machine_account_home(home)`
//! — typically `$HOME/.airc/daemon-v5.sock`. That works fine for normal use
//! (every scope under one user resolves to the same socket, attach is
//! transparent). But it FAILS for sandboxed agents (Codex, future personas)
//! whose `$HOME` is a per-invocation tmpdir: each invocation resolves to a
//! DIFFERENT "machine-account home" → different socket → spawns yet another
//! orphan daemon → can never join the project's actual room.
//!
//! This module adds a second discovery surface that's reachable across
//! sandbox boundaries: a file in `/tmp/airc-discovery-<uid>/` keyed by the
//! project root's hash. When a daemon starts it ANNOUNCES itself there;
//! when `ensure_daemon_running` finds no daemon at the home-resolved
//! socket, it looks at the discovery dir for a daemon serving the SAME
//! project root and attaches there instead of spawning a competitor.
//!
//! Trust model: the discovery dir is per-UID (file owner matches reader),
//! so only the same OS user can register/find. Sandboxes that grant `/tmp`
//! read-through (the common case — most are MORE restrictive on `$HOME`
//! than on `/tmp`) gain peer-attach for free.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// One daemon's announcement — written when it becomes ready, deleted
/// best-effort on shutdown, and pruned opportunistically when a reader
/// finds the PID dead. `project_root` is the git working tree the
/// daemon's home belongs to (when discoverable); other agents in that
/// project hash to the same key and find this daemon.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DiscoveredDaemon {
    pub socket: PathBuf,
    pub home: PathBuf,
    pub project_root: Option<PathBuf>,
    /// String form of the daemon's `PeerId` — the agent who owns the
    /// `local_identity` row for `home`. Informational; not used for
    /// trust decisions in this module.
    pub peer_id: String,
    pub pid: u32,
    pub started_at_ms: u64,
    /// Daemon binary's build identifier (`build_info::BUILD`).
    /// Cross-build attaches still work over the IPC version handshake;
    /// this is recorded so diagnostics can spot drift.
    pub build: String,
}

/// `/tmp/airc-discovery-<uid>/`. Per-UID so the file owner is always the
/// same OS user, eliminating cross-user collisions on a shared box.
/// Created if absent. Failure means no discovery — the spawn-or-attach
/// path falls back to the existing home-resolved socket only.
fn discovery_dir() -> std::io::Result<PathBuf> {
    // SAFETY: getuid is async-signal-safe and has no precondition.
    let uid = unsafe { libc::getuid() };
    let dir = std::env::temp_dir().join(format!("airc-discovery-{uid}"));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// 16-char hex prefix of SHA-256(canonical(project_root)). Stable across
/// runs, so subsequent daemon starts for the same project overwrite
/// the same file. Collisions are vanishingly unlikely at 64 bits.
fn project_key(project_root: &Path) -> String {
    let canon = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut h = Sha256::new();
    h.update(canon.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    // Inline hex of the first 8 bytes — avoids pulling in a `hex` crate
    // for one call site.
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

const HEX: &[u8; 16] = b"0123456789abcdef";

/// Announce a ready daemon so other agents can find it via the project
/// root. Called from `ensure_daemon_running` AFTER the readiness ping
/// succeeds — never before, because a half-started daemon being
/// announced would race with attaching peers.
pub fn announce(daemon: &DiscoveredDaemon) -> std::io::Result<()> {
    let dir = discovery_dir()?;
    let key_source = daemon.project_root.as_deref().unwrap_or(&daemon.home);
    let path = dir.join(format!("{}.json", project_key(key_source)));
    let data = serde_json::to_vec_pretty(daemon).map_err(std::io::Error::other)?;
    std::fs::write(&path, data)?;
    Ok(())
}

/// Delete this project's announcement. Best-effort: called on graceful
/// shutdown; failure is silent. Crashed daemons are pruned by
/// `live_daemons()` via the `pid_alive` check.
///
/// Not yet wired in — `airc daemon stop` and SIGTERM handling will
/// call this once those paths exist. Stale-PID detection in
/// `live_daemons()` keeps the discovery dir self-cleaning in the
/// meantime, so this is correctness-only, not blocking.
#[allow(dead_code)]
pub fn forget(project_root_or_home: &Path) {
    if let Ok(dir) = discovery_dir() {
        let path = dir.join(format!("{}.json", project_key(project_root_or_home)));
        let _ = std::fs::remove_file(path);
    }
}

/// `kill(pid, 0)` — POSIX standard process-existence check. No signal
/// is delivered; the kernel only validates that the target exists and
/// the caller has permission to signal it. Returns 0 if alive,
/// otherwise sets errno (ESRCH for dead, EPERM for live-but-unowned).
/// We treat EPERM as "alive" because for our purposes a daemon owned
/// by another OS user is still "running"; the per-UID discovery dir
/// keeps such daemons out of our view in the first place. errno is
/// read via `std::io::Error::last_os_error().raw_os_error()` for
/// cross-platform compatibility — direct `libc::__error()` access
/// is macOS-specific (Linux uses `__errno_location`).
fn pid_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) has no side effects beyond errno, and the
    // signal value 0 is the documented existence-check idiom.
    let res = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if res == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Read every `.json` file in the discovery dir, drop the ones whose
/// PID is dead (and best-effort remove the file), return the rest.
pub fn live_daemons() -> Vec<DiscoveredDaemon> {
    let Ok(dir) = discovery_dir() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(bytes) = std::fs::read(&path) else {
            continue;
        };
        let daemon: DiscoveredDaemon = match serde_json::from_slice(&bytes) {
            Ok(d) => d,
            Err(_) => {
                // Malformed file — prune so it doesn't accumulate.
                let _ = std::fs::remove_file(&path);
                continue;
            }
        };
        if pid_alive(daemon.pid) {
            out.push(daemon);
        } else {
            let _ = std::fs::remove_file(&path);
        }
    }
    out
}

/// Find a live daemon serving the same project root as `project_root`.
/// Used by `ensure_daemon_running` when nothing's reachable at the
/// home-resolved socket: a sandboxed agent whose `$HOME` is a tmpdir
/// would otherwise spawn yet another orphan; this routes them to the
/// project's actual daemon instead.
pub fn find_for_project(project_root: &Path) -> Option<DiscoveredDaemon> {
    let want = project_key(project_root);
    live_daemons().into_iter().find(|d| {
        let key_source = d.project_root.as_deref().unwrap_or(&d.home);
        project_key(key_source) == want
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_is_stable_across_calls() {
        let a = project_key(Path::new("/Users/joel/Development/airc"));
        let b = project_key(Path::new("/Users/joel/Development/airc"));
        assert_eq!(a, b, "same input → same key");
        assert_eq!(a.len(), 16, "16-char hex key");
    }

    #[test]
    fn project_key_distinguishes_paths() {
        let a = project_key(Path::new("/Users/joel/Development/airc"));
        let b = project_key(Path::new("/Users/joel/Development/continuum"));
        assert_ne!(a, b, "different projects → different keys");
    }

    #[test]
    fn pid_zero_is_not_alive() {
        assert!(!pid_alive(0), "pid 0 is never a real process");
    }

    #[test]
    fn current_pid_is_alive() {
        // SAFETY: getpid is async-signal-safe and has no precondition.
        let me = unsafe { libc::getpid() } as u32;
        assert!(
            pid_alive(me),
            "the current process is, by definition, alive"
        );
    }

    #[test]
    fn announce_then_find_then_forget_roundtrip() {
        // Use a unique project root so concurrent test invocations don't
        // collide (cargo test runs each test in its own thread but shares
        // process state — and we write to a shared dir).
        let unique = std::env::temp_dir().join(format!(
            "airc-discovery-test-{}-{}",
            // SAFETY: getpid is async-signal-safe.
            unsafe { libc::getpid() },
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&unique).unwrap_or_default();

        // SAFETY: getpid is async-signal-safe.
        let me = unsafe { libc::getpid() } as u32;
        let daemon = DiscoveredDaemon {
            socket: PathBuf::from("/tmp/fake.sock"),
            home: PathBuf::from("/tmp/fake-home"),
            project_root: Some(unique.clone()),
            peer_id: "00000000-0000-0000-0000-000000000000".to_string(),
            pid: me,
            started_at_ms: 0,
            build: "test".to_string(),
        };

        announce(&daemon).expect("announce writes file");
        let found = find_for_project(&unique).expect("find_for_project finds it");
        assert_eq!(found.pid, me);
        assert_eq!(found.socket, daemon.socket);

        forget(&unique);
        let gone = find_for_project(&unique);
        assert!(gone.is_none(), "forget removes the entry");

        let _ = std::fs::remove_dir_all(&unique);
    }
}
