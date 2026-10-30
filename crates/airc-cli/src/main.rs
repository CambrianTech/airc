//! airc — Rust substrate CLI.
//!
//! State lives under `<home>` (default `$HOME/.airc`):
//!   - `identity.key`   — 32-byte Ed25519 secret (0600)
//!   - `daemon.sock`    — IPC socket

// Card 7b87da8f / ef168afe: CLI rendering + refusal-message paths
// uniformly use a wildcard arm to mean "any other variant — show the
// debug label" (event_render) or "any future substrate-owned target —
// bug in airc-cli, not a runtime branch" (refusal_message). The
// wildcards ARE the idiom; enumerating every variant would force the
// CLI to track every domain enum's variant set forever, which trades
// genuine future-proofing for surface-area churn.
#![allow(clippy::wildcard_enum_match_arm)]
//!   - `events.sqlite`  — ORM-backed identity metadata, events, cursors, peer
//!     trust, subscriptions, and coordinator state
//!
//! `airc init` is the explicit identity bootstrap command. Other
//! substrate commands open the same ORM-backed runtime state and
//! auto-generate missing identity material. `VerificationPolicy::Strict`
//! is the only policy used in CLI paths — no `AllowUnsigned` opt-in.

mod addressed_bridge;
mod agent_resume;
mod build_info;
mod channel_gist_cli;
mod channel_gist_commands;
mod cli;
mod client_id;
mod collaboration_cli;
mod collaboration_commands;
mod collaboration_peers;
mod commands;
mod discovery;
mod doctor;
#[cfg(windows)]
mod elevated_owner;
mod envelope_cli;
mod event_render;
mod events_cli;
mod events_commands;
mod gh_cli;
mod gh_client;
mod gh_commands;
mod gh_reqwest;
mod gh_state;
mod gist_cli;
mod gist_commands;
mod handshake_cli;
mod handshake_commands;
mod hygiene_cli;
mod hygiene_commands;
mod identity_cli;
mod identity_commands;
mod integrations;
mod join_feed;
mod json_path;
mod knock_cli;
mod knock_commands;
mod lane_cli;
mod lane_commands;
mod lease;
mod legacy_envelope;
mod legacy_identity;
mod merger;
mod monitor;
mod network_commands;
mod pending_cli;
mod pending_commands;
mod publish_commands;
mod queue_card_cli;
mod queue_card_commands;
mod queue_card_plan;
mod queue_card_projection;
mod queue_card_runtime;
mod queue_card_staleness;
mod registry_cli;
mod registry_commands;
mod route_cli;
mod route_commands;
mod route_proof_commands;
mod runtime_context;
mod runtime_dir;
mod sos_commands;
mod staleness;
mod state_cli;
mod state_commands;
#[cfg(test)]
mod test_env;
mod transport_cli;
mod transport_commands;
mod update_artifact;
mod update_commands;
mod update_legacy;
mod update_preflight;
mod update_rollback;
#[cfg(windows)]
mod update_session;
mod update_shutdown;
mod work_cli;
mod work_commands;
mod work_commands_gh;
mod work_commands_git;
mod work_suggestions;
mod workspace_cli;
mod workspace_commands;
mod worktree_lane_cli;
mod worktree_lane_commands;
mod worktree_retirement;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use uuid::Uuid;

use airc_core::PeerId;

use channel_gist_cli::ChannelGistAction;
use cli::{Cli, Command, PeerAction, SosAction};
use collaboration_cli::CollaborationAction;
use envelope_cli::EnvelopeAction;
use events_cli::EventsAction;
use gh_cli::GhAction;
use gist_cli::GistAction;
use handshake_cli::HandshakeAction;
use identity_cli::IdentityAction;
use knock_cli::KnockAction;
use lane_cli::{LaneAction, LaneManagerAction};
use monitor::MonitorAction;
use pending_cli::PendingAction;
use route_cli::RouteAction;
use state_cli::StateAction;
use transport_cli::TransportAction;
use workspace_cli::WorkspaceAction;
use worktree_lane_cli::WorktreeLaneAction;

fn parse_peer_id(input: &str) -> Result<PeerId, Box<dyn std::error::Error>> {
    let uuid = Uuid::from_str(input)
        .map_err(|error| format!("--expected-peer {input:?} is not a valid UUID: {error}"))?;
    Ok(PeerId::from_uuid(uuid))
}

#[cfg(windows)]
const WINDOWS_MAIN_STACK_BYTES: usize = 8 * 1024 * 1024;

fn main() -> ExitCode {
    // Clippy on Windows flags an early `return` here as unneeded — and
    // it's right: with the cfg-split, each branch is the function's
    // tail expression. (First caught by the first-ever Windows clippy
    // run, on bigmama; the hosted clippy gate is ubuntu-only.)
    #[cfg(windows)]
    {
        match std::thread::Builder::new()
            .name("airc-main".to_string())
            .stack_size(WINDOWS_MAIN_STACK_BYTES)
            .spawn(run_main)
        {
            Ok(handle) => match handle.join() {
                Ok(code) => code,
                Err(_) => {
                    eprintln!("airc: main thread panicked");
                    ExitCode::FAILURE
                }
            },
            Err(error) => {
                eprintln!("airc: failed to start main thread: {error}");
                ExitCode::FAILURE
            }
        }
    }

    #[cfg(not(windows))]
    run_main()
}

fn run_main() -> ExitCode {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("airc: failed to start tokio runtime: {error}");
            return ExitCode::FAILURE;
        }
    };
    runtime.block_on(async_main())
}

async fn async_main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();

    // Card f10c951e — print a banner if this binary is behind
    // origin/rust-rewrite tip for any airc crate. Best-effort, cheap,
    // never blocks. The single highest-friction onboarding issue.
    staleness::warn_if_stale();

    // One rule for every store this process opens (airc_lib::store_admission).
    airc_lib::store_admission::install();
    let parsed = Cli::parse();
    match dispatch(parsed).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if let Some(code) = channel_gist_commands::command_exit_code(error.as_ref()) {
                return ExitCode::from(code);
            }
            if let Some(code) = collaboration_commands::command_exit_code(error.as_ref()) {
                return ExitCode::from(code);
            }
            if let Some(code) = identity_commands::command_exit_code(error.as_ref()) {
                return ExitCode::from(code);
            }
            #[cfg(windows)]
            if let Some(code) = update_session::exit_code(error.as_ref()) {
                return ExitCode::from(code);
            }
            eprintln!("airc: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(parsed: Cli) -> Result<(), Box<dyn std::error::Error>> {
    // Seam #1: `--home <path>` or `--here` (cwd-local scope) override the
    // default machine-account / git-project resolution. `--here` is the
    // ergonomic `AIRC_HOME=$PWD/.airc` shortcut.
    let cwd = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
    let home = cli::explicit_home_override(parsed.home.clone(), parsed.here, &cwd)
        .unwrap_or_else(cli::default_home_dir);

    match parsed.command {
        Command::Init { agent_name } => commands::run_init(&home, agent_name).await,

        Command::LanIp => network_commands::run_lan_ip(),

        Command::Collaboration(args) => match args.action {
            CollaborationAction::Status(args) => {
                collaboration_commands::run_status(&home, args).await
            }
            CollaborationAction::Doctor(args) => {
                collaboration_commands::run_doctor(&home, args).await
            }
            CollaborationAction::SendWarning(args) => {
                collaboration_commands::run_send_warning(&home, args).await
            }
            CollaborationAction::Peers(args) => collaboration_peers::run_peers(&home, args).await,
            CollaborationAction::PrunePeers(args) => {
                collaboration_peers::run_prune_peers(&home, args)
            }
        },

        Command::ChannelGist(args) => match args.action {
            ChannelGistAction::Resolve {
                channel,
                create_if_missing,
                require_invite,
            } => channel_gist_commands::run_resolve(&channel, create_if_missing, require_invite),
            ChannelGistAction::Find {
                channel,
                require_invite,
            } => channel_gist_commands::run_find(&channel, require_invite),
            ChannelGistAction::HostPreflight { channel, config } => {
                channel_gist_commands::run_host_preflight(&channel, config.as_deref())
            }
            ChannelGistAction::RememberCreated {
                channel,
                gist_id,
                description,
                payload_file,
            } => channel_gist_commands::run_remember_created(
                &channel,
                &gist_id,
                &description,
                &payload_file,
            ),
        },

        Command::Identity(args) => match args.action {
            IdentityAction::Bootstrap { identity_dir } => {
                println!("{}", legacy_identity::bootstrap_x25519(&identity_dir)?);
                Ok(())
            }
            IdentityAction::BootstrapEd25519 { identity_dir } => {
                legacy_identity::bootstrap_ed25519(&identity_dir)
            }
            IdentityAction::PeerPub {
                peers_dir,
                peer_name,
            } => {
                if let Some(pubkey) = legacy_identity::peer_pub(&peers_dir, &peer_name)? {
                    println!("{pubkey}");
                }
                Ok(())
            }
            IdentityAction::PeerSshPub {
                peers_dir,
                peer_name,
            } => identity_commands::run_peer_ssh_pub(&peers_dir, &peer_name),
            IdentityAction::SignEd25519 { identity_dir } => {
                println!("{}", legacy_identity::sign_ed25519_stdin(&identity_dir)?);
                Ok(())
            }
            IdentityAction::Pretty {
                name,
                identity_json,
                host,
            } => identity_commands::run_pretty(&name, &identity_json, &host),
            IdentityAction::WritePeerRecord {
                peers_dir,
                peer_name,
                host,
                airc_home,
                x25519_pub,
                paired,
            } => identity_commands::run_write_peer_record(
                &peers_dir,
                &peer_name,
                &host,
                &airc_home,
                &x25519_pub,
                &paired,
            ),
            IdentityAction::SessionFile {
                write_dir,
                transport_name,
            } => identity_commands::run_session_file(&write_dir, &transport_name),
            IdentityAction::DefaultWorkName {
                transport_name,
                session_file,
            } => identity_commands::run_default_work_name(&transport_name, &session_file),
            IdentityAction::ReadWorkName { session_file } => {
                identity_commands::run_read_work_name(&session_file)
            }
            IdentityAction::WriteWorkSession {
                session_file,
                name,
                transport_name,
            } => identity_commands::run_write_work_session(&session_file, &name, &transport_name),
            IdentityAction::Show => identity_commands::run_show(&home).await,
            IdentityAction::Set {
                name,
                pronouns,
                role,
                bio,
                status,
            } => identity_commands::run_set(&home, name, pronouns, role, bio, status).await,
            IdentityAction::Link { platform, handle } => {
                identity_commands::run_link(&home, &platform, &handle).await
            }
            IdentityAction::NudgeNeeded => identity_commands::run_nudge_needed(&home).await,
            IdentityAction::ImportContinuum { blob } => {
                identity_commands::run_import_continuum(&home, &blob).await
            }
            IdentityAction::ContinuumHandle => identity_commands::run_continuum_handle(&home).await,
            IdentityAction::PushContinuum { handle } => {
                identity_commands::run_push_continuum(&home, &handle).await
            }
        },

        Command::State(args) => match args.action {
            StateAction::Get { key, in_room } => {
                state_commands::run_get(&home, &key, in_room).await
            }
            StateAction::Set {
                key,
                value,
                version,
                in_room,
            } => state_commands::run_set(&home, &key, &value, version, in_room).await,
            StateAction::List { in_room } => state_commands::run_list(&home, in_room).await,
            StateAction::Delete { key, in_room } => {
                state_commands::run_delete(&home, &key, in_room).await
            }
        },

        Command::Envelope(args) => match args.action {
            EnvelopeAction::Wrap {
                recipient_pub,
                identity_dir,
            } => legacy_envelope::wrap_stdin(&recipient_pub, &identity_dir),
            EnvelopeAction::Unwrap {
                sender_pub,
                identity_dir,
            } => legacy_envelope::unwrap_stdin(&sender_pub, &identity_dir),
        },

        Command::Send { room, text } => {
            commands::run_send(&home, parsed.peers, room.named(), &text).await
        }

        Command::Listen { replay } => commands::run_listen(&home, parsed.peers, replay).await,

        Command::Dial {
            to,
            peer,
            timeout_ms,
        } => {
            let expected = match peer {
                Some(raw) => Some(parse_peer_id(&raw)?),
                None => None,
            };
            commands::run_dial(&home, parsed.peers, to, expected, timeout_ms).await
        }

        Command::LanSend {
            to,
            expected_peer,
            ack_timeout_ms,
            text,
        } => {
            let expected = parse_peer_id(&expected_peer)?;
            commands::run_lan_send(&home, parsed.peers, to, expected, ack_timeout_ms, &text).await
        }

        Command::LanListen { bind, replay } => {
            commands::run_lan_listen(&home, parsed.peers, bind, replay).await
        }

        Command::Daemon { socket } => {
            let socket = default_or(socket, &home);
            commands::run_daemon(&home, parsed.peers, socket).await
        }

        Command::Ping { socket } => commands::run_ping(default_or(socket, &home)).await,
        Command::Status { socket, json } => {
            let socket = default_or(socket, &home);
            if json {
                update_shutdown::print_status_json(&socket)
            } else {
                commands::run_status(&home, socket).await
            }
        }
        Command::Stop { socket } => commands::run_stop(&home, default_or(socket, &home)).await,

        Command::Msg {
            to,
            socket,
            room,
            text,
            stdin,
        } => {
            // Body from STDIN when asked, so prose never passes through shell
            // quoting (see the `--stdin` doc on the Msg variant: this class cost
            // the grid a core on 2026-09-05). Read to EOF verbatim — no trim, no
            // interpretation; a heredoc's trailing newline is the author's.
            let body = match (text, stdin) {
                (Some(t), _) => t,
                (None, true) => {
                    use std::io::{IsTerminal, Read};
                    // `--stdin` with nothing piped in would BLOCK on a terminal, waiting for an
                    // EOF the operator has to know to send. That is a hang in a deploy-adjacent
                    // tool, which is the failure class this grid spent tonight removing — so
                    // refuse it immediately and say how, rather than appearing to freeze.
                    if std::io::stdin().is_terminal() {
                        return Err("`--stdin` was passed but stdin is a terminal — nothing is \
                                    piped in and this would wait forever. Redirect a file \
                                    (`airc msg --stdin < body.txt`) or use a heredoc."
                            .to_string()
                            .into());
                    }
                    let mut buf = String::new();
                    std::io::stdin()
                        .read_to_string(&mut buf)
                        .map_err(|e| format!("reading message body from stdin: {e}"))?;
                    // An EMPTY read is the same silent corruption this flag exists to end, and
                    // my own commit message claimed it was handled when only the both-absent
                    // case was (caught in review by ce8b9074 — the review the expired window
                    // would have skipped). Whitespace-only counts: a heredoc that expanded to
                    // nothing posts a blank message that reads as a delivery failure to
                    // everyone who sees it.
                    if buf.trim().is_empty() {
                        return Err(
                            "`--stdin` produced an empty message body — nothing was piped \
                                    in, or it expanded to whitespace. Refusing rather than \
                                    posting a blank message."
                                .to_string()
                                .into(),
                        );
                    }
                    buf
                }
                // clap enforces `conflicts_with`, so the remaining gap is BOTH
                // absent. Refuse rather than send an empty message: a silent
                // empty post is the same class of quiet corruption `--stdin`
                // exists to end.
                (None, false) => {
                    return Err("no message body — pass it as an argument, or use \
                                `--stdin` (recommended for prose: it cannot be \
                                mangled by shell quoting)"
                        .to_string()
                        .into())
                }
            };
            commands::run_msg(
                &home,
                default_or(socket, &home),
                room.named(),
                &body,
                to.map(airc_core::PeerId::from_uuid),
            )
            .await
        }
        Command::Publish {
            room,
            body_text,
            body_json,
            stdin,
            headers,
            class,
        } => {
            publish_commands::run_publish(
                &home, room.room, body_text, body_json, stdin, headers, class,
            )
            .await
        }

        Command::Inbox {
            socket,
            room,
            since_lamport,
            since_event_id,
            limit,
            all,
            json,
        } => {
            if all {
                commands::run_inbox_all(&home, socket, limit, json).await
            } else {
                commands::run_inbox(
                    &home,
                    socket,
                    room.named(),
                    since_lamport,
                    since_event_id,
                    limit,
                    json,
                )
                .await
            }
        }

        Command::Room { name } => commands::run_room(&home, name).await,

        Command::DoctrinePublish { from_file } => {
            commands::run_doctrine_publish(&home, from_file).await
        }

        Command::Part { room } => commands::run_part(&home, room).await,

        Command::Peer(args) => match args.action {
            PeerAction::Add {
                spec,
                socket,
                tier,
                endpoints,
            } => {
                commands::run_peer_add(
                    &home,
                    spec,
                    default_or(socket, &home),
                    tier.map(Into::into),
                    endpoints,
                )
                .await
            }
            PeerAction::Remove { peer_id, socket } => {
                let peer_id = parse_peer_id(&peer_id)?;
                commands::run_peer_remove(&home, peer_id, default_or(socket, &home)).await
            }
            PeerAction::SetTier {
                peer_id,
                tier,
                socket,
            } => {
                let peer_id = parse_peer_id(&peer_id)?;
                commands::run_peer_set_tier(&home, peer_id, tier.into(), default_or(socket, &home))
                    .await
            }
            PeerAction::List { json } => commands::run_peer_list(&home, json).await,
            PeerAction::Prune {
                apply,
                stale_after_hours,
            } => commands::run_peer_prune(&home, apply, stale_after_hours).await,
        },

        Command::Peers => commands::run_peer_list(&home, false).await,

        Command::Network { all } => network_commands::run_network(&home, all).await,

        Command::Whois { peer } => match peer {
            Some(peer) => commands::run_whois_peer(&home, &peer).await,
            None => identity_commands::run_show(&home).await,
        },

        Command::Route(args) => match args.action {
            RouteAction::Status(args) => route_commands::run_status(args),
            RouteAction::Proof(args) => route_proof_commands::run(args).await,
        },

        Command::Registry(args) => match args.action {
            registry_cli::RegistryAction::Sync { allow_endpointless } => {
                registry_commands::run_sync(&home, allow_endpointless).await
            }
            registry_cli::RegistryAction::Gc { apply } => {
                registry_commands::run_gc(&home, apply).await
            }
        },

        Command::Transport(args) => match args.action {
            TransportAction::Health {
                quiet,
                degraded_only,
                fail,
                json,
            } => transport_commands::run_health(&home, quiet, degraded_only, fail, json).await,
        },

        Command::Events(args) => match args.action {
            EventsAction::Contains { event_id, json } => {
                events_commands::run_contains(&home, event_id, json).await
            }
            EventsAction::List {
                kind,
                header,
                header_prefix,
                limit,
                json,
            } => events_commands::run_list(&home, kind, header, header_prefix, limit, json).await,
        },

        Command::Gist(args) => match args.action {
            GistAction::Get { path, default } => gist_commands::run_get(&path, &default),
            GistAction::GetJson { path } => gist_commands::run_get_json(&path),
            GistAction::GetFirstOf { paths, default } => {
                gist_commands::run_get_first_of(&paths, &default)
            }
            GistAction::PickAddr { scope } => gist_commands::run_pick_addr(&scope),
            GistAction::PickAddrFirst => gist_commands::run_pick_addr_first(),
            GistAction::PickAddrNonlocalFirst => gist_commands::run_pick_addr_nonlocal_first(),
            GistAction::PickAddrExcluding { exclude_scopes } => {
                gist_commands::run_pick_addr_excluding(&exclude_scopes)
            }
            GistAction::ListLanEntries => gist_commands::run_list_lan_entries(),
            GistAction::GistContent { channel } => gist_commands::run_gist_content(&channel),
            GistAction::FileContent { filename } => gist_commands::run_file_content(&filename),
        },

        Command::Join { room, ensure } => commands::run_join(&home, room, ensure).await,

        Command::Sos { action } => match action {
            SosAction::Send { message } => sos_commands::run_send(&home, &message).await,
            SosAction::Watch { follow } => sos_commands::run_watch(&home, follow).await,
            SosAction::Status => sos_commands::run_status(&home).await,
        },

        Command::Version => commands::run_version(),
        Command::StorePreflight => update_preflight::run(&home).await,

        #[cfg(windows)]
        Command::SetupRecoverElevatedOwner {
            probe,
            endpoint,
            caller_sid,
            installed_binary,
        } => {
            if probe {
                if elevated_owner::access_denied(&endpoint).await? {
                    std::process::exit(5);
                }
                Ok(())
            } else {
                elevated_owner::recover(&endpoint, &caller_sid, &installed_binary).await
            }
        }

        Command::IpcEndpoint { native } => {
            // Resolve-only: print the canonical socket path airc would
            // bind for this scope. No daemon required (callers probe
            // liveness via `status`/`ping`). This is the contract
            // Continuum's airc discovery depends on.
            let path = cli::default_socket_path_in(&home);
            if native {
                println!("{}", airc_ipc::transport::native_endpoint(&path));
            } else {
                println!("{}", path.display());
            }
            Ok(())
        }

        Command::Update {
            auto,
            revision,
            adopt_installed,
        } => {
            let socket = cli::default_socket_path_in(&home);
            if adopt_installed {
                update_commands::adopt_installed(&home, socket)
            } else {
                #[cfg(windows)]
                {
                    update_session::run(&home, auto, revision.as_deref())
                }
                #[cfg(not(windows))]
                {
                    if auto {
                        update_commands::run_update_auto(&home, socket, revision.as_deref())
                    } else {
                        update_commands::run_update(&home, socket, revision.as_deref())
                    }
                }
            }
        }

        Command::Doctor { fix, health } => doctor::run_doctor(&home, fix, health).await,

        Command::Gh(args) => match args.action {
            GhAction::Run { gh_args } => gh_commands::run_gh(gh_args),
            GhAction::PatchGistFile {
                gist_id,
                filename,
                content_file,
            } => gh_commands::run_patch_gist_file(&gist_id, &filename, &content_file),
            GhAction::WaitSeconds => {
                gh_commands::run_wait_seconds();
                Ok(())
            }
            GhAction::Audit {
                count,
                summary,
                reset,
                clear_audit,
            } => gh_commands::run_audit(count, summary, reset, clear_audit),
            GhAction::Doctor { count } => gh_commands::run_doctor(count),
        },

        Command::Handshake(args) => match args.action {
            HandshakeAction::Send {
                host,
                port,
                my_name,
                my_host,
                my_ssh_pub,
                my_sign_pub,
                my_x25519_pub,
                my_airc_home,
                my_identity_json,
            } => handshake_commands::run_send(
                &host,
                port,
                &my_name,
                &my_host,
                &my_ssh_pub,
                &my_sign_pub,
                &my_x25519_pub,
                &my_airc_home,
                &my_identity_json,
            ),
            HandshakeAction::AcceptOne {
                host_port,
                peers_dir,
                identity_dir,
                config,
                host_name,
                reminder_interval,
                airc_home,
                messages,
                watch_pid,
            } => handshake_commands::run_accept_one(
                host_port,
                &peers_dir,
                &identity_dir,
                &config,
                &host_name,
                reminder_interval,
                &airc_home,
                &messages,
                watch_pid,
            ),
        },

        Command::Hygiene(args) => hygiene_commands::run(args),

        Command::Knock(args) => match args.action {
            KnockAction::GenKeys => knock_commands::run_gen_keys(),
            KnockAction::EncryptForKnocker {
                knocker_pub,
                plaintext,
            } => knock_commands::run_encrypt_for_knocker(&knocker_pub, &plaintext),
            KnockAction::DecryptFromApprover {
                knocker_priv,
                approver_pub,
                nonce,
                ciphertext,
            } => knock_commands::run_decrypt_from_approver(
                &knocker_priv,
                &approver_pub,
                &nonce,
                &ciphertext,
            ),
            KnockAction::ApprovalField { field } => knock_commands::run_approval_field(&field),
            KnockAction::IdentityJson { name, state_dir } => {
                knock_commands::run_identity_json(&name, &state_dir)
            }
            KnockAction::ExtractKnockerPub => knock_commands::run_extract_knocker_pub(),
            KnockAction::ExtractApproval => knock_commands::run_extract_approval(),
        },

        Command::Pending(args) => match args.action {
            PendingAction::HostBroadcastRoute {
                snapshot,
                config,
                fallback_gist,
            } => pending_commands::run_host_broadcast_route(&snapshot, &config, &fallback_gist),
        },

        Command::CodexHook(args) => args.run(&home).await,
        Command::ClaudeHook(args) => args.run(&home).await,

        Command::CodexStart(args) => {
            integrations::codex::start::run(&args.airc, &args.home, &args.log, args.join_args).await
        }

        Command::Work(args) => args.run(&home).await,

        Command::Lane(args) => match args.action {
            LaneAction::Create { repo, title, state } => {
                lane_commands::run_create(&home, repo, title, state).await
            }
            LaneAction::State { lane_id, state } => {
                lane_commands::run_state(&home, lane_id, state).await
            }
            LaneAction::Status { limit: _ } => lane_commands::run_status(&home).await,
            LaneAction::Manager { action } => match action {
                LaneManagerAction::Claim { repo, ttl_ms } => {
                    lane_commands::run_manager_claim(&home, repo, ttl_ms).await
                }
                LaneManagerAction::Release { repo } => {
                    lane_commands::run_manager_release(&home, repo).await
                }
                LaneManagerAction::Status { limit: _ } => {
                    lane_commands::run_manager_status(&home).await
                }
            },
        },

        Command::WorktreeLane(args) => match args.action {
            WorktreeLaneAction::AbsPath { path } => worktree_lane_commands::run_abs_path(&path),
            WorktreeLaneAction::Slug { value } => {
                worktree_lane_commands::run_slug(&value);
                Ok(())
            }
            WorktreeLaneAction::Record {
                registry,
                issue,
                repo,
                dir,
                branch,
                base,
                owner,
            } => {
                worktree_lane_commands::run_record(&registry, issue, repo, dir, branch, base, owner)
            }
            WorktreeLaneAction::List { registry, json } => {
                worktree_lane_commands::run_list(&registry, json)
            }
            WorktreeLaneAction::Find {
                registry,
                target,
                field,
            } => worktree_lane_commands::run_find(&registry, &target, &field),
        },

        Command::AgentResume(args) => agent_resume::run(&home, args).await,

        Command::Monitor(args) => match args.action {
            MonitorAction::Format { peers_dir, my_name } => {
                monitor::run_format(&peers_dir, &my_name)
            }
            MonitorAction::Attach {
                my_name,
                from_now,
                include_backlog,
                coalesce_backlog,
            } => {
                let socket = default_or(None, &home);
                // Return-value ignored: this call site only uses the
                // daemon-side state (monitor::run_attach reads the
                // store, not the socket). If discovery routes to a
                // different socket, the home is what matters here.
                let _socket = commands::ensure_daemon_running(&home, socket, parsed.peers).await?;
                // Card 7d5b6a65: clap resolves --include-backlog as
                // the inverse of --from-now (they're conflicting
                // flags). Defaults: from_now=true. coalesce_backlog
                // defaults to true; flipped to false by user only
                // when they want event-by-event replay (audit case).
                let live_only = from_now && !include_backlog;
                // `my_name` is optional and unused by attach (the daemon
                // owns identity); accepted for backward compat.
                let my_name = my_name.unwrap_or_default();
                monitor::run_attach(&home, &my_name, live_only, coalesce_backlog).await
            }
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
            WorkspaceAction::List { limit: _ } => workspace_commands::run_list(&home).await,
        },

        Command::QueueCard(args) => args.run(),

        Command::Humanhash { hex_input, words } => {
            println!("{}", airc_core::humanhash(&hex_input, words)?);
            Ok(())
        }

        Command::ClientId => {
            let Some(value) = client_id::current_client_id()? else {
                return Err("client id is unavailable".into());
            };
            println!("{value}");
            Ok(())
        }

        Command::UuidV4 => {
            println!("{}", uuid::Uuid::new_v4());
            Ok(())
        }

        Command::IsoToEpoch { timestamp } => {
            println!("{}", airc_core::iso_to_epoch(&timestamp)?);
            Ok(())
        }
    }
}

/// Resolve `--socket` override to its value, falling back to the
/// home-derived default.
fn default_or(explicit: Option<PathBuf>, home: &Path) -> PathBuf {
    explicit.unwrap_or_else(|| cli::default_socket_path_in(home))
}
