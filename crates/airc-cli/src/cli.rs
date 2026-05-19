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
pub fn default_home_dir() -> PathBuf {
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".airc-rs")
    } else {
        // No HOME — fall back to a clearly-scoped path under /tmp so
        // accidents don't clobber real homes.
        PathBuf::from("/tmp").join("airc-rs")
    }
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
    /// `$HOME/.airc-rs`. Override for tests or multi-identity setups.
    #[arg(long, env = "AIRC_RS_HOME", global = true)]
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

    /// Send a single text Message frame and exit.
    Send {
        /// Wire directory (must be the same as the receiver's).
        #[arg(long)]
        wire: PathBuf,
        /// Channel UUID (any unique value shared with the receiver).
        #[arg(long, value_name = "UUID")]
        channel: String,
        /// Message body.
        text: String,
    },

    /// Subscribe and print frames until interrupted (Ctrl-C).
    Listen {
        /// Wire directory.
        #[arg(long)]
        wire: PathBuf,
        /// Channel filter (optional). If omitted, all channels.
        #[arg(long, value_name = "UUID")]
        channel: Option<String>,
        /// Replay from the start of the wire instead of live-only.
        #[arg(long)]
        replay: bool,
    },

    /// Same-LAN secure send: dial a peer over TLS and send a single
    /// text frame.
    LanSend {
        /// Address of the listening peer (e.g. `127.0.0.1:7474`).
        #[arg(long)]
        to: SocketAddr,
        /// UUID of the listening peer (for cert pinning).
        #[arg(long)]
        expected_peer: String,
        /// Channel UUID.
        #[arg(long)]
        channel: String,
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

    /// Send a text message via the running daemon (fast — no
    /// per-call substrate setup).
    Msg {
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Wire directory the daemon should write to.
        #[arg(long)]
        wire: PathBuf,
        /// Channel UUID.
        #[arg(long)]
        channel: String,
        /// Message body.
        text: String,
    },

    /// Pull buffered frames from the daemon's inbox for a wire.
    /// On first call for a wire, the daemon starts subscribing
    /// (idempotent). Pass `--since-lamport` for consume-once cursor.
    Inbox {
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Wire directory.
        #[arg(long)]
        wire: PathBuf,
        #[arg(long)]
        since_lamport: Option<u64>,
        #[arg(long)]
        limit: Option<usize>,
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
