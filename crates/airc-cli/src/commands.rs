//! Subcommand handlers.
//!
//! Local-substrate commands (`init`, `send`, `listen`, `room`,
//! `peer add`, `peer list`) route through `airc_lib::Airc` — the
//! CLI is a thin client of the same API consumers embed. Closes
//! grievance §5 / Codex audit finding #4.
//!
//! Daemon-host commands construct daemon state directly because they
//! host the service. CLI commands that consume daemon-backed messaging
//! go through `airc_lib::Airc::attach` so apps and CLI share the same
//! SDK surface.
//!
//! `VerificationPolicy::Strict` is the only policy used in CLI paths.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use airc_core::{ClientId, EventId, PeerId, TranscriptCursor};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy, HEADER_AIRC_CLIENT};
use futures::stream::StreamExt;

use airc_daemon::{run as run_daemon_server, DaemonRuntimeInfo, DaemonState};
use airc_identity::LocalIdentity;
use airc_ipc::{AddPeerRequest, DaemonClient, RemovePeerRequest, Request, Response};
use airc_lib::{Airc, Headers, HeartbeatTask, PeerSpec, DEFAULT_HEARTBEAT_INTERVAL};
use airc_store::{EventStore, SqliteEventStore};
use airc_trust as peers_store;

/// `init` — open the substrate at `<home>`. `Airc::open` loads or
/// generates the identity, opens the event store, applies any
/// pending migrations, and primes the peer registry. The CLI then
/// prints the local peer's spec so the user can share it.
pub async fn run_init(
    home: &Path,
    agent_name: Option<String>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = match agent_name {
        Some(agent_name) => Airc::open_as(home, agent_name).await?,
        None => Airc::open(home).await?,
    };
    let current = airc.current_room().await?;
    println!("home:        {}", airc.home().display());
    println!("peer_id:     {}", airc.peer_id());
    println!("client_id:   {}", airc.client_id());
    println!("agent_name:  {}", airc.agent_name());
    println!("room:        {} ({})", current.name, current.channel);
    println!("peer_spec:   {}", airc.peer_spec());
    println!();
    println!(
        "Share peer_spec with peers; enrol theirs via `airc peer add <spec>`. \
         Use `airc room <name>` to switch rooms; `airc msg \"hi\"` sends \
         to the current room."
    );
    Ok(())
}

/// `room` — print current room. `room <name>` — switch to a
/// deterministic room derived from `<name>`.
pub async fn run_room(home: &Path, name: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    match name {
        Some(name) => {
            let next = airc.join(&name).await?;
            println!("switched room: {}", next.name);
            println!("  wire:    {}", next.wire.display());
            println!("  channel: {}", next.channel);
        }
        None => {
            let current = airc.current_room().await?;
            println!("room:    {}", current.name);
            println!("wire:    {}", current.wire.display());
            println!("channel: {}", current.channel);
        }
    }
    Ok(())
}

/// `doctrine-publish` — read a markdown file (default: AGENTS.md at
/// the git repo root) and publish it as the room's operating
/// doctrine via `Airc::publish_room_doctrine`. Card 2903a8ef slice
/// 2/4 of the engine keystone — gets the "how we work here" contract
/// onto the substrate so attaching agents in any scope load it.
///
/// Version: short SHA-256 prefix of the body bytes. Future tooling
/// can compare versions to detect "doctrine on my scope is stale."
pub async fn run_doctrine_publish(
    home: &Path,
    from_file: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    // Resolve the source path. Default: `<git-repo-root>/AGENTS.md`.
    let path = match from_file {
        Some(p) => p,
        None => {
            let repo_root = std::process::Command::new("git")
                .args(["rev-parse", "--show-toplevel"])
                .output()?;
            if !repo_root.status.success() {
                return Err(format!(
                    "no --from-file passed and git rev-parse --show-toplevel \
                     failed (not in a git repo?): {}",
                    String::from_utf8_lossy(&repo_root.stderr).trim()
                )
                .into());
            }
            let root = String::from_utf8(repo_root.stdout)?.trim().to_string();
            PathBuf::from(root).join("AGENTS.md")
        }
    };
    let body = std::fs::read_to_string(&path)
        .map_err(|e| format!("cannot read doctrine file {}: {e}", path.display()))?;

    let version = short_content_hash(body.as_bytes());

    let socket = crate::cli::default_socket_path_in(home);
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    let airc = Airc::attach(home, socket).await?;
    airc.publish_room_doctrine(body.clone(), version.clone())
        .await?;
    println!(
        "doctrine_published: file={file} version={version} bytes={bytes}",
        file = path.display(),
        bytes = body.len(),
    );
    Ok(())
}

/// Short content discriminator — first 12 chars of SHA-256 hex of
/// `body`. Twelve chars are enough to distinguish unrelated revisions
/// of a kilobyte-scale doctrine file (the AGENTS.md target) without
/// pulling in a heavier hash; collisions at this scale are
/// astronomically unlikely and the substrate stores every event so a
/// "version" collision degrades to "two latest with the same tag,"
/// not data loss.
fn short_content_hash(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let hex: String = digest.iter().map(|b| format!("{:02x}", b)).collect();
    hex.chars().take(12).collect()
}

/// `part` — leave a subscribed room without deleting identity, trust,
/// or other room subscriptions.
pub async fn run_part(home: &Path, room: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let parted = airc.part_channel(room.as_deref()).await?;
    println!("parted:  #{}", parted.name);
    println!("channel: {}", parted.channel);
    Ok(())
}

/// `join` — account-room coordinator entrypoint. With no explicit
/// room, subscribe to `#general` plus the inferred Git owner channel.
/// With a room, join that arbitrary channel and make it default.
pub async fn run_join(home: &Path, room: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    // Start the machine-singular daemon and attach: join, heartbeat, and
    // the live feed all route through the daemon's router (one path).
    let socket = crate::cli::default_socket_path_in(home);
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    let airc = Airc::attach(home, socket.clone()).await?;
    let runtime_context = crate::runtime_context::RuntimeContext::current();
    match room {
        Some(room) => {
            let joined = airc.join(&room).await?;
            println!("joined:  #{}", joined.name);
            println!("wire:    {}", joined.wire.display());
            println!("channel: {}", joined.channel);
            print_scope_context(home, &joined.wire);
        }
        None => {
            let cwd = std::env::current_dir()?;
            let rooms = airc.join_default_context(cwd).await?;
            let current = airc.current_room().await?;
            println!("joined default account context:");
            for room in rooms {
                println!("  #{} ({})", room.name, room.channel);
            }
            println!("default: #{}", current.name);
            println!("wire:    {}", current.wire.display());
            print_scope_context(home, &current.wire);
        }
    }
    sync_daemon_peers_for_current_rooms(home, socket).await?;
    ensure_runtime_integrations();

    // Card 745e93f0 (slice 4/4 of engine-keystone 2903a8ef): surface
    // the room's operating doctrine to the attaching agent. Agent
    // runner harnesses scrape this region from join stdout and
    // inject it into the agent's system context — the "user is not
    // the engine" fix lands here. Marked with stable BEGIN/END
    // markers so the scrape is unambiguous; silent when the room
    // has no published doctrine.
    if let Ok(Some(doctrine)) = airc.room_doctrine().await {
        println!("--- BEGIN ROOM DOCTRINE (version={}) ---", doctrine.version);
        println!("{}", doctrine.body);
        println!("--- END ROOM DOCTRINE ---");
    }

    let _heartbeat = if runtime_context.should_stream_join() {
        Some(start_join_heartbeat(&airc, home, &runtime_context).await?)
    } else {
        None
    };

    if runtime_context.should_stream_join() {
        crate::join_feed::run(&airc).await?;
    }
    Ok(())
}

async fn start_join_heartbeat(
    airc: &Airc,
    home: &Path,
    runtime_context: &crate::runtime_context::RuntimeContext,
) -> Result<HeartbeatTask, Box<dyn std::error::Error>> {
    let scope = join_scope_label(home);
    let runtime = runtime_context.runtime_label().to_string();
    let client_id = runtime_context.client_id().map(ToString::to_string);
    let build = (!crate::build_info::is_unknown()).then(|| crate::build_info::COMMIT_SHORT.into());

    // Card 0bf262eb: populate the coordination signal added in
    // aacf2162. This is the minimum-viable slice — the build SHA
    // stands in for `doctrine_version` (the build tree includes
    // AGENTS.md, so observers can still detect peers on stale
    // doctrine), and the other two fields stay default. A follow-up
    // card refreshes `active_claims` from the board projection on
    // every tick.
    let coordination = airc_lib::CoordinationSignal {
        doctrine_version: build.clone(),
        ..Default::default()
    };

    Ok(airc
        .start_agent_heartbeat_with_coordination(
            runtime,
            client_id,
            Some(scope),
            build,
            DEFAULT_HEARTBEAT_INTERVAL,
            coordination,
        )
        .await?)
}

fn join_scope_label(home: &Path) -> String {
    match std::env::current_dir() {
        Ok(cwd) => cwd.display().to_string(),
        Err(_) => home.display().to_string(),
    }
}

fn ensure_runtime_integrations() {
    match crate::integrations::codex::install::install_hooks_for_default_home_if_present() {
        Ok(report) if report.is_empty() => {}
        Ok(report) => {
            for line in report.lines {
                println!("runtime: {line}");
            }
        }
        Err(error) => {
            eprintln!("airc: Codex hook setup skipped: {error}");
        }
    }
}

/// `version` — print package version + install dir. Distinct from
/// clap's `--version` flag (which only prints the package version)
/// because operators use `airc version` to verify two scopes/tabs
/// are on the same build path, not just the same version string.
///
/// Richer build metadata (commit sha, branch, commit subject) is a
/// follow-up — would need a `build.rs` that captures git state at
/// compile time. For now: package version + binary path is enough
/// to distinguish "are we on the same install."
pub fn run_version() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let exe_path = exe.canonicalize().unwrap_or(exe);
    println!("  airc {}", crate::build_info::PACKAGE_VERSION);
    println!("  install: {}", exe_path.display());
    if !crate::build_info::is_unknown() {
        println!(
            "  build:   {} on {}",
            crate::build_info::COMMIT_SHORT,
            crate::build_info::BRANCH
        );
    } else {
        println!("  build:   unknown (git unavailable at compile time)");
    }
    Ok(())
}

/// Make sure a daemon serving `home` is reachable; return the socket
/// the caller should attach to.
///
/// The returned socket is USUALLY equal to `socket` — every agent
/// resolving the same `home` finds the existing daemon on the same
/// socket. For sandboxed agents (Codex etc.) whose home-resolved
/// socket has no daemon, the cross-sandbox discovery directory
/// (card 282850c2) is consulted and we route to the project's
/// actual daemon instead of spawning a competing orphan in the
/// agent's tmpdir. If neither path finds an existing daemon, a fresh
/// one is spawned at `socket` and announced for future agents.
pub async fn ensure_daemon_running(
    home: &Path,
    socket: PathBuf,
    _peers: Vec<PeerSpec>,
) -> Result<PathBuf, Box<dyn std::error::Error>> {
    // 1. Fast path: home-resolved socket already has a current daemon.
    let client = DaemonClient::new(socket.clone());
    if let Ok(status) = client.status_with_timeout(Duration::from_millis(250)).await {
        if daemon_status_is_current(&status) {
            return Ok(socket);
        }
        let _ = client.stop().await;
        wait_for_daemon_exit(&client, Duration::from_secs(3)).await;
    }

    // 2. Card 282850c2: no daemon at the home-resolved socket. Before
    // spawning, consult cross-sandbox discovery for a daemon serving
    // the SAME project root. Sandboxed agents whose `$HOME` was
    // forced into a tmpdir would otherwise orphan a fresh daemon
    // every invocation. We only auto-attach when the discovered
    // daemon's `home` matches ours — different homes mean different
    // identities, and attaching across them would silently borrow
    // the wrong agent's keys (card a1b4552a was the prior class of
    // this kind of attribution leak). In practice the Codex case
    // DOES match: both agents resolve home from the same project
    // root, so the home values agree.
    let project_root = home.parent().map(Path::to_path_buf);
    if let Some(ref pr) = project_root {
        if let Some(discovered) = crate::discovery::find_for_project(pr) {
            if discovered.home == home {
                let alt = DaemonClient::new(discovered.socket.clone());
                if let Ok(status) = alt.status_with_timeout(Duration::from_millis(250)).await {
                    if daemon_status_is_current(&status) {
                        return Ok(discovered.socket);
                    }
                }
            }
        }
    }

    // 3. Spawn a fresh daemon at the home-resolved socket.
    std::fs::create_dir_all(home)?;
    let log = home.join("airc-daemon.log");
    let stdout = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)?;
    let stderr = stdout.try_clone()?;
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--home")
        .arg(home)
        .arg("daemon")
        .arg("--socket")
        .arg(&socket)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    detach_daemon(&mut command);
    let child = command.spawn()?;
    let daemon_pid = child.id();

    let client = DaemonClient::new(socket.clone());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if client
            .ping_with_timeout(Duration::from_millis(250))
            .await
            .is_ok()
        {
            // 4. Card 282850c2: announce so a sandboxed agent
            // attaching later finds this daemon instead of orphaning
            // a new one. Best-effort — if the announcement fails,
            // the normal singleton-per-home model still works.
            announce_running_daemon(home, &socket, project_root.as_deref(), daemon_pid).await;
            return Ok(socket);
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("daemon did not become ready; see {}", log.display()).into())
}

/// Read `peer_id`/`build` from the freshly-ready daemon and write a
/// discovery entry. Called from `ensure_daemon_running` after the
/// readiness ping succeeds; failure is silent.
async fn announce_running_daemon(
    home: &Path,
    socket: &Path,
    project_root: Option<&Path>,
    pid: u32,
) {
    let client = DaemonClient::new(socket.to_path_buf());
    let Ok(status) = client.status_with_timeout(Duration::from_millis(250)).await else {
        return;
    };
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let entry = crate::discovery::DiscoveredDaemon {
        socket: socket.to_path_buf(),
        home: home.to_path_buf(),
        project_root: project_root.map(Path::to_path_buf),
        peer_id: status.peer_id,
        pid,
        started_at_ms: now_ms,
        build: status.build_commit.unwrap_or_else(|| "unknown".to_string()),
    };
    let _ = crate::discovery::announce(&entry);
}

fn daemon_status_is_current(status: &airc_ipc::StatusResponse) -> bool {
    status.ipc_protocol_version == Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION))
        && crate::build_info::is_unknown_or_matches(status.build_commit.as_deref())
}

async fn wait_for_daemon_exit(client: &DaemonClient, max_wait: Duration) {
    let deadline = Instant::now() + max_wait;
    while Instant::now() < deadline {
        if client
            .ping_with_timeout(Duration::from_millis(150))
            .await
            .is_err()
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[cfg(unix)]
fn detach_daemon(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    // SAFETY: this closure runs in the child just before exec and
    // only calls setsid, which is async-signal-safe.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(windows)]
fn detach_daemon(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x00000008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x00000200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

/// Push the current scope's peer trust into the running daemon's
/// in-memory registry. In the owner-core there is no per-wire subscribe:
/// the one machine daemon already routes every channel through its
/// `EventRouter`, so a scope just needs the daemon to know its peers
/// (for cross-machine verify), nothing more.
async fn sync_daemon_peers_for_current_rooms(
    home: &Path,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let client = DaemonClient::new(socket);
    let set = airc.subscription_set().await?;
    sync_daemon_peers(&client, home, &set).await?;
    Ok(())
}

async fn sync_daemon_peers(
    client: &DaemonClient,
    home: &Path,
    set: &airc_lib::SubscriptionSet,
) -> Result<(), Box<dyn std::error::Error>> {
    sync_daemon_peers_from_store(client, home).await?;
    for subscription in set.all() {
        if let Some(wire_root) = subscription
            .as_room()
            .wire
            .parent()
            .and_then(|path| path.parent())
        {
            if wire_root != home {
                sync_daemon_peers_from_store(client, wire_root).await?;
            }
        }
    }
    Ok(())
}

async fn sync_daemon_peers_from_store(
    client: &DaemonClient,
    home: &Path,
) -> Result<(), Box<dyn std::error::Error>> {
    for peer in peers_store::load(home).await? {
        client
            .add_peer(AddPeerRequest {
                peer_id: peer.peer_id,
                pubkey_b64: peer.pubkey_b64,
            })
            .await?;
    }
    Ok(())
}

/// Tell the operator which scope they actually joined and whether
/// it's sharing the machine-account wire or running isolated. The
/// substrate already routes correctly; this is purely diagnostic so
/// `airc join` from a project dir doesn't leave anyone wondering
/// "am I on the same mesh as my HOME tabs?"
///
/// Codex's criterion #2: "airc join from a project scope must
/// converge onto the same usable account mesh or clearly show which
/// scope it is using."
fn print_scope_context(home: &Path, wire: &Path) {
    // wire = <wire_root>/wires/<channel> → wire_root is two parents up.
    let wire_root = wire
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.to_path_buf());
    let scope = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let wire_root_canon = wire_root
        .as_ref()
        .map(|p| p.canonicalize().unwrap_or_else(|_| p.clone()));
    let canonical_account_home = canonical_machine_account_home();
    println!("scope:   {}", scope.display());
    match (&wire_root_canon, &canonical_account_home) {
        // Scope IS the canonical $HOME/.airc machine-account home.
        // It IS the wire root by definition — but that's the
        // intended "everybody on this machine routes here" home,
        // NOT a "project-local isolated" scope. Label accordingly.
        (Some(root), Some(account_home)) if root == &scope && &scope == account_home => {
            println!(
                "mesh:    machine-account home (this is the canonical `{}` — all scopes on this user's machine route here)",
                scope.display()
            );
        }
        // Scope is its own wire root AND not the canonical machine-
        // account home — genuinely isolated (tempdirs, CI harnesses,
        // explicit AIRC_HOME=/tmp/... overrides).
        (Some(root), _) if root == &scope => {
            println!(
                "mesh:    project-local (this scope's identity AND wire live in `{}` — sends are isolated to this dir)",
                scope.display()
            );
        }
        // Scope is a subdir under $HOME but the wire is promoted up
        // to $HOME/.airc — the common "agent ran airc join from a
        // project" case.
        (Some(root), _) => {
            println!(
                "mesh:    machine-account (this scope shares wire at `{}` with every other scope on this user's machine)",
                root.display()
            );
        }
        (None, _) => {
            println!("mesh:    unknown (could not resolve wire root)");
        }
    }
}

fn canonical_machine_account_home() -> Option<PathBuf> {
    let user_home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    let user_home_canon = user_home.canonicalize().unwrap_or(user_home);
    Some(user_home_canon.join(".airc"))
}

/// `send` — local-fs single-shot send to the current room. Routes
/// through `Airc::say`; ad-hoc `--peer` flags are enrolled in the
/// in-process registry for the duration of the invocation.
/// Open an `Airc` attached to this machine's singular daemon, starting
/// it if needed. Same-machine send/read/subscribe route through the
/// daemon's router — the only same-machine path (no more `frames.jsonl`).
pub(crate) async fn attached_airc(home: &Path) -> Result<Airc, Box<dyn std::error::Error>> {
    let socket = crate::cli::default_socket_path_in(home);
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    Ok(Airc::attach(home, socket).await?)
}

pub async fn run_send(
    home: &Path,
    peers: Vec<PeerSpec>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = attached_airc(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    let current = airc.current_room().await?;
    airc.say_with_headers(text, runtime_headers()?).await?;
    let peer_count = airc.peers().await?.len();
    // `Airc::say` returned Ok — the frame is signed, persisted to the
    // local store, and written to the wire. Any scope tailing this
    // channel's wire will receive it. The peer-registry count
    // (`peers()`) reflects cryptographically-paired *remote* peers;
    // a count of 0 does NOT mean "no readers." Two scopes on the
    // same machine share the wire via the account-home convention
    // and deliver to each other without any peer enrollment.
    //
    // The previous "stored locally — not delivered to another
    // agent" wording was a lie in exactly that case (caught by
    // Codex's criterion #3): the message DID deliver to same-
    // machine same-HOME tailers. Replace with a description that
    // matches what actually happened.
    if peer_count == 0 {
        println!(
            "sent to {} ({}). 0 paired remote peers; any scope tailing this channel on this machine will receive it.",
            current.name, current.channel
        );
    } else {
        println!(
            "sent to {} ({}) — {peer_count} paired peer(s) + any local scope tailing this channel.",
            current.name, current.channel
        );
    }
    Ok(())
}

/// `listen` — subscribe to live events on the current room and
/// print them until Ctrl-C. Routes through `Airc::subscribe`. The
/// underlying wire subscriber is replay-anchored: existing frames
/// on the wire are replayed through the broadcast first, then live
/// events flow. `--replay` is accepted for compatibility but the
/// distinction is no longer load-bearing — the substrate always
/// gives consumers the full transcript.
pub async fn run_listen(
    home: &Path,
    peers: Vec<PeerSpec>,
    _replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = attached_airc(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    let current = airc.current_room().await?;
    println!(
        "listening on {} ({}, peer_id {}) …",
        current.name,
        current.wire.display(),
        airc.peer_id()
    );

    // Subscribe creates the live receiver BEFORE spawning the wire
    // subscriber (see `Airc::subscribe`), so pre-existing frames
    // on the wire flow through this stream without race-loss.
    let mut stream = airc.subscribe().await?;
    print_event_stream_until_signal(&mut stream).await
}

/// `lan-send` — TLS-wrapped single-shot send to a remote peer, on
/// the current room's channel.
pub async fn run_lan_send(
    home: &Path,
    peers: Vec<PeerSpec>,
    to: std::net::SocketAddr,
    expected_peer: PeerId,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    let current = airc.current_room().await?;
    airc.connect_lan(to, expected_peer).await?;
    airc.say_with_headers(text, runtime_headers()?).await?;
    println!(
        "sent over lan-tcp to {} ({}).",
        current.name, current.channel
    );
    Ok(())
}

/// `lan-listen` — bind a TLS server, accept peers, print frames.
pub async fn run_lan_listen(
    home: &Path,
    peers: Vec<PeerSpec>,
    bind: std::net::SocketAddr,
    _replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    for peer in &peers {
        airc.enrol_volatile_peer(peer)?;
    }
    // Subscribe BEFORE binding the listener. `listen_lan` starts the LAN
    // frame-ingest task, which fans each received frame into `live_tx`
    // (see `append_received_frame`). `subscribe()` is a live broadcast
    // receiver with no backlog for a not-yet-created subscriber, and
    // `lan-listen` does not replay the store — so a frame that arrives in
    // the gap between bind and subscribe is fanned out to no receiver and
    // lost to this consumer (still persisted, just never printed).
    // Creating the receiver first guarantees it predates any ingested
    // frame, closing an intermittent CI frame-drop ("listener did not
    // print the message"). subscribe() does not depend on the listener
    // being bound.
    let mut stream = airc.subscribe().await?;
    let actual = airc.listen_lan(bind).await?;
    println!("listening on {actual} (peer_id {}) …", airc.peer_id());
    print_event_stream_until_signal(&mut stream).await
}

/// `daemon` — run the long-lived daemon process on the given socket.
pub async fn run_daemon(
    home: &Path,
    identity: LocalIdentity,
    peers: Vec<PeerSpec>,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Card 800ce5bd: install a tracing subscriber so the existing
    // `tracing::warn!` / `tracing::info!` calls in airc-bus, airc-lib,
    // airc-relay, etc. actually emit. Before this, every tracing call
    // in the workspace was a no-op (no subscriber registered) — load-
    // bearing diagnostics had nowhere to land. `RUST_LOG=info` turns on
    // the fan-out + subscribe instrumentation; default `warn` filter
    // keeps the daemon quiet at steady state. `set_global_default`
    // failures are ignored (a re-run inside the same process shouldn't
    // crash — e.g. in-process tests sharing the daemon entry).
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn"));
    let subscriber = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .finish();
    let _ = tracing::subscriber::set_global_default(subscriber);

    let registry = build_combined_registry(home, &identity, &peers).await?;

    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // ONE ORM per machine account (§3.3). The daemon is the single
    // owner: every scope under this user's `$HOME` resolves the same
    // machine-account home, so they share one `events.sqlite` — the
    // router's durable transcript + persisted epoch, and the coordinator
    // store's subscriptions / beacons / identity. No per-scope store.
    let machine_home = airc_lib::machine_account_home(home);
    std::fs::create_dir_all(&machine_home)?;
    let db_path = machine_home.join("events.sqlite");
    let coordinator_store: Arc<dyn EventStore> =
        Arc::new(SqliteEventStore::open_path(&db_path).await?);
    let state = Arc::new(
        DaemonState::build(
            identity.peer_id,
            identity.keypair,
            registry,
            VerificationPolicy::Strict,
            machine_home,
            &db_path,
            coordinator_store,
            current_daemon_runtime_info(),
        )
        .await?,
    );
    println!(
        "airc daemon: peer_id={} listening on {}",
        identity.peer_id,
        socket.display()
    );
    run_daemon_server(state, socket).await?;
    println!("airc daemon: stopped.");
    Ok(())
}

fn current_daemon_runtime_info() -> DaemonRuntimeInfo {
    DaemonRuntimeInfo {
        ipc_protocol_version: Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION)),
        build_commit: (!crate::build_info::is_unknown()).then(|| crate::build_info::COMMIT.into()),
        build_branch: (!crate::build_info::is_unknown()).then(|| crate::build_info::BRANCH.into()),
        executable: std::env::current_exe()
            .ok()
            .map(|path| path.display().to_string()),
    }
}

// ---- Daemon-client commands (no identity load needed) ---------------

pub async fn run_ping(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.ping().await?;
    println!("pong");
    Ok(())
}

/// `status` — daemon health snapshot.
///
/// Card 2bdae532: regression-fix. Earlier builds auto-spawned the
/// daemon if the socket wasn't reachable, so `airc status` doubled as
/// a "make the daemon ready" command. The current binary had lost
/// that, so a fresh attach (cargo install then airc status) failed
/// with "daemon not reachable: No such file or directory" with no
/// next step — Codex hit this on first onboard 2026-05-28. Restoring
/// `ensure_daemon_running` before the probe gives every recipe that
/// says "run `airc status` first" a working contract again.
pub async fn run_status(home: &Path, socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    let client = DaemonClient::new(socket);
    let status = client.status().await?;
    println!("peer_id:        {}", status.peer_id);
    println!("uptime_seconds: {}", status.uptime_seconds);
    if let Some(version) = status.ipc_protocol_version {
        println!("ipc_protocol:   {version}");
    }
    if let Some(commit) = status.build_commit.as_deref() {
        let short = &commit[..commit.len().min(12)];
        println!("build:          {short}");
    }
    if let Some(branch) = status.build_branch.as_deref() {
        println!("branch:         {branch}");
    }
    if let Some(executable) = status.executable.as_deref() {
        println!("executable:     {executable}");
    }
    Ok(())
}

pub async fn run_stop(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.stop().await?;
    println!("daemon: stop requested.");
    Ok(())
}

pub async fn run_msg(
    home: &Path,
    socket: PathBuf,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
    sync_daemon_peers_for_current_rooms(home, socket.clone()).await?;
    let airc = Airc::attach(home, socket).await?;
    let current = airc.current_room().await?;
    airc.say_with_headers(text, runtime_headers()?).await?;
    let peer_count = airc.peers().await?.len();
    // See run_send for the rationale — same message-honesty fix
    // for the daemon-attached send path.
    if peer_count == 0 {
        println!(
            "sent to {} ({}). 0 paired remote peers; any scope tailing this channel on this machine will receive it.",
            current.name, current.channel
        );
    } else {
        println!(
            "sent to {} ({}) — {peer_count} paired peer(s) + any local scope tailing this channel.",
            current.name, current.channel
        );
    }
    Ok(())
}

pub async fn run_inbox(
    home: &Path,
    socket: Option<PathBuf>,
    since_lamport: Option<u64>,
    since_event_id: Option<String>,
    limit: Option<usize>,
    as_json: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = match socket {
        Some(socket) => {
            let socket = ensure_daemon_running(home, socket, Vec::new()).await?;
            Airc::attach(home, socket).await?
        }
        None => attached_airc(home).await?,
    };
    // Both --since-lamport and --since-event-id must be supplied
    // together; the cursor is a tuple per grievance §7.
    let since = match (since_lamport, since_event_id) {
        (Some(lamport), Some(ref ev)) => Some(TranscriptCursor {
            lamport,
            event_id: EventId::from_uuid(uuid::Uuid::parse_str(ev)?),
        }),
        (None, None) => None,
        _ => {
            return Err(
                "--since-lamport and --since-event-id must be passed together (cursor is a tuple)"
                    .into(),
            );
        }
    };
    let effective_limit = limit.unwrap_or(32);
    let events = match since {
        Some(cursor) => airc.resume_from(&cursor, effective_limit).await?,
        None => airc.page_recent(effective_limit).await?,
    };
    if as_json {
        print_inbox_json(&events)?;
        return Ok(());
    }
    if events.is_empty() {
        println!("(no events)");
        return Ok(());
    }
    for event in &events {
        print_event(event);
    }
    if let Some(cursor) = events.last().map(airc_core::TranscriptEvent::cursor) {
        println!();
        println!(
            "cursor: lamport={} event_id={} — pass both as --since-lamport / --since-event-id",
            cursor.lamport, cursor.event_id
        );
    }
    Ok(())
}

/// Emit a single JSON document for `airc inbox --json`.
///
/// Shape: `{ count, events, cursor: {lamport, event_id} | null }`.
/// The cursor is the paging hint pointing at the newest event in
/// this page; pass both halves back as `--since-lamport` /
/// `--since-event-id` for the next call. Mirrors `airc events
/// list --json` for the `{count, events}` shape and extends it
/// with the paging tuple inbox callers need.
fn print_inbox_json(events: &[airc_core::TranscriptEvent]) -> Result<(), serde_json::Error> {
    #[derive(serde::Serialize)]
    struct InboxJson<'a> {
        count: usize,
        events: &'a [airc_core::TranscriptEvent],
        cursor: Option<InboxCursorJson>,
    }
    #[derive(serde::Serialize)]
    struct InboxCursorJson {
        lamport: u64,
        event_id: String,
    }

    let cursor = events
        .last()
        .map(airc_core::TranscriptEvent::cursor)
        .map(|cursor| InboxCursorJson {
            lamport: cursor.lamport,
            event_id: cursor.event_id.to_string(),
        });
    println!(
        "{}",
        serde_json::to_string_pretty(&InboxJson {
            count: events.len(),
            events,
            cursor,
        })?
    );
    Ok(())
}

async fn print_event_stream_until_signal<S>(
    stream: &mut S,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: futures::stream::Stream<
            Item = Result<std::sync::Arc<airc_core::TranscriptEvent>, airc_lib::LiveLag>,
        > + Unpin,
{
    let sigint = tokio::signal::ctrl_c();
    let mut sigint = Box::pin(sigint);
    loop {
        tokio::select! {
            biased;
            _ = &mut sigint => {
                println!();
                println!("interrupted; exiting.");
                return Ok(());
            }
            next = stream.next() => {
                match next {
                    Some(Ok(event)) => print_event(&event),
                    Some(Err(lag)) => {
                        // LiveLag is the explicit signal that the
                        // consumer fell behind broadcast capacity.
                        // Print and continue — the operating doc
                        // says lag must surface, not silently drop.
                        eprintln!("{lag}");
                    }
                    None => {
                        println!("stream closed; exiting.");
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn print_event(event: &airc_core::TranscriptEvent) {
    // Structured events render by kind; `alive` heartbeats are suppressed
    // (None) so they don't drown the feed. See `event_render`.
    if let Some(line) = crate::event_render::render_feed_line(event) {
        println!("{line}");
    }
}

/// Build the runtime `PeerKeyRegistry` from persistent peers
/// (store-backed peer trust) + ad-hoc `--peer` flags. Self is always
/// enroled. Ad-hoc unions on top of persistent — if the same peer_id
/// appears in both, the ad-hoc pubkey wins (matches "this invocation
/// is authoritative" intuition).
async fn build_combined_registry(
    home: &Path,
    identity: &LocalIdentity,
    adhoc: &[PeerSpec],
) -> Result<Arc<PeerKeyRegistry>, Box<dyn std::error::Error>> {
    let registry = PeerKeyRegistry::new();
    registry.enrol(identity.peer_id, 0, identity.keypair.public_bytes())?;
    for stored in peers_store::load(home).await? {
        registry.enrol(stored.peer_id, 0, stored.pubkey_bytes()?)?;
    }
    for spec in adhoc {
        registry.enrol(spec.peer_id, 0, spec.pubkey)?;
    }
    Ok(Arc::new(registry))
}

/// `peer add <spec>` — persist a peer to the trust store via
/// `Airc::add_peer`. If a daemon is running on the given socket,
/// also tells it via the AddPeer RPC so the in-memory registry
/// stays in sync.
pub async fn run_peer_add(
    home: &Path,
    spec: PeerSpec,
    socket: PathBuf,
    tier: Option<airc_store::TrustTier>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let pubkey_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        spec.pubkey,
    );
    let peer_id = spec.peer_id;
    airc.add_peer(spec).await?;
    // Card 34942ec1 Sub-C: --tier override. If unset, the substrate
    // default (Untrusted) from Sub-A applies — no surface change for
    // existing callers. If set, promote the freshly-added row to the
    // requested tier in a separate set_peer_trust_tier call. The
    // two-step write isn't atomic at the SQL level but the
    // substrate's invariant is "tier is orthogonal to key material"
    // — a peer briefly visible at Untrusted before the promotion
    // commits is the same state any honest peer starts at.
    if let Some(tier) = tier {
        airc_trust::set_tier(home, peer_id, tier)
            .await?
            .ok_or_else(|| {
                format!(
                    "internal: just-added peer {peer_id} missing during tier-set — \
                     report this as a substrate bug"
                )
            })?;
        println!("enroled peer_id={peer_id} (pubkey 32 bytes) tier={tier}");
    } else {
        println!("enroled peer_id={peer_id} (pubkey 32 bytes) tier=untrusted (default)");
    }

    // Best-effort daemon sync. If the daemon isn't running, that's
    // fine — it'll pick up the trust store on next start.
    let client = DaemonClient::new(socket);
    match client
        .call_with_timeout(
            Request::AddPeer(AddPeerRequest {
                peer_id,
                pubkey_b64,
            }),
            Duration::from_millis(250),
        )
        .await
    {
        Ok(Response::Ok) => println!("daemon: in-memory registry updated."),
        Ok(other) => println!("daemon: skipped in-memory registry sync ({other:?})."),
        Err(_) => {
            println!("daemon: not running (trust store updated; daemon will load on next start).")
        }
    }
    Ok(())
}

/// `peer remove <peer-id>` — remove a peer from durable trust and
/// update the running daemon's verifier when present.
pub async fn run_peer_remove(
    home: &Path,
    peer_id: airc_core::PeerId,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let removed = airc.remove_peer(peer_id, "manual").await?;
    if removed {
        println!("removed peer_id={peer_id}");
    } else {
        println!("peer_id={peer_id} was not enroled");
    }

    let client = DaemonClient::new(socket);
    match client
        .call_with_timeout(
            Request::RemovePeer(RemovePeerRequest { peer_id }),
            Duration::from_millis(250),
        )
        .await
    {
        Ok(Response::Ok) => println!("daemon: in-memory registry updated."),
        Ok(other) => println!("daemon: skipped in-memory registry sync ({other:?})."),
        Err(_) => {
            println!("daemon: not running (trust store updated; daemon will load on next start).")
        }
    }
    Ok(())
}

/// Card 34942ec1 Sub-C: update an enrolled peer's tier without
/// touching key material. Refuses for unknown peers (no implicit
/// add — the operator should `peer add <spec> --tier=…` instead).
/// Idempotent for no-op transitions.
pub async fn run_peer_set_tier(
    home: &Path,
    peer_id: airc_core::PeerId,
    tier: airc_store::TrustTier,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    // Look up the prior tier so the output can name both old + new
    // (operator audit trail) AND so the idempotent no-op path can
    // honestly say "no change."
    let prior = airc_trust::load(home)
        .await?
        .into_iter()
        .find(|p| p.peer_id == peer_id);
    let Some(prior) = prior else {
        return Err(format!(
            "peer {peer_id} is not enrolled in this scope's trust store. \
             Use `airc peer add <spec> --tier={tier}` to enrol fresh, \
             or check `airc peer list` for the right peer_id."
        )
        .into());
    };
    if prior.tier == tier {
        println!("no change: peer_id={peer_id} already at tier={tier} (idempotent)");
        return Ok(());
    }
    let prior_tier = prior.tier;
    let updated = airc_trust::set_tier(home, peer_id, tier)
        .await?
        .ok_or_else(|| {
            format!(
                "internal: peer {peer_id} disappeared between load and set_tier — \
             likely a concurrent `peer remove`; retry or check the trust store"
            )
        })?;
    println!(
        "tier_changed: peer_id={peer_id} {prior_tier} → {new}",
        new = updated.tier
    );

    // Best-effort daemon sync — same shape as run_peer_add /
    // run_peer_remove. The daemon currently has no SetTier RPC; on
    // a follow-up Sub-D it should subscribe to a TrustTierChanged
    // event and re-evaluate its in-memory verifier policy. For now
    // the trust store is the source of truth; the daemon will pick
    // up the new tier on its next read.
    let _ = socket; // placeholder until SetTier RPC ships (Sub-D)
    Ok(())
}

/// `peer list` — print enroled peers via `Airc::peers`. The daemon
/// writes the same trust store, so this view stays consistent
/// whether the daemon is running or not. `--json` produces the
/// machine-readable shape consumers (bridge, router) read off of.
pub async fn run_peer_list(home: &Path, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let peers = airc_trust::load(home).await?;
    if json {
        // Card 34942ec1 Sub-C V4: JSON shape is the contract
        // consumers read. Pin the field names + the tier wire
        // string so a future schema drift breaks the test in
        // peer_commands.rs, not the consumer at runtime.
        let rows: Vec<serde_json::Value> = peers
            .iter()
            .map(|p| {
                serde_json::json!({
                    "peer_id": p.peer_id.to_string(),
                    "pubkey_b64": p.pubkey_b64,
                    "added_at_ms": p.added_at_ms,
                    "tier": p.tier.as_wire_str(),
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    if peers.is_empty() {
        println!("(no enroled peers — use `airc peer add <spec>` to enrol)");
        return Ok(());
    }
    for peer in &peers {
        println!(
            "{}  {}  tier={}",
            peer.peer_id,
            peer.pubkey_b64,
            peer.tier.as_wire_str()
        );
    }
    println!();
    println!("{} peer(s) enroled at {}", peers.len(), home.display());
    Ok(())
}

/// `whois <peer>` — print the trust entry for an enrolled peer.
///
/// Rich peer identity cards are a roster-layer follow-up. This command
/// is intentionally honest today: it resolves the peer trust entry that
/// controls message verification instead of pretending to have profile
/// metadata that is not yet published on the substrate.
pub async fn run_whois_peer(home: &Path, target: &str) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let peers = airc.peers().await?;
    let matches = peers
        .iter()
        .filter(|peer| peer.peer_id.to_string().starts_with(target))
        .collect::<Vec<_>>();

    match matches.as_slice() {
        [] => {
            println!("peer not found: {target}");
            if peers.is_empty() {
                println!("(no enroled peers — use `airc peer add <spec>` to enrol)");
            } else {
                println!("known peers:");
                for peer in peers {
                    println!("  {}  {}", peer.peer_id, peer.pubkey_b64);
                }
            }
            Err("peer not found".into())
        }
        [peer] => {
            println!("  peer_id:   {}", peer.peer_id);
            println!("  pubkey:    {}", peer.pubkey_b64);
            // Card 20066c49: read the identity card the peer published
            // via the substrate (IdentityPublished events emitted on
            // join — cards 088af06 / cd638b8) when known. Falls back
            // to the honest "not published yet" line so the user can
            // tell unknown from blank-but-known.
            match airc.peer_identity_card(peer.peer_id).await {
                Ok(Some(card)) => {
                    let id = &card.identity;
                    let name = if id.name.is_empty() {
                        "(unset)"
                    } else {
                        id.name.as_str()
                    };
                    let pronouns = if id.pronouns.is_empty() {
                        "(unset)"
                    } else {
                        id.pronouns.as_str()
                    };
                    let role = if id.role.is_empty() {
                        "(unset)"
                    } else {
                        id.role.as_str()
                    };
                    let bio = if id.bio.is_empty() {
                        "(unset)"
                    } else {
                        id.bio.as_str()
                    };
                    let status = if id.status.is_empty() {
                        "(none)"
                    } else {
                        id.status.as_str()
                    };
                    let fingerprint = if id.fingerprint.is_empty() {
                        "(unset)"
                    } else {
                        id.fingerprint.as_str()
                    };
                    println!("  identity:  published");
                    println!("    name:        {name}");
                    println!("    pronouns:    {pronouns}");
                    println!("    role:        {role}");
                    println!("    bio:         {bio}");
                    println!("    status:      {status}");
                    println!("    fingerprint: {fingerprint}");
                    if !id.integrations.is_empty() {
                        println!("    integrations:");
                        for (k, v) in &id.integrations {
                            println!("      {k}: {v}");
                        }
                    }
                    println!("    emitted_at:  {} ms", card.emitted_at_ms);
                }
                Ok(None) => println!("  identity:  not published yet"),
                Err(error) => println!("  identity:  lookup failed: {error}"),
            }
            println!("  source:    peer trust store");
            Ok(())
        }
        _ => {
            println!("ambiguous peer prefix: {target}");
            for peer in matches {
                println!("  {}  {}", peer.peer_id, peer.pubkey_b64);
            }
            Err("ambiguous peer prefix".into())
        }
    }
}

// Silence the unused-import warning for `ClientId`: it's used
// transitively through `LocalIdentity::client_id` (the
// `airc_core::ClientId` newtype) but not referenced by name in this
// file. Keeping the import explicit makes the dep graph readable.
#[allow(dead_code)]
fn _client_id_kept_in_scope(_: ClientId) {}

fn runtime_headers() -> Result<Headers, Box<dyn std::error::Error>> {
    let mut headers = Headers::new();
    if let Some(client) = crate::client_id::current_client_id()? {
        headers.insert(HEADER_AIRC_CLIENT.to_string(), client);
    }
    Ok(headers)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(commit: Option<&str>, protocol: Option<u32>) -> airc_ipc::StatusResponse {
        airc_ipc::StatusResponse {
            peer_id: "07e7ad58-ba56-4535-b4e5-a161a110e487".to_string(),
            uptime_seconds: 1,
            ipc_protocol_version: protocol,
            build_commit: commit.map(str::to_string),
            build_branch: Some("rust-rewrite".to_string()),
            executable: Some("/tmp/airc".to_string()),
        }
    }

    #[test]
    fn daemon_status_current_requires_matching_protocol_and_build() {
        assert!(daemon_status_is_current(&status(
            Some(crate::build_info::COMMIT),
            Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION))
        )));
        assert!(!daemon_status_is_current(&status(
            Some("old-build"),
            Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION))
        )));
        assert!(!daemon_status_is_current(&status(
            Some(crate::build_info::COMMIT),
            Some(u32::from(airc_ipc::IPC_PROTOCOL_VERSION) + 1)
        )));
    }

    #[test]
    fn daemon_status_without_metadata_is_stale() {
        assert!(!daemon_status_is_current(&status(None, None)));
    }
}
