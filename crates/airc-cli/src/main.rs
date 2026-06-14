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
mod staleness;
mod transport_cli;
mod transport_commands;
mod update_commands;
mod work_cli;
mod work_commands;
mod work_commands_gh;
mod work_commands_git;
mod work_suggestions;
mod workspace_cli;
mod workspace_commands;
mod worktree_lane_cli;
mod worktree_lane_commands;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::str::FromStr;

use clap::Parser;
use uuid::Uuid;

use airc_core::PeerId;

use airc_identity::LocalIdentity;
use channel_gist_cli::ChannelGistAction;
use cli::{Cli, Command, PeerAction};
use collaboration_cli::CollaborationAction;
use envelope_cli::EnvelopeAction;
use events_cli::EventsAction;
use gh_cli::GhAction;
use gist_cli::GistAction;
use handshake_cli::HandshakeAction;
use identity_cli::IdentityAction;
use integrations::codex::CodexHookAction;
use knock_cli::KnockAction;
use lane_cli::{LaneAction, LaneManagerAction};
use monitor::MonitorAction;
use pending_cli::PendingAction;
use queue_card_cli::QueueCardAction;
use route_cli::RouteAction;
use transport_cli::TransportAction;
use work_cli::WorkAction;
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
            eprintln!("airc: {error}");
            ExitCode::FAILURE
        }
    }
}

async fn dispatch(parsed: Cli) -> Result<(), Box<dyn std::error::Error>> {
    let home = parsed.home.clone().unwrap_or_else(cli::default_home_dir);

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
            CollaborationAction::Peers(args) => collaboration_peers::run_peers(&home, args),
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
                pronouns,
                role,
                bio,
                status,
            } => identity_commands::run_set(&home, pronouns, role, bio, status).await,
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
            commands::run_send(&home, parsed.peers, room.as_deref(), &text).await
        }

        Command::Listen { replay } => commands::run_listen(&home, parsed.peers, replay).await,

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
            let identity = LocalIdentity::load_or_generate(&home).await?;
            let socket = default_or(socket, &home);
            commands::run_daemon(&home, identity, parsed.peers, socket).await
        }

        Command::Ping { socket } => commands::run_ping(default_or(socket, &home)).await,
        Command::Status { socket } => commands::run_status(&home, default_or(socket, &home)).await,
        Command::Stop { socket } => commands::run_stop(default_or(socket, &home)).await,

        Command::Msg { socket, room, text } => {
            commands::run_msg(&home, default_or(socket, &home), room.as_deref(), &text).await
        }
        Command::Publish {
            room,
            body_text,
            body_json,
            headers,
            kind,
        } => publish_commands::run_publish(&home, room, body_text, body_json, headers, kind).await,

        Command::Inbox {
            socket,
            since_lamport,
            since_event_id,
            limit,
            json,
        } => commands::run_inbox(&home, socket, since_lamport, since_event_id, limit, json).await,

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
            PeerAction::Prune { apply } => commands::run_peer_prune(&home, apply).await,
        },

        Command::Peers => commands::run_peer_list(&home, false).await,

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

        Command::Join { room } => commands::run_join(&home, room).await,

        Command::Version => commands::run_version(),

        Command::Update => update_commands::run_update(&home, cli::default_socket_path_in(&home)),

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

        Command::CodexHook(args) => match args.action {
            CodexHookAction::InstallHooks { codex_home } => {
                integrations::codex::install::run_install_hooks(codex_home).await
            }
            CodexHookAction::UninstallHooks { codex_home } => {
                integrations::codex::install::run_uninstall_hooks(codex_home).await
            }
            CodexHookAction::UserPromptSubmit {
                count,
                max_items,
                raw,
                include_self,
            } => {
                integrations::codex::hook::run_user_prompt_submit(
                    &home,
                    count,
                    max_items,
                    raw,
                    include_self,
                )
                .await
            }
            CodexHookAction::Poll {
                count,
                max_items,
                raw,
                include_self,
                wait_ms,
            } => {
                integrations::codex::hook::run_poll(
                    &home,
                    count,
                    max_items,
                    raw,
                    include_self,
                    wait_ms,
                )
                .await
            }
        },

        Command::CodexStart(args) => {
            integrations::codex::start::run(&args.airc, &args.home, &args.log, args.join_args).await
        }

        Command::Work(args) => match args.action {
            WorkAction::Create {
                repo,
                title,
                body,
                lane_id,
                priority,
            } => work_commands::run_create(&home, repo, title, body, lane_id, priority).await,
            WorkAction::Seed {
                repo,
                title,
                body,
                lane_id,
                priority,
                evidence_key,
            } => {
                work_commands::run_seed(&home, repo, title, body, lane_id, priority, evidence_key)
                    .await
            }
            WorkAction::Claim {
                card_id,
                ttl_ms,
                no_lease_required,
            } => work_commands::run_claim(&home, card_id, ttl_ms, no_lease_required).await,
            WorkAction::Heartbeat {
                card_id,
                claim_id,
                ttl_ms,
            } => work_commands::run_heartbeat(&home, card_id, claim_id, ttl_ms).await,
            WorkAction::Release {
                card_id,
                claim_id,
                reason,
            } => work_commands::run_release(&home, card_id, claim_id, reason).await,
            WorkAction::Update {
                card_id,
                title,
                body,
                priority,
            } => work_commands::run_update(&home, card_id, title, body, priority).await,
            WorkAction::State { card_id, state } => {
                work_commands::run_state(&home, card_id, state).await
            }
            WorkAction::Close { card_id } => work_commands::run_close(&home, card_id).await,
            WorkAction::Cleanup { dry_run, force } => {
                work_commands::run_cleanup(&home, dry_run, force).await
            }
            WorkAction::Board {
                limit,
                available,
                mine,
                others,
            } => {
                let filter = work_commands::BoardFilter::from_flags(available, mine, others);
                work_commands::run_board(&home, limit, filter).await
            }
            WorkAction::Next {
                repo,
                max_priority,
                exclude_stale,
                limit,
                event_limit,
                check_idle,
            } => {
                work_commands::run_next(
                    &home,
                    repo,
                    max_priority,
                    !exclude_stale,
                    limit,
                    event_limit,
                    check_idle,
                )
                .await
            }
            WorkAction::Roster {
                repo,
                event_limit,
                active_within_ms,
            } => work_commands::run_roster(&home, repo, event_limit, active_within_ms).await,
            WorkAction::Manage {
                repo,
                max_priority,
                exclude_stale,
                limit,
                event_limit,
                active_within_ms,
            } => {
                work_commands::run_manage(
                    &home,
                    repo,
                    max_priority,
                    !exclude_stale,
                    limit,
                    event_limit,
                    active_within_ms,
                )
                .await
            }
            WorkAction::Review {
                parent_id,
                pr,
                priority,
                body,
            } => work_commands::run_review(&home, parent_id, pr, priority, body).await,
            WorkAction::Availability {
                repo,
                state,
                note,
                ttl_ms,
            } => work_commands::run_availability(&home, repo, state, note, ttl_ms).await,
            WorkAction::Merger { action } => match action {
                work_cli::MergerAction::Run {
                    interval_secs,
                    dry_run,
                } => {
                    merger::run(
                        &home,
                        std::time::Duration::from_secs(interval_secs),
                        dry_run,
                    )
                    .await
                }
            },
            WorkAction::Merge {
                card_id,
                dry_run,
                pending_timeout_secs,
            } => work_commands::run_merge(&home, card_id, dry_run, pending_timeout_secs).await,
            WorkAction::Link { card_id, pr } => work_commands::run_link(&home, card_id, pr).await,
            WorkAction::Relink { card_id, pr } => {
                work_commands::run_relink(&home, card_id, pr).await
            }
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
            WorkspaceAction::List { limit } => workspace_commands::run_list(&home, limit).await,
        },

        Command::QueueCard(args) => match args.action {
            QueueCardAction::Body {
                id,
                branch,
                owner,
                status,
                blockers,
                environment,
                evidence,
                next_action,
                last_heartbeat,
            } => queue_card_commands::run_body(queue_card_commands::QueueCardInput {
                id,
                branch,
                owner,
                status,
                blockers,
                environment,
                evidence,
                next_action,
                last_heartbeat,
            }),
            QueueCardAction::MutateBody {
                body_file,
                mutations_file,
                log_msg,
                timestamp,
            } => queue_card_commands::run_mutate_body(
                &body_file,
                &mutations_file,
                &log_msg,
                &timestamp,
            ),
            QueueCardAction::ClaimFields { body_file } => {
                queue_card_commands::run_claim_fields(&body_file)
            }
            QueueCardAction::DispatchMessage {
                target_agent,
                extra_message,
                next_json_file,
            } => queue_card_commands::run_dispatch_message(
                &target_agent,
                &extra_message,
                &next_json_file,
            ),
            QueueCardAction::AdoptBody {
                issue_json_file,
                queue_body_file,
                force,
            } => queue_card_commands::run_adopt_body(&issue_json_file, &queue_body_file, force),
            QueueCardAction::NudgeSummary { raw_json_file } => {
                queue_card_commands::run_nudge_summary(&raw_json_file)
            }
            QueueCardAction::NudgeCardMeta { issue_file } => {
                queue_card_commands::run_nudge_card_meta(&issue_file)
            }
            QueueCardAction::List {
                repo,
                owner,
                status,
                json,
                raw_json_file,
            } => queue_card_projection::run_list(&repo, &owner, &status, json, &raw_json_file),
            QueueCardAction::Stale {
                repo,
                stale_after,
                json,
                raw_json_file,
            } => queue_card_projection::run_stale(&repo, &stale_after, json, &raw_json_file),
            QueueCardAction::Next {
                repo,
                owner,
                base,
                repo_root,
                json,
                raw_json_file,
            } => queue_card_projection::run_next(
                &repo,
                &owner,
                &base,
                &repo_root,
                json,
                &raw_json_file,
            ),
            QueueCardAction::Pongs {
                repo,
                sweep_id,
                since,
                json,
                cards_file,
                messages_file,
            } => queue_card_runtime::run_pongs(
                &repo,
                &sweep_id,
                &since,
                json,
                &cards_file,
                &messages_file,
            ),
            QueueCardAction::Availability {
                repo,
                sweep_id,
                since,
                stale_after,
                json,
                cards_file,
                messages_file,
            } => queue_card_runtime::run_availability(
                &repo,
                &sweep_id,
                &since,
                &stale_after,
                json,
                &cards_file,
                &messages_file,
            ),
            QueueCardAction::ReviewRefs {
                repo,
                raw_json_file,
            } => queue_card_staleness::run_review_refs(&repo, &raw_json_file),
            QueueCardAction::PrMeta { pr_file } => queue_card_staleness::run_pr_meta(&pr_file),
            QueueCardAction::StalenessAnalyze {
                repo_root,
                pr_repo,
                pr_num,
                base_ref,
                head_ref,
                base_git_ref,
                head_git_ref,
                merge_base,
                pr_url,
                limit_lines,
                json,
                files_file,
                diff_file,
                base_new_file,
            } => queue_card_staleness::run_staleness_analyze(
                queue_card_staleness::StalenessAnalyzeInput {
                    repo_root: &repo_root,
                    pr_repo: &pr_repo,
                    pr_num: &pr_num,
                    base_ref: &base_ref,
                    head_ref: &head_ref,
                    base_git_ref: &base_git_ref,
                    head_git_ref: &head_git_ref,
                    merge_base: &merge_base,
                    pr_url: &pr_url,
                    limit: limit_lines,
                    output_json: json,
                    files_file: &files_file,
                    diff_file: &diff_file,
                    base_new_file: &base_new_file,
                },
            ),
            QueueCardAction::CloseMergedMeta { pr_file } => {
                queue_card_commands::run_close_merged_meta(&pr_file)
            }
            QueueCardAction::CloseMergedRefs { pr_file, repo } => {
                queue_card_commands::run_close_merged_refs(&pr_file, &repo)
            }
            QueueCardAction::CardStatus { body_file } => {
                queue_card_commands::run_card_status(&body_file)
            }
            QueueCardAction::Plan {
                repo,
                owner,
                stale_after,
                json,
                raw_json_file,
            } => queue_card_plan::run_plan(&repo, &owner, &stale_after, json, &raw_json_file),
            QueueCardAction::Steward {
                repo,
                owner,
                stale_after,
                json,
                raw_json_file,
            } => queue_card_plan::run_steward(&repo, &owner, &stale_after, json, &raw_json_file),
        },

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
