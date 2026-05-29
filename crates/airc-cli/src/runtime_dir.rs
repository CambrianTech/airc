//! Platform-correct runtime directory for the daemon's IPC socket and
//! other ephemeral runtime artifacts.
//!
//! Card 7e88c34d. Replaces the `/tmp/airc-discovery-<uid>/` indirection
//! from card 282850c2 / PR #1036. Joel pushback 2026-05-28:
//! "still using tmp ugh" + "we have our own DIR man, does claude code
//! use tmp?". `/tmp` is the lowest-common-denominator but the wrong
//! answer:
//!
//!   - Linux has `$XDG_RUNTIME_DIR` (`/run/user/<uid>/`) — the standard
//!     location for runtime sockets, tmpfs-backed, per-user, cleaned
//!     on logout.
//!   - macOS has `$TMPDIR` per-user under `/var/folders/.../T/` —
//!     sandbox-passthrough in the common case, not `/tmp`.
//!   - Windows has `%LOCALAPPDATA%\airc\runtime\`.
//!
//! All three are reachable across sandbox boundaries in the common
//! case AND they're namespaced. The daemon socket lives here as
//! `airc-<project-hash>.sock`; agents in the same project compute the
//! same hash → find the same socket → no discovery file needed.
//!
//! The discovery module (`crates/airc-cli/src/discovery.rs` from
//! #1036) becomes vestigial once every caller uses this resolution.
//! It stays during the transition and is removed in a follow-up.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The platform's runtime directory for `airc`. Created if absent.
/// Returns the path where `airc-<hash>.sock` files belong.
///
/// Resolution order:
///   1. `$AIRC_RUNTIME_DIR` (explicit override; honored anywhere)
///   2. `$XDG_RUNTIME_DIR/airc` (Linux convention)
///   3. `$TMPDIR/airc` (macOS — user-private under /var/folders)
///   4. `~/.airc/runtime` (fallback, home-private)
///
/// Returns `Err` only if the chosen path can't be created — every
/// platform should reach at least case 4.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    if let Some(explicit) = std::env::var_os("AIRC_RUNTIME_DIR") {
        let dir = PathBuf::from(explicit);
        std::fs::create_dir_all(&dir)?;
        return Ok(dir);
    }
    if let Some(xdg) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(xdg).join("airc");
        std::fs::create_dir_all(&dir)?;
        return Ok(dir);
    }
    // On macOS, std::env::temp_dir() returns $TMPDIR which IS
    // per-user under /var/folders. On Linux without XDG_RUNTIME_DIR
    // it returns /tmp — we'd rather fall through to ~/.airc/runtime
    // in that case, but distinguishing is hard. Use $TMPDIR ONLY
    // when it's not /tmp.
    if let Some(tmpdir) = std::env::var_os("TMPDIR") {
        let path = PathBuf::from(&tmpdir);
        if path.as_os_str() != "/tmp" && path.starts_with("/var/") {
            let dir = path.join("airc");
            std::fs::create_dir_all(&dir)?;
            return Ok(dir);
        }
    }
    // Last resort: ~/.airc/runtime/. Home-private (sandboxed agents
    // without $HOME pass-through can't reach it), but it's namespaced
    // and persistent.
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "no $HOME or $USERPROFILE — cannot resolve any runtime dir",
            )
        })?;
    let dir = PathBuf::from(home).join(".airc").join("runtime");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Project-derived socket path. The hash of the canonicalized project
/// root is stable across runs, so every agent in the same project
/// computes the same socket path and reaches the same daemon — no
/// discovery indirection needed.
///
/// `project_root` is typically the git working tree (the parent of
/// `.airc/`). The hash is 16 hex chars of SHA-256, collision-resistant
/// at the scale of "projects on one machine."
pub fn project_socket_path(project_root: &Path) -> std::io::Result<PathBuf> {
    let dir = runtime_dir()?;
    let key = project_key(project_root);
    Ok(dir.join(format!(
        "airc-{}-v{}.sock",
        key,
        airc_ipc::IPC_PROTOCOL_VERSION
    )))
}

/// 16-char hex prefix of SHA-256(canonical(project_root)). Same scheme
/// as `discovery::project_key` so the transition produces consistent
/// keys; once discovery is removed this is the only definition.
fn project_key(project_root: &Path) -> String {
    let canon = project_root
        .canonicalize()
        .unwrap_or_else(|_| project_root.to_path_buf());
    let mut h = Sha256::new();
    h.update(canon.as_os_str().as_encoded_bytes());
    let digest = h.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(16);
    for b in &digest[..8] {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0xf) as usize] as char);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_key_is_stable_across_calls() {
        let a = project_key(Path::new("/Users/joel/Development/airc"));
        let b = project_key(Path::new("/Users/joel/Development/airc"));
        assert_eq!(a, b);
        assert_eq!(a.len(), 16);
    }

    #[test]
    fn project_key_distinguishes_paths() {
        let a = project_key(Path::new("/Users/joel/Development/airc"));
        let b = project_key(Path::new("/Users/joel/Development/continuum"));
        assert_ne!(a, b);
    }

    #[test]
    fn runtime_dir_honors_airc_runtime_dir_env() {
        // Use a unique path to avoid colliding with other test runs
        let unique = std::env::temp_dir().join(format!(
            "airc-runtime-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        // SAFETY: tests are single-threaded by default; env mutation
        // is acceptable here. We restore after.
        let previous = std::env::var_os("AIRC_RUNTIME_DIR");
        // SAFETY: set_var is unsafe in Rust 2024 because of inherent
        // thread-safety concerns; tests run serially so this is OK.
        // SAFETY: env::set_var is unsafe in Rust 2024 due to inherent
        // thread-safety concerns; tests run serially so this is OK.
        unsafe {
            std::env::set_var("AIRC_RUNTIME_DIR", &unique);
        }
        let resolved = runtime_dir().expect("runtime_dir succeeds with explicit override");
        assert_eq!(resolved, unique, "explicit override is honored verbatim");
        // SAFETY: env::set_var is unsafe in Rust 2024 due to inherent
        // thread-safety concerns; tests run serially so this is OK.
        unsafe {
            match previous {
                Some(v) => std::env::set_var("AIRC_RUNTIME_DIR", v),
                None => std::env::remove_var("AIRC_RUNTIME_DIR"),
            }
        }
        let _ = std::fs::remove_dir_all(&unique);
    }

    #[test]
    fn project_socket_path_contains_hash_and_version() {
        // Confirm the filename shape: airc-<16hex>-v<N>.sock
        let unique = std::env::temp_dir().join(format!(
            "airc-socket-shape-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
        ));
        // SAFETY: env::set_var is unsafe in Rust 2024 due to inherent
        // thread-safety concerns; tests run serially so this is OK.
        unsafe {
            std::env::set_var("AIRC_RUNTIME_DIR", &unique);
        }
        let pr = Path::new("/Users/joel/Development/airc");
        let socket = project_socket_path(pr).expect("succeeds");
        let name = socket.file_name().unwrap().to_str().unwrap();
        assert!(name.starts_with("airc-"), "name starts with airc-: {name}");
        assert!(name.ends_with(".sock"), "extension is .sock: {name}");
        assert!(
            name.contains(&format!("v{}", airc_ipc::IPC_PROTOCOL_VERSION)),
            "version is embedded: {name}"
        );
        // SAFETY: env::set_var is unsafe in Rust 2024 due to inherent
        // thread-safety concerns; tests run serially so this is OK.
        unsafe {
            std::env::remove_var("AIRC_RUNTIME_DIR");
        }
        let _ = std::fs::remove_dir_all(&unique);
    }
}
