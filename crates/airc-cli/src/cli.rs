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
///     `<runtime-dir>/airc-<project-hash>-v<N>.sock` so parallel
///     isolated runs do not collide on the same daemon socket.
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
/// `runtime-dir` resolves via [`runtime_dir::runtime_dir`]:
///   `$AIRC_RUNTIME_DIR` → `$XDG_RUNTIME_DIR/airc` → `$TMPDIR/airc`
///   (macOS only) → `~/.airc/runtime`. All four are namespaced under
///   the user (no shared-machine collisions) and reachable across
///   sandbox boundaries in the common case.
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
    // `machine_account_home(scope_home)` returns `$HOME/.airc` when
    // scope_home is under `$HOME`, otherwise returns scope_home
    // unchanged. So if `machine_home != home` here, the scope IS
    // a project scope under `$HOME` — share the machine-singular
    // socket with every other such scope on this OS account. If
    // `machine_home == home`, two distinct cases:
    //   (a) `home` IS literally `$HOME/.airc` — already
    //       machine-singular; share.
    //   (b) `home` is outside `$HOME` (CI temp dir, test root) —
    //       isolated; use the per-project socket so parallel
    //       isolated scopes don't collide.
    let is_machine_account_scope =
        machine_home != home || is_under_user_home(&machine_home);
    if is_machine_account_scope {
        let socket_name = format!("airc-machine-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION);
        return match crate::runtime_dir::runtime_dir() {
            Ok(dir) => dir.join(socket_name),
            Err(_) => {
                machine_home.join(format!("daemon-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION))
            }
        };
    }
    // Isolated scope (CI temp, test root outside `$HOME`): per-project
    // socket so multiple isolated tests can run in parallel without
    // colliding on the same daemon.
    let project_root = home.parent().unwrap_or(home);
    match crate::runtime_dir::project_socket_path(project_root) {
        Ok(path) => path,
        Err(_) => machine_home.join(format!("daemon-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION)),
    }
}

/// Returns true if `path` is under the OS user-home directory
/// (`$HOME` on POSIX, `$USERPROFILE` on Windows). Used by
/// [`default_socket_path_in`] to distinguish "this scope is on the
/// real user's account → share the machine-singular daemon" from
/// "this scope is in a CI temp dir / isolated test root → its own
/// daemon."
fn is_under_user_home(path: &std::path::Path) -> bool {
    let user_home = std::env::var_os("HOME")
        .or_else(|| {
            if cfg!(windows) {
                std::env::var_os("USERPROFILE")
            } else {
                None
            }
        })
        .map(PathBuf::from);
    let Some(user_home) = user_home else {
        return false;
    };
    let normalized_home = user_home.canonicalize().unwrap_or(user_home);
    let normalized_path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    normalized_path.starts_with(&normalized_home)
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

    /// Same-LAN secure send: dial a peer over TLS and send a single
    /// text frame to the current room's channel.
    LanSend {
        /// Address of the listening peer (e.g. `127.0.0.1:7474`).
        #[arg(long)]
        to: SocketAddr,
        /// UUID of the listening peer (for cert pinning).
        #[arg(long)]
        expected_peer: String,
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
    #[test]
    fn project_scopes_under_user_home_share_one_daemon_socket() {
        let user_home = tempfile::TempDir::new().unwrap();
        // Override $HOME so `machine_account_home` + `is_under_user_home`
        // treat user_home as the OS user account boundary.
        let original_home = std::env::var_os("HOME");
        // Also set AIRC_RUNTIME_DIR so `runtime_dir()` is deterministic
        // and the two scopes are guaranteed to resolve to the same dir
        // regardless of CI's XDG_RUNTIME_DIR / TMPDIR state.
        let original_airc_runtime_dir = std::env::var_os("AIRC_RUNTIME_DIR");
        let runtime_dir = user_home.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        // SAFETY: tests touching env vars must serialize; we accept
        // the existing tests' pattern. Restored on the way out.
        unsafe {
            std::env::set_var("HOME", user_home.path());
            std::env::set_var("AIRC_RUNTIME_DIR", &runtime_dir);
        }

        let project_a = user_home.path().join("continuum").join(".airc");
        let project_b = user_home.path().join("openclaw").join(".airc");
        std::fs::create_dir_all(&project_a).unwrap();
        std::fs::create_dir_all(&project_b).unwrap();

        let socket_a = default_socket_path_in(&project_a);
        let socket_b = default_socket_path_in(&project_b);

        // Restore env BEFORE assertions so a panic doesn't leak the
        // override into sibling tests.
        unsafe {
            if let Some(h) = original_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(r) = original_airc_runtime_dir {
                std::env::set_var("AIRC_RUNTIME_DIR", r);
            } else {
                std::env::remove_var("AIRC_RUNTIME_DIR");
            }
        }

        assert_eq!(
            socket_a, socket_b,
            "project scopes under the same OS user account must \
             share one daemon socket per state.rs:36 doctrine; \
             socket_a={socket_a:?} socket_b={socket_b:?}"
        );
        // And it must be the machine-singular name, not a
        // project-hashed name.
        let rendered = socket_a.to_string_lossy();
        assert!(
            rendered.contains("airc-machine-v"),
            "machine-account scopes must use the machine-singular \
             socket name 'airc-machine-v<N>.sock', got: {rendered}"
        );
    }

    /// Card e51ab14e: scopes OUTSIDE the OS user account (CI temp
    /// roots, isolated test trees) keep their per-project socket so
    /// parallel test runs do not collide on the same daemon.
    /// This preserves PR #1040's original guarantee for the case
    /// it was solving for.
    #[test]
    fn isolated_scopes_outside_user_home_keep_per_project_sockets() {
        // Force user_home to a path that doesn't contain either scope
        // we're about to create. Then both scopes live OUTSIDE
        // `$HOME` and are treated as isolated.
        let elsewhere = tempfile::TempDir::new().unwrap();
        let original_home = std::env::var_os("HOME");
        let original_airc_runtime_dir = std::env::var_os("AIRC_RUNTIME_DIR");
        let runtime_dir = elsewhere.path().join("runtime");
        std::fs::create_dir_all(&runtime_dir).unwrap();
        unsafe {
            std::env::set_var("HOME", elsewhere.path());
            std::env::set_var("AIRC_RUNTIME_DIR", &runtime_dir);
        }

        let isolated_a = tempfile::TempDir::new().unwrap();
        let isolated_b = tempfile::TempDir::new().unwrap();
        let scope_a = isolated_a.path().join("project_a").join(".airc");
        let scope_b = isolated_b.path().join("project_b").join(".airc");
        std::fs::create_dir_all(&scope_a).unwrap();
        std::fs::create_dir_all(&scope_b).unwrap();

        let socket_a = default_socket_path_in(&scope_a);
        let socket_b = default_socket_path_in(&scope_b);

        unsafe {
            if let Some(h) = original_home {
                std::env::set_var("HOME", h);
            } else {
                std::env::remove_var("HOME");
            }
            if let Some(r) = original_airc_runtime_dir {
                std::env::set_var("AIRC_RUNTIME_DIR", r);
            } else {
                std::env::remove_var("AIRC_RUNTIME_DIR");
            }
        }

        assert_ne!(
            socket_a, socket_b,
            "isolated scopes outside the OS user account must keep \
             per-project sockets so parallel test runs don't collide; \
             got the same socket for two unrelated isolated roots: \
             {socket_a:?}"
        );
    }
}

#[derive(Debug, Args)]
pub struct PeerArgs {
    #[command(subcommand)]
    pub action: PeerAction,
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
