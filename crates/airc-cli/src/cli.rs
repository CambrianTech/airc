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
    for ancestor in cwd.ancestors() {
        if ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == ".airc")
        {
            return ancestor.to_path_buf();
        }
    }
    git_toplevel(cwd)
        .map(|root| root.join(".airc"))
        .unwrap_or_else(|| cwd.join(".airc"))
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
/// The socket lives **inside the machine-account home** (`~/.airc`), not
/// a temp dir: every scope under one user's `$HOME` resolves to the same
/// `~/.airc/daemon-v<N>.sock`, so they all reach the ONE machine-singular
/// daemon (§1), and there's no hashing — the home dir IS the unique key.
/// The per-socket `DaemonBindGuard` then guarantees a single owner: the
/// first scope to start binds it, the rest attach.
///
/// The filename includes `airc_ipc::IPC_PROTOCOL_VERSION`: if the local
/// daemon wire protocol changes, a new client must not talk to an old
/// daemon that still owns the prior socket.
pub fn default_socket_path_in(home: &std::path::Path) -> PathBuf {
    airc_lib::machine_account_home(home)
        .join(format!("daemon-v{}.sock", airc_ipc::IPC_PROTOCOL_VERSION))
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
    Init,

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
    },
    /// Remove a peer from local trust.
    Remove {
        /// Peer UUID to remove from the trust store.
        peer_id: String,
        /// Override the default daemon IPC endpoint.
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List enrolled peers from the peer trust store.
    List,
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
