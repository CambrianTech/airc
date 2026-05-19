//! airc-rs — Rust substrate CLI binary.
//!
//! State lives under `<home>` (default `$HOME/.airc-rs`):
//!   - `identity.key`   — 32-byte Ed25519 secret (0600)
//!   - `identity.json`  — stable peer_id + client_id (0600)
//!   - `daemon.sock`    — IPC socket
//!   - `peers.json`     — (next PR) persisted peer registry
//!
//! `airc-rs init` is the only command that creates the identity from
//! nothing. All others load `<home>/identity.{key,json}` (auto-
//! generating if absent). `VerificationPolicy::Strict` is the only
//! policy used in CLI paths — no `AllowUnsigned` opt-in.

mod cli;
mod commands;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use uuid::Uuid;

use airc_core::PeerId;

use airc_daemon::LocalIdentity;
use cli::{Cli, Command, PeerAction};

fn parse_peer_id(input: &str) -> Result<PeerId, Box<dyn std::error::Error>> {
    let uuid = Uuid::from_str(input)
        .map_err(|error| format!("--expected-peer {input:?} is not a valid UUID: {error}"))?;
    Ok(PeerId::from_uuid(uuid))
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();

    let parsed = Cli::parse();
    match dispatch(parsed).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("airc-rs: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(parsed: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let home = parsed.home.clone().unwrap_or_else(cli::default_home_dir);

    match parsed.command {
        Command::Init => commands::run_init(&home),

        Command::Send { text } => {
            let identity = LocalIdentity::load_or_generate(&home)?;
            commands::run_send(&home, &identity, parsed.peers, &text).await
        }

        Command::Listen { replay } => {
            let identity = LocalIdentity::load_or_generate(&home)?;
            commands::run_listen(&home, &identity, parsed.peers, replay).await
        }

        Command::LanSend {
            to,
            expected_peer,
            text,
        } => {
            let identity = LocalIdentity::load_or_generate(&home)?;
            let expected = parse_peer_id(&expected_peer)?;
            commands::run_lan_send(&home, &identity, parsed.peers, to, expected, &text).await
        }

        Command::LanListen { bind, replay } => {
            let identity = LocalIdentity::load_or_generate(&home)?;
            commands::run_lan_listen(&home, &identity, parsed.peers, bind, replay).await
        }

        Command::Daemon { socket } => {
            let identity = LocalIdentity::load_or_generate(&home)?;
            let socket = default_or(socket, &home);
            commands::run_daemon(&home, identity, parsed.peers, socket).await
        }

        Command::Ping { socket } => commands::run_ping(default_or(socket, &home)).await,
        Command::Status { socket } => commands::run_status(default_or(socket, &home)).await,
        Command::Stop { socket } => commands::run_stop(default_or(socket, &home)).await,

        Command::Msg { socket, text } => {
            commands::run_msg(&home, default_or(socket, &home), &text).await
        }

        Command::Inbox {
            socket,
            since_lamport,
            since_event_id,
            limit,
        } => {
            commands::run_inbox(
                &home,
                default_or(socket, &home),
                since_lamport,
                since_event_id,
                limit,
            )
            .await
        }

        Command::Room { name, wire } => commands::run_room(&home, name, wire),

        Command::Peer(args) => match args.action {
            PeerAction::Add { spec, socket } => {
                commands::run_peer_add(&home, spec, default_or(socket, &home)).await
            }
            PeerAction::List => commands::run_peer_list(&home),
        },
    }
}

/// Resolve `--socket` override to its value, falling back to the
/// home-derived default.
fn default_or(explicit: Option<PathBuf>, home: &Path) -> PathBuf {
    explicit.unwrap_or_else(|| cli::default_socket_path_in(home))
}
