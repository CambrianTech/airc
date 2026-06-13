//! Command-line interface definitions (clap derive).
//!
//! All commands default to the persisted state at `<home>` (default
//! the current git project's `.airc`), which contains:
//!   - `identity.key`   — 32-byte Ed25519 secret (0600 on Unix)
//!   - daemon IPC endpoint, derived from scope + IPC protocol version
//!   - `events.sqlite`  — ORM-backed identity metadata, events, cursors, peer
//!     trust, subscriptions, and coordinator state
//!
//! The `--home` flag overrides for testing / multi-identity setups.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand, ValueEnum};

use airc_lib::PeerSpec;

use crate::channel_gist_cli::ChannelGistArgs;
use crate::collaboration_cli::CollaborationArgs;
use crate::envelope_cli::EnvelopeArgs;
use crate::gh_cli::GhArgs;
use crate::gist_cli::GistArgs;
use crate::handshake_cli::HandshakeArgs;
use crate::hygiene_cli::HygieneArgs;
use crate::identity_cli::IdentityArgs;
use crate::integrations::codex::{CodexHookArgs, CodexStartArgs};
use crate::knock_cli::KnockArgs;
use crate::pending_cli::PendingArgs;
use crate::route_cli::RouteArgs;
use crate::transport_cli::TransportArgs;
use crate::work_cli::WorkArgs;

/// Default home directory for persisted identity + IPC state.
///
/// Resolution order:
///   1. `$AIRC_HOME` → explicit scope override.
///   2. First `.airc` ancestor when cwd is inside a scope.
///   3. Git project root `.airc` when cwd is inside a worktree.
///   4. `./.airc` in the current working dir.
///
/// Account-wide state still lives under the canonical machine account
/// home (`$HOME/.airc`) inside `airc-lib`; this default is the
/// consumer/project scope. That preserves the original public contract:
/// running `airc join` in a repo uses that repo's `.airc`.
pub fn default_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("AIRC_HOME") {
        return PathBuf::from(home);
    }

    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    default_home_dir_for(&cwd)
}

fn default_home_dir_for(cwd: &Path) -> PathBuf {
    let machine_account = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| PathBuf::from(h).join(".airc"));
    default_home_dir_for_with(cwd, machine_account.as_deref(), &git_main_working_tree)
}

/// Card a1b4552a — pure-function half of the resolution so tests can
/// drive it with synthetic paths instead of needing a real `$HOME`
/// and git repo. `machine_account_home` is the path we MUST NOT
/// resolve to (typically `$HOME/.airc`); `git_main_working_tree_fn`
/// resolves the main working tree of a git checkout from any of its
/// worktrees (production: shells `git rev-parse --git-common-dir`).
fn default_home_dir_for_with(
    cwd: &Path,
    machine_account_home: Option<&Path>,
    git_main_working_tree_fn: &dyn Fn(&Path) -> Option<PathBuf>,
) -> PathBuf {
    for ancestor in cwd.ancestors() {
        let matches_dotairc = ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".airc");
        if !matches_dotairc {
            continue;
        }
        // Card a1b4552a: SKIP the machine-account home itself when
        // walking ancestors. `~/.airc` is a SYSTEM-LEVEL airc state
        // directory holding the singleton machine-account identity +
        // the daemon socket — not a per-project agent scope. Agents
        // who `cd ~/.airc/worktrees/<short>/` (the d1b2798d auto-spawn
        // workflow) would otherwise resolve home here and silently
        // borrow the machine-account's identity, attributing their
        // actions to whoever owns the singleton row. Reproducibly
        // observed today: peer 9bb24964 claiming cards from a
        // spawned worktree → board shows the claims under peer
        // cdff6a9d (machine-account-owner) instead.
        if Some(ancestor) == machine_account_home {
            continue;
        }
        return ancestor.to_path_buf();
    }
    // Card a1b4552a: when cwd is inside a git worktree spawned by
    // `airc work claim` (worktrees live under `~/.airc/worktrees/`),
    // the canonical project scope is the MAIN repo's `.airc`, not
    // the worktree's own (worktrees rarely contain their own `.airc`).
    // `git rev-parse --git-common-dir` points at the MAIN repo's
    // `.git/` for any worktree; its parent is the main working tree.
    git_main_working_tree_fn(cwd)
        .map(|root| root.join(".airc"))
        .or_else(|| git_toplevel(cwd).map(|root| root.join(".airc")))
        .unwrap_or_else(|| cwd.join(".airc"))
}

fn git_main_working_tree(cwd: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let common_dir = text.trim();
    if common_dir.is_empty() {
        return None;
    }
    let common_path = PathBuf::from(common_dir);
    let abs = if common_path.is_absolute() {
        common_path
    } else {
        cwd.join(common_path).canonicalize().ok()?
    };
    // common dir is `<main-working-tree>/.git`; parent is the main
    // working tree.
    abs.parent().map(|p| p.to_path_buf())
}

fn git_toplevel(cwd: &Path) -> Option<PathBuf> {
    let output = std::process::Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let root = text.trim();
    if root.is_empty() {
        None
    } else {
        Some(PathBuf::from(root))
    }
}

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
    let machine_home = airc_lib::machine_account_home(home);
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
/// - `machine_home`: result of [`airc_lib::machine_account_home`]
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

/// AIRC substrate CLI.
#[derive(Debug, Parser)]
#[command(
    name = "airc",
    version,
    about = "AIRC substrate CLI",
    long_about = "Cross-process / cross-machine AI chat over the airc substrate. \
                  Provides the public AIRC command surface."
)]
pub struct Cli {
    /// State directory for persisted identity + IPC socket. Defaults
    /// to the current git project root's `.airc` unless `$AIRC_HOME`
    /// is set. Override for tests or multi-identity setups.
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    /// Ad-hoc peers to enrol for this invocation only, repeatable.
    /// Format: `<uuid>:<base64-pubkey-no-padding>`. Persistent peers
    /// come from the peer trust store (managed via `airc peer add`);
    /// this flag unions on top for one-shot use.
    #[arg(long = "peer", value_name = "SPEC", global = true)]
    pub peers: Vec<PeerSpec>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or load the persisted identity (`<home>/identity.key`
    /// plus ORM-backed metadata), then print this peer's spec for
    /// out-of-band sharing. Idempotent — repeat runs return the same
    /// peer_id.
    Init {
        /// Local agent identity name. Same effect as `AIRC_AGENT_NAME`,
        /// but explicit CLI input takes precedence.
        #[arg(long = "as", value_name = "AGENT_NAME")]
        agent_name: Option<String>,
    },

    /// Print the primary non-loopback LAN IPv4 address, if detectable.
    LanIp,

    /// Inspect collaboration health during Rust cutover.
    Collaboration(CollaborationArgs),

    /// Resolve channel-to-gist discovery state during Rust cutover.
    ChannelGist(ChannelGistArgs),

    /// Identity and whois helpers during Rust cutover.
    Identity(IdentityArgs),

    /// Legacy envelope encryption helpers during Rust cutover.
    Envelope(EnvelopeArgs),

    /// Send a single text Message frame to the default subscribed
    /// room and exit. The default channel lives in the ORM store.
    Send {
        /// Message body.
        text: String,
    },

    /// Subscribe to the current room and print frames until
    /// interrupted (Ctrl-C).
    Listen {
        /// Replay from the start of the wire instead of live-only.
        #[arg(long)]
        replay: bool,
    },

    /// Same-LAN secure send: dial a peer over TLS, send a single
    /// text frame to the current room's channel, and wait for the
    /// receiver's typed delivery ack (card 39d37629). Exit 0 only on
    /// `delivered`; undeliverable or no-ack outcomes exit nonzero.
    LanSend {
        /// Address of the listening peer (e.g. `127.0.0.1:7474`).
        #[arg(long)]
        to: SocketAddr,
        /// UUID of the listening peer (for cert pinning).
        #[arg(long)]
        expected_peer: String,
        /// How long to wait for the receiver's delivery ack before
        /// reporting no-ack (older receivers never ack).
        #[arg(long, default_value_t = 10_000)]
        ack_timeout_ms: u64,
        /// Message body.
        text: String,
    },

    /// Same-LAN secure listen: bind a TLS server, accept peers,
    /// print received frames.
    LanListen {
        /// Bind address (e.g. `127.0.0.1:7474` or `0.0.0.0:7474`).
        #[arg(long)]
        bind: SocketAddr,
        /// Replay-mode subscription (defaults to live-only).
        #[arg(long)]
        replay: bool,
    },

    /// Start the daemon in the foreground. Holds substrate state so
    /// subsequent short-lived CLI calls (`ping`, `msg`, `status`)
    /// don't re-load identity or re-handshake.
    Daemon {
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Probe the daemon — returns immediately if alive.
    Ping {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Daemon health snapshot.
    Status {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Ask the daemon to shut down gracefully.
    Stop {
        #[arg(long)]
        socket: Option<PathBuf>,
    },

    /// Send a text message to the current room via the running
    /// daemon (fast — no per-call substrate setup).
    Msg {
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Message body.
        text: String,
    },

    /// Publish a structured frame and emit a JSON receipt on
    /// stdout. Designed for consumers (Continuum chat, OpenClaw,
    /// bridge processes) that need typed event id + lamport +
    /// channel without human-prose parsing, and to route to a
    /// non-default room without mutating this scope's default
    /// pointer.
    Publish {
        /// Channel name to route to. Must already be subscribed
        /// (publish does not auto-join). Defaults to the current
        /// room when omitted.
        #[arg(long)]
        room: Option<String>,
        /// Inline UTF-8 body. Mutually exclusive with
        /// `--body-json`.
        #[arg(long, conflicts_with = "body_json", group = "body")]
        body_text: Option<String>,
        /// Path to a UTF-8 JSON file whose contents become the
        /// frame body. Pass `-` to read from stdin.
        #[arg(long, group = "body")]
        body_json: Option<String>,
        /// Header in `key=value` form. Repeatable.
        #[arg(long = "header", value_name = "KEY=VALUE")]
        headers: Vec<String>,
        /// Frame kind. Defaults to `event` for structured payloads.
        #[arg(long, value_enum, default_value = "event")]
        kind: PublishFrameKind,
    },

    /// Pull buffered frames from the daemon's inbox for the current
    /// room's wire.
    Inbox {
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Cursor lamport — pair with `--since-event-id`. The cursor
        /// is `(lamport, event_id)`; both halves required when paging
        /// from a specific point.
        #[arg(long, requires = "since_event_id")]
        since_lamport: Option<u64>,
        /// Cursor event_id (UUID) — pair with `--since-lamport`.
        #[arg(long, requires = "since_lamport")]
        since_event_id: Option<String>,
        #[arg(long)]
        limit: Option<usize>,
        /// Emit a single JSON document on stdout instead of
        /// human-readable text. Shape mirrors `airc events list
        /// --json` and `airc publish` for machine consumers
        /// (continuum's CliAircRealtimeStore, shell scripts, CI
        /// smoke tests).
        #[arg(long)]
        json: bool,
    },

    /// Print or switch the current room. With no name, prints the
    /// current room's name + wire + channel. With a name, derives a
    /// deterministic `(wire, channel)` from the name and sets it as
    /// the current room — two peers who run `airc room project-x`
    /// land in the same channel without sharing the UUID.
    Room {
        /// Room name. Omit to just print the current room.
        name: Option<String>,
    },

    /// Publish the room's operating doctrine (card 2903a8ef slice 2/4).
    /// Reads a markdown file and emits a `RoomDoctrinePublished`
    /// substrate event so every attaching agent loads the latest
    /// doctrine on join. Default file is `AGENTS.md` at the git repo
    /// root; pass `--from-file` to override.
    DoctrinePublish {
        /// Path to the markdown file. Defaults to `AGENTS.md` at the
        /// git repo root if omitted.
        #[arg(long)]
        from_file: Option<std::path::PathBuf>,
    },

    /// Leave a subscribed room without deleting identity or trust.
    /// With no room, leaves the current default channel.
    Part {
        /// Optional channel name to leave.
        room: Option<String>,
    },

    /// Manage the persisted peer trust registry.
    Peer(PeerArgs),

    /// List enrolled peers in the current scope.
    ///
    /// IRC-shaped public command. Equivalent to `airc peer list`,
    /// kept as the low-friction human / agent coordination surface.
    Peers,

    /// Show identity information for self or an enrolled peer.
    ///
    /// With no target, prints this scope's identity card. With a peer
    /// id or prefix, prints the enrolled trust entry for that peer.
    Whois {
        /// Optional peer UUID or unambiguous UUID prefix.
        peer: Option<String>,
    },

    /// Inspect transport route policy and candidate selection.
    Route(RouteArgs),

    /// Account-mesh registry: same-account cross-machine discovery.
    ///
    /// The daemon runs publish/refresh on a cadence automatically; this
    /// verb is the manual proof + Mac-bootstrap surface — one
    /// publish+refresh against the gh-gist rendezvous, printing what was
    /// published and who was enrolled.
    Registry(crate::registry_cli::RegistryArgs),

    /// Inspect transport health and substrate connectivity.
    Transport(TransportArgs),

    /// Inspect persisted events through subscription-style filters.
    Events(crate::events_cli::EventsArgs),

    /// Parse legacy GitHub gist envelope JSON.
    Gist(GistArgs),

    /// Join the account mesh. With no room, subscribes to #general
    /// and the inferred repo/org channel. With a room, subscribes to
    /// that channel and makes it the default.
    ///
    /// Sets up the account mesh and, in interactive/agent runtimes,
    /// streams live events from ALL subscribed channels to stdout
    /// until interrupted. Scripts/tests return after setup; there is
    /// no separate public "attach" mode.
    Join {
        /// Optional channel name to join.
        room: Option<String>,
    },

    /// Print the installed `airc` build metadata: short commit, branch,
    /// commit subject, and install dir. Use this to verify two scopes
    /// are on the same build. (`--version` flag prints just the
    /// package version.)
    Version,

    /// Fast-forward the installed source checkout and refresh the
    /// installed `airc` binary + skills from that source.
    #[command(visible_aliases = ["upgrade", "pull"])]
    Update,

    /// Self-diagnose the airc install + scope state.
    ///
    /// Walks the install/identity/daemon/route checklist that
    /// `skills/doctor/SKILL.md` documents agents calling. Default
    /// mode is the env probe (fast, local). `--health` adds live
    /// route/process state. `--fix` applies only safe auto-recovery
    /// for detected issues (currently stale daemon sockets).
    Doctor {
        /// After diagnosing, apply safe auto-recovery. Identity
        /// partial states are reported with manual fix commands;
        /// doctor does not wipe identity/trust state automatically.
        /// Without `--fix`, doctor only reports.
        #[arg(long)]
        fix: bool,

        /// Include live route/process health (calls into the
        /// route resolver + daemon status).
        #[arg(long)]
        health: bool,
    },

    /// Shared GitHub request governor.
    Gh(GhArgs),

    /// TCP pairing handshake during Rust cutover.
    Handshake(HandshakeArgs),

    /// Workspace/resource hygiene policy.
    Hygiene(HygieneArgs),

    /// Knock/approve crypto helpers during Rust cutover.
    Knock(KnockArgs),

    /// Pending-queue routing helpers during Rust cutover.
    Pending(PendingArgs),

    /// Codex lifecycle hook adapters backed by Rust AIRC events.
    CodexHook(CodexHookArgs),

    /// Launch the runtime feed owner for Codex integration.
    CodexStart(CodexStartArgs),

    /// Coordinate work cards over the current room's AIRC substrate.
    Work(WorkArgs),

    /// Coordinate work lanes over the current room's AIRC substrate.
    Lane(crate::lane_cli::LaneArgs),

    /// Manage local git worktree lane registry.
    WorktreeLane(crate::worktree_lane_cli::WorktreeLaneArgs),

    /// Queue-card parsing and mutation primitives during Rust cutover.
    QueueCard(crate::queue_card_cli::QueueCardArgs),

    /// Format monitor events for AI/runtime consumers.
    Monitor(crate::monitor::MonitorArgs),

    /// Coordinate workspace leases over the current room's AIRC substrate.
    Workspace(crate::workspace_cli::WorkspaceArgs),

    /// Print the stable mnemonic for a hex digest.
    Humanhash {
        /// Hex input to convert into a mnemonic.
        hex_input: String,
        /// Number of words to emit.
        #[arg(long, default_value_t = 4)]
        words: usize,
    },

    /// Print this runtime process's client id, if one can be derived.
    ClientId,

    /// Generate a UUIDv4.
    UuidV4,

    /// Convert a canonical UTC timestamp to Unix epoch seconds.
    IsoToEpoch {
        /// Timestamp in `YYYY-MM-DDTHH:MM:SSZ` form.
        timestamp: String,
    },
}

#[cfg(test)]
mod tests {
    use super::{default_home_dir_for, default_socket_path_in};

    #[test]
    fn default_home_uses_enclosing_airc_scope() {
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();

        assert_eq!(default_home_dir_for(&nested), scope);
    }

    #[test]
    fn default_home_uses_git_project_root_scope() {
        let root = tempfile::TempDir::new().unwrap();
        let repo = root.path().join("repo");
        let nested = repo.join("src").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(&repo)
            .status()
            .unwrap();
        assert!(status.success());

        let actual = default_home_dir_for(&nested);
        let expected = repo.join(".airc");
        std::fs::create_dir_all(&actual).unwrap();
        assert_eq!(
            actual.canonicalize().unwrap(),
            expected.canonicalize().unwrap()
        );
    }

    #[test]
    fn default_home_skips_machine_account_home_when_inside_worktrees_subdir() {
        // Card a1b4552a — the leak we caught live today. Without this
        // guard an agent who `cd ~/.airc/worktrees/<short>/` per the
        // d1b2798d auto-spawn workflow would resolve home to ~/.airc
        // and borrow the machine-account identity. Test pins that
        // ancestor walk SKIPS the machine-account ~/.airc and falls
        // through to git_main_working_tree (the main repo's .airc).
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let machine_account = PathBuf::from("/Users/test/.airc");
        let cwd = machine_account.join("worktrees").join("abc12345");
        // Stub git_main_working_tree_fn to a known main repo working tree.
        let main_repo = PathBuf::from("/Users/test/Development/airc");
        let stub = |_: &Path| -> Option<PathBuf> { Some(main_repo.clone()) };
        let resolved = default_home_dir_for_with(&cwd, Some(machine_account.as_path()), &stub);
        assert_eq!(
            resolved,
            main_repo.join(".airc"),
            "must NOT resolve to machine-account ~/.airc when cwd is under its worktrees dir",
        );
    }

    #[test]
    fn default_home_still_resolves_project_scope_when_not_under_machine_account() {
        // Sanity: a nested cwd that has an enclosing project .airc that
        // is NOT the machine-account home resolves correctly (regression
        // guard — we don't want the fix above to skip legitimate
        // project scopes too).
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let machine_account = PathBuf::from("/Users/test/.airc");
        let root = tempfile::TempDir::new().unwrap();
        let scope = root.path().join(".airc");
        let nested = scope.join("debug");
        std::fs::create_dir_all(&nested).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&nested, Some(machine_account.as_path()), &stub);
        assert_eq!(resolved, scope);
    }

    #[test]
    fn default_home_falls_through_to_cwd_airc_when_nothing_else_resolves() {
        // Final fallback: no ancestor .airc, no git context — cwd's
        // own .airc is the answer. Pre-existing behaviour, pinned so
        // the refactor preserves it.
        use super::default_home_dir_for_with;
        use std::path::{Path, PathBuf};
        let root = tempfile::TempDir::new().unwrap();
        let cwd = root.path().join("standalone");
        std::fs::create_dir_all(&cwd).unwrap();
        let stub = |_: &Path| -> Option<PathBuf> { None };
        let resolved = default_home_dir_for_with(&cwd, None, &stub);
        assert_eq!(resolved, cwd.join(".airc"));
    }

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
        use super::resolve_socket_path;
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
        use super::resolve_socket_path;
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
        use super::resolve_socket_path;
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
        use super::resolve_socket_path;
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
        use super::resolve_socket_path;
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
            socket.as_os_str().len() < super::MAX_SOCKET_PATH_LEN,
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
        use super::resolve_socket_path;
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
        use super::resolve_socket_path;
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
            socket.as_os_str().len() < super::MAX_SOCKET_PATH_LEN,
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

#[derive(Debug, Args)]
pub struct PeerArgs {
    #[command(subcommand)]
    pub action: PeerAction,
}

/// clap value_parser shim for `--endpoint` — clap wants a
/// `fn(&str) -> Result<T, E>` and the typed parser lives with the
/// enum in airc-lib.
fn parse_cli_route_endpoint(input: &str) -> Result<airc_lib::RouteEndpoint, String> {
    airc_lib::RouteEndpoint::parse_cli(input)
}

#[derive(Debug, Subcommand)]
pub enum PeerAction {
    /// Enrol a peer by spec. If a daemon is running on
    /// the scope's default IPC endpoint, also tells it via RPC so the
    /// in-memory registry stays in sync — no daemon restart required.
    Add {
        /// Peer spec: `<uuid>:<base64-pubkey-no-padding>` (the
        /// `peer_spec:` line from the other side's `airc init`).
        spec: PeerSpec,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Card 34942ec1 Sub-C: enrol at this trust tier instead of
        /// the substrate-default Untrusted. Used to manually pin
        /// Friend / OwnAccount / OwnMachine when the operator knows
        /// the relationship out-of-band (Joel pinning Friend on
        /// Toby's airc, OwnAccount on his other machine, etc.).
        #[arg(long, value_enum)]
        tier: Option<CliTrustTier>,
        /// Card 625abe6d slice 1 (DEV verb — production endpoints
        /// arrive via the account registry / mDNS): advertise where
        /// this peer can be dialed. Repeatable; stored order = dial
        /// cost order. Forms: `lan-tcp:HOST:PORT`,
        /// `tailscale-tcp:HOST:PORT`, `udp:HOST:PORT`, `relay:URL`.
        /// Route discovery (`airc transport health`, daemon refresh)
        /// dials these outbound — the peer never needs an inbound
        /// rule on OUR side.
        #[arg(long = "endpoint", value_parser = parse_cli_route_endpoint)]
        endpoints: Vec<airc_lib::RouteEndpoint>,
    },
    /// Remove a peer from local trust.
    Remove {
        /// Peer UUID to remove from the trust store.
        peer_id: String,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// Card 34942ec1 Sub-C: update the trust tier of an
    /// already-enrolled peer without rotating the key. Pubkey-
    /// rotation has its own path (`peer add` with a re-pair flow);
    /// this is the orthogonal tier-update.
    ///
    /// Refuses for unknown peers (no implicit add). Idempotent for
    /// no-op transitions.
    SetTier {
        /// Peer UUID to re-tier.
        peer_id: String,
        /// New trust tier.
        #[arg(value_enum)]
        tier: CliTrustTier,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List enrolled peers from the peer trust store.
    List {
        /// Print as JSON ({peer_id, pubkey_b64, tier, added_at_ms}
        /// per row). Consumers (continuum bridge, hermes router)
        /// read this to build their grid routing tables — see card
        /// 34942ec1 Sub-C V4.
        #[arg(long)]
        json: bool,
    },
}

/// CLI mirror of `airc_store::TrustTier`. Kept distinct so clap's
/// value_enum machinery can derive the snake-case rename without
/// pulling clap into the storage layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "snake_case")]
pub enum CliTrustTier {
    OwnMachine,
    OwnAccount,
    Friend,
    Untrusted,
}

impl From<CliTrustTier> for airc_store::TrustTier {
    fn from(value: CliTrustTier) -> Self {
        match value {
            CliTrustTier::OwnMachine => airc_store::TrustTier::OwnMachine,
            CliTrustTier::OwnAccount => airc_store::TrustTier::OwnAccount,
            CliTrustTier::Friend => airc_store::TrustTier::Friend,
            CliTrustTier::Untrusted => airc_store::TrustTier::Untrusted,
        }
    }
}

/// Frame kind selector for `airc publish`. Maps 1:1 onto
/// `airc_protocol::FrameKind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
pub enum PublishFrameKind {
    /// Plain message frame (human-readable chat).
    Message,
    /// Structured event frame (recommended for typed envelopes
    /// like Continuum's `AircRealtimeEnvelope`).
    Event,
    /// Control-plane signalling.
    Control,
}
