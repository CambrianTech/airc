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
mod codex_cli;
mod codex_commands;
mod codex_config;
mod codex_hooks_json;
mod codex_install;
mod commands;
mod events_cli;
mod events_commands;
mod lane_cli;
mod lane_commands;
mod work_cli;
mod work_commands;
mod workspace_cli;
mod workspace_commands;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use uuid::Uuid;

use airc_core::PeerId;

use airc_daemon::LocalIdentity;
use cli::{Cli, Command, PeerAction};
use codex_cli::CodexHookAction;
use events_cli::EventsAction;
use lane_cli::{LaneAction, LaneManagerAction};
use work_cli::WorkAction;
use workspace_cli::WorkspaceAction;

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
        Command::Init => commands::run_init(&home).await,

        Command::Send { text } => commands::run_send(&home, parsed.peers, &text).await,

        Command::Listen { replay } => commands::run_listen(&home, parsed.peers, replay).await,

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

        Command::Room { name, wire } => commands::run_room(&home, name, wire).await,

        Command::Peer(args) => match args.action {
            PeerAction::Add { spec, socket } => {
                commands::run_peer_add(&home, spec, default_or(socket, &home)).await
            }
            PeerAction::List => commands::run_peer_list(&home).await,
        },

        Command::Events(args) => match args.action {
            EventsAction::List {
                kind,
                header,
                header_prefix,
                limit,
            } => events_commands::run_list(&home, kind, header, header_prefix, limit).await,
        },

        Command::CodexHook(args) => match args.action {
            CodexHookAction::InstallHooks { codex_home } => {
                codex_install::run_install_hooks(codex_home).await
            }
            CodexHookAction::UninstallHooks { codex_home } => {
                codex_install::run_uninstall_hooks(codex_home).await
            }
            CodexHookAction::UserPromptSubmit {
                cursor_file,
                count,
                max_items,
                raw,
                include_self,
            } => {
                codex_commands::run_user_prompt_submit(
                    &home,
                    cursor_file,
                    count,
                    max_items,
                    raw,
                    include_self,
                )
                .await
            }
        },

        Command::Work(args) => match args.action {
            WorkAction::Create {
                repo,
                title,
                body,
                lane_id,
                priority,
            } => work_commands::run_create(&home, repo, title, body, lane_id, priority).await,
            WorkAction::Claim { card_id, ttl_ms } => {
                work_commands::run_claim(&home, card_id, ttl_ms).await
            }
            WorkAction::Release {
                card_id,
                claim_id,
                reason,
            } => work_commands::run_release(&home, card_id, claim_id, reason).await,
            WorkAction::Board { limit } => work_commands::run_board(&home, limit).await,
        },

        Command::Lane(args) => match args.action {
            LaneAction::Create { repo, title, state } => {
                lane_commands::run_create(&home, repo, title, state).await
            }
            LaneAction::State { lane_id, state } => {
                lane_commands::run_state(&home, lane_id, state).await
            }
            LaneAction::Status { limit } => lane_commands::run_status(&home, limit).await,
            LaneAction::Manager { action } => match action {
                LaneManagerAction::Claim { repo, ttl_ms } => {
                    lane_commands::run_manager_claim(&home, repo, ttl_ms).await
                }
                LaneManagerAction::Release { repo } => {
                    lane_commands::run_manager_release(&home, repo).await
                }
                LaneManagerAction::Status { limit } => {
                    lane_commands::run_manager_status(&home, limit).await
                }
            },
        },

        Command::Workspace(args) => match args.action {
            WorkspaceAction::Request {
                card_id,
                claim_id,
                repo,
                branch,
                base,
            } => {
                workspace_commands::run_request(&home, card_id, claim_id, repo, branch, base).await
            }
            WorkspaceAction::Allocate { workspace_id, path } => {
                workspace_commands::run_allocate(&home, workspace_id, path).await
            }
            WorkspaceAction::Heartbeat {
                workspace_id,
                disk_bytes,
            } => workspace_commands::run_heartbeat(&home, workspace_id, disk_bytes).await,
            WorkspaceAction::Release { workspace_id } => {
                workspace_commands::run_release(&home, workspace_id).await
            }
            WorkspaceAction::List { limit } => workspace_commands::run_list(&home, limit).await,
        },
    }
}

/// Resolve `--socket` override to its value, falling back to the
/// home-derived default.
fn default_or(explicit: Option<PathBuf>, home: &Path) -> PathBuf {
    explicit.unwrap_or_else(|| cli::default_socket_path_in(home))
}
