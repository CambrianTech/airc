//! airc-rs — Rust substrate CLI binary.
//!
//! Wires the substrate crates (`airc-core`, `airc-protocol`,
//! `airc-transport`) into a small command-line tool that proves the
//! Rust substrate works end-to-end without any Python. Two terminals
//! can chat over local-fs or LAN-TCP via this binary.
//!
//! MVP scope:
//!   - `init` — generate / load identity, print peer spec.
//!   - `send` / `listen` — local-fs (same-Mac multi-process).
//!   - `lan-send` / `lan-listen` — TLS-wrapped TCP (same-LAN secure).
//!
//! What's deferred:
//!   - Persistent registry (for now, peers are passed via repeated
//!     `--peer` flags on each command).
//!   - A bridge daemon that holds state for short-lived CLI calls.
//!   - Cross-platform polish (Windows ACLs, etc.).

mod cli;
mod commands;
mod identity;
mod registry;

use std::process::ExitCode;

use clap::Parser;
use uuid::Uuid;

use airc_core::PeerId;

use cli::{Cli, Command};

fn parse_peer_id(input: &str) -> Result<PeerId, Box<dyn std::error::Error>> {
    let uuid = Uuid::parse_str(input)
        .map_err(|error| format!("--peer-id {input:?} is not a valid UUID: {error}"))?;
    Ok(PeerId::from_uuid(uuid))
}

#[tokio::main]
async fn main() -> ExitCode {
    // Install the rustls crypto provider once at process start so all
    // TLS paths Just Work. Ignore the duplicate-install error in case
    // a test harness installed it first.
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
    let identity_file = parsed
        .identity_file
        .clone()
        .ok_or("--identity-file or AIRC_RS_IDENTITY env var is required")?;

    let parsed_peer_id = match parsed.peer_id.as_deref() {
        Some(input) => Some(parse_peer_id(input)?),
        None => None,
    };

    match parsed.command {
        Command::Init => commands::run_init(&identity_file, parsed_peer_id).map_err(Into::into),
        Command::Send {
            wire,
            channel,
            text,
        } => {
            let peer_id = parsed_peer_id.ok_or("--peer-id is required for send")?;
            commands::run_send(
                &identity_file,
                peer_id,
                parsed.peers,
                &wire,
                &channel,
                &text,
            )
            .await
        }
        Command::Listen {
            wire,
            channel,
            replay,
        } => {
            let peer_id = parsed_peer_id.ok_or("--peer-id is required for listen")?;
            commands::run_listen(
                &identity_file,
                peer_id,
                parsed.peers,
                &wire,
                channel,
                replay,
            )
            .await
        }
        Command::LanSend {
            to,
            expected_peer,
            channel,
            text,
        } => {
            let peer_id = parsed_peer_id.ok_or("--peer-id is required for lan-send")?;
            let expected = parse_peer_id(&expected_peer)?;
            commands::run_lan_send(
                &identity_file,
                peer_id,
                parsed.peers,
                to,
                expected,
                &channel,
                &text,
            )
            .await
        }
        Command::LanListen { bind, replay } => {
            let peer_id = parsed_peer_id.ok_or("--peer-id is required for lan-listen")?;
            commands::run_lan_listen(&identity_file, peer_id, parsed.peers, bind, replay).await
        }
    }
}
