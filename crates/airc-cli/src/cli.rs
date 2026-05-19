//! Command-line interface definitions (clap derive).

use std::net::SocketAddr;
use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::registry::PeerSpec;

/// Default Unix socket path for the daemon. Overridable per-command
/// via `--socket`.
pub fn default_socket_path() -> PathBuf {
    // ~/.airc-rs/daemon.sock if HOME is set; otherwise /tmp.
    if let Some(home) = std::env::var_os("HOME") {
        PathBuf::from(home).join(".airc-rs").join("daemon.sock")
    } else {
        PathBuf::from("/tmp").join("airc-rs-daemon.sock")
    }
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
    /// Path to the 32-byte Ed25519 identity file. Created on first
    /// use if absent.
    #[arg(long, env = "AIRC_RS_IDENTITY", global = true)]
    pub identity_file: Option<PathBuf>,

    /// This peer's UUID. Generated and printed on first `init`;
    /// pass it back on subsequent commands.
    #[arg(long, env = "AIRC_RS_PEER_ID", global = true)]
    pub peer_id: Option<String>,

    /// Peers to enrol in the local registry, repeatable.
    /// Format: `<uuid>:<base64-pubkey-no-padding>`. Obtain by
    /// running `airc-rs init` on the peer side and copying its
    /// printed spec.
    #[arg(long = "peer", value_name = "SPEC", global = true)]
    pub peers: Vec<PeerSpec>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Create or load an identity, then print this peer's spec for
    /// out-of-band sharing.
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

    /// Same-LAN secure listen: bind a TLS server, accept ONE peer
    /// (MVP), print received frames.
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
        /// Override the default socket path
        /// (`$HOME/.airc-rs/daemon.sock`).
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
}
