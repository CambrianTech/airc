//! socket_path.rs — the canonical airc IPC socket derivation, in a LIBRARY.
//!
//! ## Why this is here and not in the CLI
//!
//! This derivation used to live in `airc-cli`, which is a BINARY-ONLY crate. Nothing
//! outside the `airc` executable could call it, so every other program that needed the
//! socket path had to ask by SPAWNING the CLI:
//!
//! ```text
//! $ airc ipc-endpoint      # -> println!("{}", default_socket_path_in(&home).display())
//! ```
//!
//! That is a process launch to evaluate a pure function of a directory path. Continuum
//! did exactly that on EVERY socket re-resolution, having deprecated its own local copy
//! precisely because a second derivation drifts. The deprecation was right about the
//! problem and wrong about the remedy: the answer to "two derivations drift" is ONE
//! derivation both callers can reach, not a subprocess standing in for a function call.
//!
//! So it lives in the library now, and `airc-cli` re-exports it. There is no fallback
//! path and no second implementation — a caller that cannot reach this one is a build
//! error, not a runtime guess.

use std::path::PathBuf;

/// Default daemon IPC endpoint for `home`.
///
/// Card 7e88c34d: the socket now lives at the platform's **runtime
/// directory** keyed by the **project root** hash, not at the
/// home-private `~/.airc/daemon-v<N>.sock`. This eliminates the
/// `/tmp/airc-discovery-<uid>/` indirection from card 282850c2 /
/// PR #1036: every agent resolving the same project_root computes
/// the same socket path → reaches the same daemon → no discovery
/// file needed.
///
/// Resolution:
///   - Machine-account scope (machine_account_home resolves under
///     `$HOME` — i.e., this scope shares a daemon with every other
///     project scope on the same OS account):
///     `<runtime-dir>/airc-machine-v<N>.sock` (ONE per user, per the
///     `state.rs:36` doctrine "one daemon per machine account").
///   - Isolated scope (CI temp dir, test root — machine_account_home
///     equals the scope itself because the scope is outside `$HOME`):
///     `<project-root>/daemon-v<N>.sock` where project-root is
///     `home.parent()`. Card f122b5b5: keyed off the project root so
///     sibling scopes share one daemon, placed UNDER the home tree so
///     no hermetic test socket lands in the production `~/.airc/runtime`
///     (the prior `<runtime-dir>/airc-<project-hash>` placement did).
///
/// **Card e51ab14e**: this is the consolidation of the per-project
/// socket from card 7e88c34d / PR #1040. PR #1040 solved
/// agents-in-one-project-find-the-same-socket via project hashing,
/// but applied the same hashing to project scopes on the SAME OS
/// account, which gave each project its own daemon and broke
/// cross-scope live event delivery (see card e51ab14e body, and
/// `crates/airc-daemon/src/state.rs:36-37`). The fix keeps PR #1040's
/// runtime-dir resolution unchanged, but consolidates project scopes
/// under `$HOME` onto the machine-singular socket name. Isolated
/// scopes outside `$HOME` (the case the project-hash was originally
/// solving for) keep the per-project name so parallel test runs
/// don't collide.
///
/// `runtime-dir` resolves via [`runtime_dir::runtime_dir`]: the
///   explicit `$AIRC_RUNTIME_DIR` override (test isolation only),
///   else always `~/.airc/runtime`. Card 50d1728b: it deliberately
///   does NOT consult `$TMPDIR`/`$XDG_RUNTIME_DIR` — those are
///   per-session and fragmented the machine-singular daemon into one
///   instance per shell.
///
/// The filename includes `airc_ipc::IPC_PROTOCOL_VERSION`: if the
/// local daemon wire protocol changes, a new client must not talk
/// to an old daemon that still owns the prior socket.
///
/// On runtime_dir failure (extremely rare — only if HOME isn't set
/// and every other env hint failed), falls back to the legacy
/// home-private path so the substrate still functions; the legacy
/// path is what existed before this card and remains backwards-
/// compatible for old binaries.
pub fn default_socket_path_in(home: &std::path::Path) -> PathBuf {
    let machine_home = crate::machine_account_home(home);
    let user_home = read_user_home_from_env();
    let runtime_dir = crate::runtime_dir::runtime_dir().ok();
    // SUN_LEN fallback root for deep-home cases (see machine_socket_path).
    // The OS per-user temp dir is short; never used for normal homes.
    let short_fallback = std::env::temp_dir();
    let socket = resolve_socket_path(
        home,
        &machine_home,
        user_home.as_deref(),
        runtime_dir.as_deref(),
        Some(short_fallback.as_path()),
    );
    // runtime_dir() created ~/.airc/runtime, but the SUN_LEN fallback dir
    // (<temp>/airc) may not exist yet — ensure the chosen socket's parent
    // exists so bind() doesn't fail on a missing directory. Best-effort.
    if let Some(parent) = socket.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    socket
}

/// Pure-function variant of [`default_socket_path_in`] for tests +
/// callers that need to inject the env directly. Production code goes
/// through [`default_socket_path_in`] which reads env once and
/// delegates here; tests bypass env entirely so they don't race with
/// `cargo test`'s parallel pool (env mutation is unsound under
/// concurrent reads — Rust 1.80+ marks `set_var` `unsafe` for this
/// reason).
///
/// - `home`: the scope's home directory (the caller's `--home` /
///   `AIRC_HOME` / resolved default).
/// - `machine_home`: result of [`crate::machine_account_home`]
///   for `home` (equals `home` when scope is outside the user account,
///   `$HOME/.airc` when scope is under it).
/// - `user_home`: the OS user-home directory (`$HOME` /
///   `$USERPROFILE`). `None` ⇒ treat every scope as isolated.
/// - `runtime_dir`: result of [`crate::runtime_dir::runtime_dir`].
///   `None` ⇒ the legacy home-private fallback.
fn resolve_socket_path(
    home: &std::path::Path,
    machine_home: &std::path::Path,
    user_home: Option<&std::path::Path>,
    runtime_dir: Option<&std::path::Path>,
    short_fallback_dir: Option<&std::path::Path>,
) -> PathBuf {
    // `machine_account_home(scope_home)` returns `$HOME/.airc` when
    // scope_home is under `$HOME`, otherwise returns scope_home
    // unchanged. So if `machine_home != home` here, the scope IS
    // a project scope under `$HOME` — share the machine-singular
    // socket with every other such scope on this OS account. If
    // `machine_home == home`, two distinct cases:
    //   (a) `home` IS literally `$HOME/.airc` — already
    //       machine-singular; share.
    //   (b) `home` is an isolated scope (CI temp dir, test root) —
    //       derive the socket from the PROJECT ROOT (`home.parent()`)
    //       so sibling scopes under one account share ONE daemon, but
    //       place it under the home tree so nothing lands under the
    //       user's `~/.airc/runtime` (card f122b5b5).
    // Case (a) must be an EQUALITY check, not "machine_home under
    // user_home": on Windows `%TEMP%` lives under `%USERPROFILE%`, so
    // the prefix form classified temp-rooted isolated scopes as
    // machine-account scopes and resolved the PRODUCTION socket —
    // `ensure_daemon_running`'s build-mismatch path then stopped the
    // real daemon (bite 3 of card b0a81c31; flagged by the #1119
    // sentinel).
    let is_machine_account_scope =
        machine_home != home || user_home.map(|uh| machine_home == uh.join(".airc")) == Some(true);
    let legacy_fallback =
        machine_home.join(format!("daemon-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION));
    if is_machine_account_scope {
        // Hash the user-home into the socket NAME so distinct OS user
        // accounts (different `$HOME` values) get distinct sockets in a
        // shared `runtime_dir`. Real users on the same machine see
        // different home dirs → different sockets → independent daemons,
        // which is what "one daemon per machine account" means. As a
        // side benefit, tests that override `HOME=<tempdir>` for state
        // isolation get unique sockets too, with no test-side changes
        // required — implicit isolation becomes explicit.
        let account_key = user_home
            .map(machine_account_key)
            .unwrap_or_else(|| machine_account_key(machine_home));
        let socket_name = format!(
            "airc-machine-{}-v{}.sock",
            account_key,
            airc_ipc::IPC_PROTOCOL_VERSION
        );
        return machine_socket_path(
            runtime_dir,
            short_fallback_dir,
            &socket_name,
            legacy_fallback,
        );
    }
    // Isolated scope (CI temp, test root outside `$HOME`): the socket
    // derives from the PROJECT ROOT (`home.parent()`) and lives UNDER
    // it — card f122b5b5.
    //
    // Why the project root, not the scope home: sibling scopes under one
    // account (the integration suite's `<tmp>/claude`, `<tmp>/codex`
    // tabs, which share `HOME=<tmp>`) MUST converge on ONE daemon — the
    // "one daemon per machine account" model the lifecycle tests assert.
    // Keying the socket off the full scope home gave each tab its own
    // daemon, so a test's single `airc stop` left the siblings running
    // (the leak the CI zero-leak guard caught on macOS). `home.parent()`
    // is the shared account/project root, so every sibling computes the
    // same path and reaches the same daemon.
    //
    // Why under the home tree, not `runtime_dir`: the PRIOR code keyed
    // the NAME off the project root too, but placed the FILE in
    // `runtime_dir` (= `~/.airc/runtime`) — so every hermetic temp-home
    // test daemon planted its socket (and stale remains:
    // airc-10e8167b5d5b936d-v5.sock, observed live) under the PRODUCTION
    // runtime dir. Placing it under the project root keeps it inside the
    // temp tree (reaped with the tempdir, never touching production).
    let project_root = home.parent().unwrap_or(home);
    let shared = project_root.join(format!("daemon-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION));
    if fits_socket_limit(&shared) {
        return shared;
    }
    // SUN_LEN fallback: a deep tempdir project root overflows
    // sockaddr_un. Use the short OS temp dir with the PROJECT ROOT
    // hashed into the name, so sibling scopes still collide on the same
    // socket (shared daemon preserved) and it still never lands under
    // `~/.airc/runtime`.
    if let Some(short) = short_fallback_dir {
        let candidate = short.join("airc").join(format!(
            "airc-scope-{}-v{}.sock",
            machine_account_key(project_root),
            airc_ipc::IPC_PROTOCOL_VERSION
        ));
        if fits_socket_limit(&candidate) {
            return candidate;
        }
    }
    // Nothing shorter available — return the project-root path and let
    // bind surface a clear SUN_LEN error rather than silently landing
    // in a shared directory.
    shared
}

/// `sockaddr_un.sun_path` is 104 bytes on macOS (incl. NUL) and 108 on
/// Linux. Use the conservative macOS bound so a socket path that fits
/// here binds on both. Card 50d1728b: the machine socket normally lives
/// at `~/.airc/runtime/airc-machine-<hash>.sock`, which is well under
/// this for any real home — but a pathologically deep `$HOME` (the
/// integration suite's `/var/folders/.../T/.tmpXXX` tempdir homes, or a
/// rare deep real home) can overflow it.
const MAX_SOCKET_PATH_LEN: usize = 100;

fn fits_socket_limit(path: &std::path::Path) -> bool {
    path.as_os_str().len() < MAX_SOCKET_PATH_LEN
}

/// Join `socket_name` under `runtime_dir` (the stable `~/.airc/runtime`),
/// but if that overflows `MAX_SOCKET_PATH_LEN`, fall back to the OS
/// per-user temp dir, which is short. The socket NAME already hashes the
/// account, so isolation holds even though the short fallback dir is
/// shared across accounts. This is a SUN_LEN safety net ONLY: every real
/// home stays at `~/.airc/runtime`, so the machine-singular guarantee is
/// unaffected — the fallback exists so deep tempdir homes (tests,
/// containers) bind a working socket instead of failing.
fn machine_socket_path(
    runtime_dir: Option<&std::path::Path>,
    short_fallback_dir: Option<&std::path::Path>,
    socket_name: &str,
    legacy_fallback: PathBuf,
) -> PathBuf {
    let Some(primary) = runtime_dir.map(|dir| dir.join(socket_name)) else {
        return legacy_fallback;
    };
    if fits_socket_limit(&primary) {
        return primary;
    }
    if let Some(short) = short_fallback_dir {
        let candidate = short.join("airc").join(socket_name);
        if fits_socket_limit(&candidate) {
            return candidate;
        }
    }
    // Nothing shorter available — return the primary and let the bind
    // surface a clear SUN_LEN error rather than silently misbehaving.
    primary
}

fn read_user_home_from_env() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from)
}

/// 16-char hex prefix of SHA-256(canonical(path)). Used to namespace
/// the machine-singular socket by the OS user account so distinct
/// users on the same machine + tempdir-isolated tests + parallel CI
/// scopes all get distinct sockets in a shared `runtime_dir`. Same
/// hashing scheme as [`crate::runtime_dir::project_socket_path`] /
/// `discovery::project_key` for consistency.
fn machine_account_key(path: &std::path::Path) -> String {
    use sha2::{Digest, Sha256};
    let canon = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let digest = Sha256::digest(canon.as_os_str().as_encoded_bytes());
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
    fn default_socket_path_is_versioned_by_ipc_protocol() {
        let root = tempfile::TempDir::new().unwrap();
        let home = root.path().join(".airc");
        std::fs::create_dir_all(&home).unwrap();

        let socket = default_socket_path_in(&home);
        let rendered = socket.to_string_lossy();

        assert!(
            rendered.contains(&format!("v{}", airc_ipc::IPC_PROTOCOL_VERSION)),
            "socket endpoint must include IPC protocol version to avoid stale daemon protocol reuse: {rendered}"
        );
    }

    /// Card e51ab14e: every project scope under the same OS user
    /// account resolves to the SAME daemon socket. This is the doctrine
    /// "one daemon per machine account" from `airc-daemon/src/state.rs:36`,
    /// the missing-piece consolidation of PR #1040's per-project
    /// hashing.
    ///
    /// Without this guarantee the cross-scope live-event delivery
    /// proven in `test/public_installed_runtime_proof.sh` fails:
    /// openclaw's daemon never sees continuum's published msg as a
    /// live event because they were two different daemons.
    ///
    /// Test injects synthetic env via [`resolve_socket_path`] rather
    /// than mutating `$HOME` — env mutation under `cargo test`'s
    /// parallel pool races with sibling tests that read it.
    #[test]
    fn project_scopes_under_user_home_share_one_daemon_socket() {
        let user_home = std::path::PathBuf::from("/synthetic/user-home");
        let runtime_dir = std::path::PathBuf::from("/synthetic/runtime");
        let machine_home = user_home.join(".airc");
        let project_a = user_home.join("continuum").join(".airc");
        let project_b = user_home.join("openclaw").join(".airc");

        let socket_a = resolve_socket_path(
            &project_a,
            &machine_home,
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            None,
        );
        let socket_b = resolve_socket_path(
            &project_b,
            &machine_home,
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            None,
        );

        assert_eq!(
            socket_a, socket_b,
            "project scopes under the same OS user account must \
             share one daemon socket per state.rs:36 doctrine; \
             socket_a={socket_a:?} socket_b={socket_b:?}"
        );
        let rendered = socket_a.to_string_lossy();
        assert!(
            rendered.contains("airc-machine-"),
            "machine-account scopes must use the machine-singular \
             socket name 'airc-machine-<account-hash>-v<N>.sock', got: {rendered}"
        );
    }

    /// Card b0a81c31 bite 3 (#1119 sentinel): on Windows, `%TEMP%`
    /// lives UNDER `%USERPROFILE%`. The old clause classified any
    /// `machine_home` under `user_home` as a machine-account scope, so
    /// a temp-rooted `--home` (integration tests spawning the binary
    /// without a HOME override) resolved the PRODUCTION machine socket
    /// — and `ensure_daemon_running`'s build-mismatch path then stopped
    /// the real daemon. Pure path math, so this pins the Windows shape
    /// on every platform. With the b0a81c31 lib fix,
    /// `machine_account_home(temp_scope)` returns the scope itself, and
    /// this function must then treat it as ISOLATED (home-derived
    /// socket), never the machine-account socket.
    #[test]
    fn temp_rooted_scope_under_userprofile_never_resolves_the_machine_socket() {
        let user_home = std::path::PathBuf::from("/synthetic/userprofile");
        let runtime_dir = std::path::PathBuf::from("/synthetic/runtime");
        // Windows shape: the temp dir nests INSIDE the user home.
        let temp_scope = user_home
            .join("AppData/Local/Temp/.tmpAbC123")
            .join("agent");

        let socket = resolve_socket_path(
            &temp_scope,
            // Post-fix lib behavior: temp-rooted scope is its own
            // account boundary.
            &temp_scope,
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            None,
        );

        assert!(
            !socket.to_string_lossy().contains("airc-machine-")
                && !socket.starts_with(user_home.join(".airc")),
            "a temp-rooted scope under the user profile must stay \
             isolated (project-root-derived socket under the temp tree); \
             resolving the machine socket here is how integration tests \
             reached — and stopped — the production daemon (card b0a81c31 \
             bite 3); got {socket:?}"
        );
        assert!(
            socket.starts_with(temp_scope.parent().unwrap()),
            "the socket derives from the project root (home.parent()), \
             under the temp tree; got {socket:?}"
        );
        // And the literal machine home keeps sharing (case (a) of the
        // clause this test tightened — equality, not prefix).
        let machine_home = user_home.join(".airc");
        let machine_socket = resolve_socket_path(
            &machine_home,
            &machine_home,
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            None,
        );
        assert!(
            machine_socket.to_string_lossy().contains("airc-machine-"),
            "literal $HOME/.airc must still resolve the machine-singular \
             socket, got {machine_socket:?}"
        );
    }

    /// Card e51ab14e + f122b5b5: scopes OUTSIDE the OS user account
    /// (CI temp roots, isolated test trees) derive their socket from the
    /// PROJECT ROOT (`home.parent()`), placed under the home tree. Two
    /// scopes under DIFFERENT roots get distinct sockets (parallel runs
    /// don't collide); nothing lands under the user home / runtime dir.
    #[test]
    fn isolated_scopes_outside_user_home_get_project_root_sockets() {
        let user_home = std::path::PathBuf::from("/synthetic/user-home");
        let runtime_dir = std::path::PathBuf::from("/synthetic/runtime");
        // Two isolated scopes under DIFFERENT project roots.
        let scope_a = std::path::PathBuf::from("/synthetic/isolated/a/.airc");
        let scope_b = std::path::PathBuf::from("/synthetic/isolated/b/.airc");

        let socket_a = resolve_socket_path(
            &scope_a,
            &scope_a,
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            None,
        );
        let socket_b = resolve_socket_path(
            &scope_b,
            &scope_b,
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            None,
        );

        assert_ne!(
            socket_a, socket_b,
            "isolated scopes under different project roots must keep \
             distinct sockets so parallel test runs don't collide; \
             got the same socket for two unrelated roots: {socket_a:?}"
        );
        assert!(
            socket_a.starts_with("/synthetic/isolated/a") && !socket_a.starts_with(&runtime_dir),
            "project-root-derived under the home tree, not runtime_dir: {socket_a:?}"
        );
        assert!(
            socket_b.starts_with("/synthetic/isolated/b") && !socket_b.starts_with(&runtime_dir),
            "project-root-derived under the home tree, not runtime_dir: {socket_b:?}"
        );
    }

    /// Card f122b5b5 REGRESSION PIN — sibling scopes under ONE account
    /// share ONE daemon socket. The integration suite runs many tabs
    /// (`<tmp>/claude`, `<tmp>/codex`) under a shared `HOME=<tmp>`; they
    /// must converge on one daemon (the "one daemon per account" model
    /// the lifecycle tests assert and `airc stop` relies on). Keying the
    /// isolated socket off the full scope home gave each tab its own
    /// daemon, so a single `airc stop` left siblings leaked — caught by
    /// the macOS zero-leak guard. Mutation: key the isolated socket off
    /// `home` instead of `home.parent()` → these diverge → this fails.
    #[test]
    fn sibling_isolated_scopes_under_one_account_share_a_socket() {
        let user_home = std::path::PathBuf::from("/synthetic/user-home");
        // A temp account root with two tabs — the daemon_lifecycle shape.
        let account = std::path::PathBuf::from("/var/folders/zz/T/.tmpACCT");
        let claude = account.join("claude");
        let codex = account.join("codex");

        let claude_sock = resolve_socket_path(
            &claude,
            &claude, // temp-rooted ⇒ isolated boundary
            Some(user_home.as_path()),
            None,
            Some(std::path::Path::new("/tmp")),
        );
        let codex_sock = resolve_socket_path(
            &codex,
            &codex,
            Some(user_home.as_path()),
            None,
            Some(std::path::Path::new("/tmp")),
        );

        assert_eq!(
            claude_sock, codex_sock,
            "sibling tabs under one account root must share ONE daemon \
             socket; got claude={claude_sock:?} codex={codex_sock:?}"
        );
    }

    /// Card f122b5b5 PIN — the production-pollution bug. An EXPLICIT
    /// temp home (a hermetic test daemon's `--home` under the OS temp
    /// dir, with the operator's REAL user home + runtime dir in env)
    /// must NEVER resolve a socket under the user's `~/.airc/runtime`.
    /// The pre-fix code keyed the socket NAME by project-root hash but
    /// placed the FILE in `runtime_dir`, which is how the stale
    /// `airc-10e8167b5d5b936d-v5.sock` landed in the production
    /// `~/.airc/runtime` (observed live, card body). Mutation: revert
    /// the isolated branch to the runtime-dir placement → this fails.
    #[test]
    fn explicit_temp_home_never_places_socket_under_user_runtime_dir() {
        let user_home = std::path::PathBuf::from("/Users/operator");
        let runtime_dir = user_home.join(".airc").join("runtime");
        // The literal shape of a leaked daemon's home: tempfile tempdir
        // under macOS's /var/folders, no HOME override.
        let temp_home = std::path::PathBuf::from("/var/folders/8d/x/T/.tmpQwErTy/agent");

        let socket = resolve_socket_path(
            &temp_home,
            &temp_home, // machine_account_home(temp) == temp (isolated)
            Some(user_home.as_path()),
            Some(runtime_dir.as_path()),
            Some(std::path::Path::new("/tmp")),
        );

        assert!(
            !socket.starts_with(&runtime_dir) && !socket.starts_with(&user_home),
            "an explicit temp home must NEVER place its socket under the \
             user home / production runtime dir (card f122b5b5), got {socket:?}"
        );
        assert!(
            socket.starts_with("/var/folders") || socket.starts_with("/tmp"),
            "the socket must derive from the project root under the temp \
             tree (or the short temp fallback hashed from it), got {socket:?}"
        );
        assert!(
            socket.as_os_str().len() < MAX_SOCKET_PATH_LEN,
            "and it must still fit sockaddr_un: {socket:?}"
        );
    }

    /// Card e51ab14e: when `runtime_dir` resolution fails (`$HOME`
    /// unset and no `$AIRC_RUNTIME_DIR` override, so `~/.airc/runtime`
    /// can't be built), the machine-account path falls through to the
    /// legacy home-private `<machine_home>/daemon-v<N>.sock` so the
    /// substrate stays functional even in degraded environments.
    #[test]
    fn machine_account_scope_falls_back_to_home_private_when_runtime_dir_unavailable() {
        let user_home = std::path::PathBuf::from("/synthetic/user-home");
        let machine_home = user_home.join(".airc");
        let project = user_home.join("continuum").join(".airc");

        let socket = resolve_socket_path(
            &project,
            &machine_home,
            Some(user_home.as_path()),
            None,
            None,
        );

        let expected =
            machine_home.join(format!("daemon-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION));
        assert_eq!(socket, expected);
    }

    /// Card 50d1728b SUN_LEN guard: a deep `$HOME` (the integration
    /// suite's `/var/folders/.../T/.tmpXXX` tempdir homes) would push
    /// `~/.airc/runtime/airc-machine-<hash>.sock` past sockaddr_un's
    /// 104-byte limit. The machine socket must then fall back to the
    /// short OS temp dir — NOT fail to bind, NOT silently use the
    /// over-long path. Normal homes are unaffected (covered by the live
    /// daemon_lifecycle integration test, which binds at ~/.airc/runtime).
    #[test]
    fn machine_socket_falls_back_to_short_dir_when_runtime_path_too_long() {
        // A realistically deep macOS tempdir home — what `HOME=<TempDir>`
        // resolves to under `cargo test`.
        let user_home =
            std::path::PathBuf::from("/var/folders/8d/778wjbv96mq1760tv6gk374m0000gn/T/.tmpXY12ab");
        let machine_home = user_home.join(".airc");
        let scope = user_home.join("scope").join(".airc");
        let deep_runtime = user_home.join(".airc").join("runtime");
        let short_fallback = std::path::PathBuf::from("/tmp");

        let socket = resolve_socket_path(
            &scope,
            &machine_home,
            Some(user_home.as_path()),
            Some(deep_runtime.as_path()),
            Some(short_fallback.as_path()),
        );

        assert!(
            socket.as_os_str().len() < MAX_SOCKET_PATH_LEN,
            "resolved socket must fit sockaddr_un: {} ({} bytes)",
            socket.display(),
            socket.as_os_str().len()
        );
        assert!(
            socket.starts_with("/tmp/airc"),
            "deep home must fall back to the short dir, got {}",
            socket.display()
        );
        // Isolation preserved: the account-hashed name still rides along.
        assert!(
            socket
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("airc-machine-")),
            "socket name keeps the account hash: {}",
            socket.display()
        );
    }
}
