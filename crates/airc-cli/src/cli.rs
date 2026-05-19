//! Command-line interface definitions (clap derive).
//!
//! All commands default to the persisted state at `<home>` (default
//! `$HOME/.airc-rs`), which contains:
//!   - `identity.key`   — 32-byte Ed25519 secret (0600 on Unix)
//!   - `identity.json`  — stable peer_id + client_id (0600)
//!   - `daemon.sock`    — IPC socket for the daemon
//!   - `peers.json`     — (future) persisted peer registry
//!
//! The `--home` flag overrides for testing / multi-identity setups.

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::registry::PeerSpec;

/// Default home directory for persisted identity + IPC state.
///
/// Resolution order:
///   1. `$HOME` (Unix; also Git Bash on Windows) → `<home>/.airc-rs`
///   2. `%USERPROFILE%` (native Windows cmd / PowerShell) →
///      `<userprofile>/.airc-rs`
///   3. fallback to `./.airc-rs` in the current working dir
pub fn default_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        return PathBuf::from(home).join(".airc-rs");
    }
    #[cfg(windows)]
    if let Some(userprofile) = std::env::var_os("USERPROFILE") {
        return PathBuf::from(userprofile).join(".airc-rs");
    }
    PathBuf::from(".airc-rs")
}

/// Default Unix socket path inside `home`.
pub fn default_socket_path_in(home: &std::path::Path) -> PathBuf {
    home.join("daemon.sock")
}

/// airc-rs — Rust substrate CLI. Replaces the Python `airc` step by
/// step; each subcommand exercises one slice of the substrate.
#[derive(Debug, Parser)]
#[command(
    name = "airc-rs",
    version,
    about = "AIRC substrate CLI (Rust)",
    long_about = "Cross-process / cross-machine AI chat over the airc substrate. \
                  Replaces the Python airc CLI as the Rust path matures."
)]
pub struct Cli {
    /// State directory for persisted identity + IPC socket. Default
    /// `$HOME/.airc-rs` (Unix) or `%USERPROFILE%/.airc-rs` (Windows).
    /// Override for tests or multi-identity setups.
    #[arg(long, global = true)]
    pub home: Option<PathBuf>,

    /// Ad-hoc peers to enrol for this invocation only, repeatable.
    /// Format: `<uuid>:<base64-pubkey-no-padding>`. Persistent peers
    /// come from `<home>/peers.json` (managed via `airc-rs peer add`);
    /// this flag unions on top for one-shot use.
    #[arg(long = "peer", value_name = "SPEC", global = true)]
    pub peers: Vec<PeerSpec>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or load the persisted identity (`<home>/identity.key` +
    /// `<home>/identity.json`), then print this peer's spec for
    /// out-of-band sharing. Idempotent — repeat runs return the same
    /// peer_id.
    Init,

    /// Send a single text Message frame to the current room and exit.
    /// The current room lives in `<home>/room.json`; switch with
    /// `airc-rs room <name>`.
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
        /// Override the default socket path (`<home>/daemon.sock`).
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
    },

    /// Print or switch the current room. With no name, prints the
    /// current room's name + wire + channel. With a name, derives a
    /// deterministic `(wire, channel)` from the name and sets it as
    /// the current room — two peers who run `airc-rs room project-x`
    /// land in the same channel without sharing the UUID.
    Room {
        /// Room name. Omit to just print the current room.
        name: Option<String>,
        /// Override the default wire path (`<home>/wires/<name>/`).
        /// Use for shared-wire setups (e.g. local-fs tests where two
        /// processes need to read/write the same dir).
        #[arg(long)]
        wire: Option<PathBuf>,
    },

    /// Manage the persisted peer registry (`<home>/peers.json`).
    Peer(PeerArgs),
}

#[derive(Debug, Args)]
pub struct PeerArgs {
    #[command(subcommand)]
    pub action: PeerAction,
}

#[derive(Debug, Subcommand)]
pub enum PeerAction {
    /// Enrol a peer by spec. If a daemon is running on
    /// `<home>/daemon.sock`, also tells it via RPC so the in-memory
    /// registry stays in sync — no daemon restart required.
    Add {
        /// Peer spec: `<uuid>:<base64-pubkey-no-padding>` (the
        /// `peer_spec:` line from the other side's `airc-rs init`).
        spec: PeerSpec,
        /// Override the default socket (`<home>/daemon.sock`).
        #[arg(long)]
        socket: Option<PathBuf>,
    },
    /// List enrolled peers. Reads from peers.json on disk (the
    /// daemon writes the same file, so both views agree).
    List,
}
