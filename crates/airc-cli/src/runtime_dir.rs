//! Runtime directory for the daemon's IPC socket.
//!
//! Card 50d1728b. The socket is the rendezvous point for a
//! MACHINE-SINGULAR daemon, so its path MUST be machine-stable — the
//! same value for every process, tab, and shell on this user's
//! machine. It is `~/.airc/runtime`, full stop. That is where all
//! other airc state already lives (`~/.airc/wires`, `~/.airc/worktrees`,
//! identity/trust), so the socket belongs there too.
//!
//! History — why this used to be more complicated (and wrong): card
//! 7e88c34d (replacing the `/tmp/airc-discovery-<uid>/` layer from
//! #1036) tried to be platform-clever — prefer `$XDG_RUNTIME_DIR` on
//! Linux, `$TMPDIR/airc` on macOS, `~/.airc/runtime` only as a
//! fallback. That DEFEATED machine-singularity: `$TMPDIR` is
//! per-SESSION on macOS and is freely overridden by harnesses (the
//! Claude Code harness sets `TMPDIR=/tmp/claude-<id>`; terminals carry
//! `/var/folders/.../T`; a daemon spawned with no `$TMPDIR` fell to
//! `~/.airc/runtime`). So the same binary, with the same machine-hash
//! in the socket NAME, resolved a DIFFERENT directory per process —
//! and each one started its own daemon. Observed 2026-05-29: 4 live
//! daemons + 18 client connections fragmented across them. The hash
//! made the socket name machine-stable; the env-derived directory
//! un-did it.
//!
//! The fix is to stop deriving the directory from per-session env at
//! all. `$AIRC_RUNTIME_DIR` remains as the ONE explicit override — its
//! only real consumer is the integration suite, which points ephemeral
//! test daemons at throwaway dirs so they don't collide with the real
//! machine daemon. Normal operation never sets it.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The runtime directory for `airc` — where `airc-<hash>-v<N>.sock`
/// files live. Created if absent.
///
/// Resolution:
///   1. `$AIRC_RUNTIME_DIR` if set — explicit override, honored
///      verbatim. Exists for TEST ISOLATION (ephemeral daemons in
///      throwaway dirs); normal operation never sets it.
///   2. `~/.airc/runtime` — the machine-stable default. Always this.
///
/// Deliberately does NOT consult `$TMPDIR`/`$XDG_RUNTIME_DIR`: those
/// are per-session and would fragment the machine-singular daemon (see
/// module docs / card 50d1728b).
///
/// Returns `Err` only if `$HOME`/`$USERPROFILE` is unset (so the
/// default can't be built) or the directory can't be created.
pub fn runtime_dir() -> std::io::Result<PathBuf> {
    let dir = resolve_runtime_dir(
        std::env::var_os("AIRC_RUNTIME_DIR"),
        std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")),
    )?;
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Pure path resolution — no env reads, no filesystem. `override_dir`
/// is `$AIRC_RUNTIME_DIR`; `home` is `$HOME`/`$USERPROFILE`. Extracted
/// so the machine-stable invariant is testable WITHOUT mutating
/// process-global env (which races across parallel tests), and so the
/// type signature itself proves no per-session var (`$TMPDIR`,
/// `$XDG_RUNTIME_DIR`) can influence the path.
fn resolve_runtime_dir(
    override_dir: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> std::io::Result<PathBuf> {
    if let Some(explicit) = override_dir {
        return Ok(PathBuf::from(explicit));
    }
    let home = home.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "no $HOME or $USERPROFILE — cannot resolve ~/.airc/runtime",
        )
    })?;
    Ok(PathBuf::from(home).join(".airc").join("runtime"))
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

    /// Card 50d1728b — the machine-singular invariant. With no
    /// override, the runtime dir is ALWAYS `$HOME/.airc/runtime`,
    /// regardless of any per-session env. Because `resolve_runtime_dir`
    /// takes only the override and home, no `$TMPDIR`/`$XDG_RUNTIME_DIR`
    /// can reach it — this test plus the signature is the guard against
    /// the regression that fragmented the mesh into 4 daemons.
    #[test]
    fn resolve_defaults_to_home_dot_airc_runtime() {
        let resolved = resolve_runtime_dir(None, Some(std::ffi::OsString::from("/home/jane")))
            .expect("home is set");
        assert_eq!(resolved, PathBuf::from("/home/jane/.airc/runtime"));
    }

    #[test]
    fn resolve_honors_explicit_override_verbatim() {
        let resolved = resolve_runtime_dir(
            Some(std::ffi::OsString::from("/tmp/test-iso/airc")),
            Some(std::ffi::OsString::from("/home/jane")),
        )
        .expect("override wins");
        // Override is honored verbatim AND ignores home entirely.
        assert_eq!(resolved, PathBuf::from("/tmp/test-iso/airc"));
    }

    #[test]
    fn resolve_errors_without_home_and_no_override() {
        let err = resolve_runtime_dir(None, None).expect_err("no home, no override → error");
        assert_eq!(err.kind(), std::io::ErrorKind::NotFound);
    }

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
