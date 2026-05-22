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
use std::sync::RwLock;
use std::time::{Duration, Instant};

use airc_core::{ClientId, EventId, PeerId, TranscriptCursor};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy, HEADER_AIRC_CLIENT};
use futures::stream::StreamExt;

use airc_daemon::{
    peers_store, run as run_daemon_server, AddPeerRequest, DaemonClient, DaemonState,
    LocalIdentity, SubscribeRequest,
};
use airc_lib::{Airc, Body, Headers, PeerSpec};
use airc_store::{EventStore, SqliteEventStore};

/// `init` — open the substrate at `<home>`. `Airc::open` loads or
/// generates the identity, opens the event store, applies any
/// pending migrations, and primes the peer registry. The CLI then
/// prints the local peer's spec so the user can share it.
pub async fn run_init(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let current = airc.current_room().await?;
    println!("home:        {}", airc.home().display());
    println!("peer_id:     {}", airc.peer_id());
    println!("client_id:   {}", airc.client_id());
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
/// deterministic room derived from `<name>`. `--wire` overrides the
/// per-home default wire dir (test-only shared-wire setup).
pub async fn run_room(
    home: &Path,
    name: Option<String>,
    wire: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    match name {
        Some(name) => {
            let next = match wire {
                Some(wire) => airc.join_with_wire(&name, wire).await?,
                None => airc.join(&name).await?,
            };
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

/// `join` — account-room coordinator entrypoint. With no explicit
/// room, subscribe to `#general` plus the inferred Git owner channel.
/// With a room, join that arbitrary channel and make it default.
pub async fn run_join(home: &Path, room: Option<String>) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
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
    let socket = crate::cli::default_socket_path_in(home);
    ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    subscribe_daemon_to_current_rooms(home, socket).await?;
    ensure_runtime_integrations();

    // `airc join` is THE public verb — no separate attach command.
    // Whether to stream live events after setup is decided by
    // runtime context, NOT a flag:
    //   - Agent runtime (Claude Code, Codex): attach, multi-room stream
    //   - Interactive TTY: attach
    //   - cargo test / cargo run / pipe / script context: return cleanly
    //
    // Agent runtime is detected through explicit env markers OR the
    // same runtime client identity resolver used for event stamping
    // (`airc client-id`). The cargo signal (`CARGO_BIN_EXE_*` /
    // `CARGO_PKG_NAME` env vars set on the test binary AND inherited
    // by spawned children) takes priority — even if Claude Code or
    // Codex is the parent process, a `cargo test` run inside it
    // should not hang the test harness.
    // `AIRC_NO_ATTACH=1` is an explicit internal opt-out for any
    // other script that needs setup-only without inheriting cargo
    // envs.
    if should_attach_after_join() {
        crate::join_feed::run(&airc, home).await?;
    }
    Ok(())
}

/// Decide whether `airc join` should attach to the multi-room live
/// stream after completing setup. Internal contract, no public
/// flag. The rule:
///
/// 1. `AIRC_NO_ATTACH=1` — explicit opt-out (scripts, smokes).
///    Highest priority.
/// 2. Cargo context — `CARGO_BIN_EXE_*` or `CARGO_PKG_NAME` set
///    means we're inside `cargo test` / `cargo run` and must not
///    hang the harness. This wins over agent-runtime markers
///    (which may also be set if Claude Code is the parent shell).
/// 3. Agent runtime markers — Claude Code (`CLAUDECODE`,
///    `CLAUDE_CODE_SESSION_ID`, `AI_AGENT`), Codex
///    (`CODEX_AGENT_ID`, `CODEX_SESSION_ID`,
///    `AIRC_CODEX_START_CHILD`).
/// 4. TTY — interactive terminal use.
/// 5. Otherwise — exit cleanly after setup.
fn should_attach_after_join() -> bool {
    use std::io::IsTerminal;
    let runtime_client_detected = crate::client_id::current_client_id()
        .ok()
        .flatten()
        .is_some();
    should_attach_after_join_with_client(
        std::env::vars_os().map(|(key, value)| {
            (
                key.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        }),
        std::io::stdout().is_terminal(),
        runtime_client_detected,
    )
}

#[cfg(test)]
fn should_attach_after_join_from<I, K, V>(env: I, stdout_is_tty: bool) -> bool
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    should_attach_after_join_with_client(env, stdout_is_tty, false)
}

fn should_attach_after_join_with_client<I, K, V>(
    env: I,
    stdout_is_tty: bool,
    runtime_client_detected: bool,
) -> bool
where
    I: IntoIterator<Item = (K, V)>,
    K: AsRef<str>,
    V: AsRef<str>,
{
    let mut has_opt_out = false;
    let mut has_cargo_context = false;
    let mut has_agent_marker = false;
    for (key, _value) in env {
        let key = key.as_ref();
        if key == "AIRC_NO_ATTACH" {
            has_opt_out = true;
        }
        if key.starts_with("CARGO_BIN_EXE_") || key == "CARGO_PKG_NAME" {
            has_cargo_context = true;
        }
        if matches!(
            key,
            "CLAUDECODE"
                | "CLAUDE_CODE_SESSION_ID"
                | "AI_AGENT"
                | "CODEX_AGENT_ID"
                | "CODEX_SESSION_ID"
                | "AIRC_CODEX_START_CHILD"
        ) {
            has_agent_marker = true;
        }
    }
    if has_opt_out {
        return false;
    }
    if has_cargo_context {
        return false;
    }
    if has_agent_marker {
        return true;
    }
    if runtime_client_detected {
        return true;
    }
    stdout_is_tty
}

fn ensure_runtime_integrations() {
    match crate::codex_install::install_hooks_for_default_home_if_present() {
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
    println!("  airc {}", env!("CARGO_PKG_VERSION"));
    println!("  install: {}", exe_path.display());
    Ok(())
}

pub async fn ensure_daemon_running(
    home: &Path,
    socket: PathBuf,
    _peers: Vec<PeerSpec>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket.clone());
    if client
        .ping_with_timeout(Duration::from_millis(250))
        .await
        .is_ok()
    {
        return Ok(());
    }

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
    command.spawn()?;

    let client = DaemonClient::new(socket);
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if client
            .ping_with_timeout(Duration::from_millis(250))
            .await
            .is_ok()
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Err(format!("daemon did not become ready; see {}", log.display()).into())
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

async fn subscribe_daemon_to_current_rooms(
    home: &Path,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let client = DaemonClient::new(socket);
    let set = airc.subscription_set().await?;
    sync_daemon_peers(&client, home, &set).await?;
    for subscription in set.all() {
        client
            .subscribe(SubscribeRequest {
                wire: subscription.as_room().wire,
            })
            .await?;
    }
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
    for peer in peers_store::load(home)? {
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
pub async fn run_send(
    home: &Path,
    peers: Vec<PeerSpec>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
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
    let airc = Airc::open(home).await?;
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
    let actual = airc.listen_lan(bind).await?;
    println!("listening on {actual} (peer_id {}) …", airc.peer_id());
    let mut stream = airc.subscribe().await?;
    print_event_stream_until_signal(&mut stream).await
}

/// `daemon` — run the long-lived daemon process on the given socket.
pub async fn run_daemon(
    home: &Path,
    identity: LocalIdentity,
    peers: Vec<PeerSpec>,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = build_combined_registry(home, &identity, &peers)?;

    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    // The durable event store lives under `<home>/events.sqlite`.
    // Migrations are applied on open; consumers that subscribed
    // before the daemon was last restarted can resume from the
    // same `(lamport, event_id)` cursor on next boot.
    let store_path = home.join("events.sqlite");
    let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::open_path(&store_path).await?);
    let state = Arc::new(DaemonState::new(
        identity.peer_id,
        identity.keypair,
        registry,
        VerificationPolicy::Strict,
        home.to_path_buf(),
        store,
    ));
    println!(
        "airc daemon: peer_id={} listening on {}",
        identity.peer_id,
        socket.display()
    );
    run_daemon_server(state, socket).await?;
    println!("airc daemon: stopped.");
    Ok(())
}

// ---- Daemon-client commands (no identity load needed) ---------------

pub async fn run_ping(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.ping().await?;
    println!("pong");
    Ok(())
}

pub async fn run_status(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    let status = client.status().await?;
    println!("peer_id:        {}", status.peer_id);
    println!("uptime_seconds: {}", status.uptime_seconds);
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
    ensure_daemon_running(home, socket.clone(), Vec::new()).await?;
    subscribe_daemon_to_current_rooms(home, socket.clone()).await?;
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
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = match socket {
        Some(socket) => Airc::attach(home, socket).await?,
        None => Airc::open(home).await?,
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

async fn print_event_stream_until_signal<S>(
    stream: &mut S,
) -> Result<(), Box<dyn std::error::Error>>
where
    S: futures::stream::Stream<Item = Result<airc_core::TranscriptEvent, airc_lib::LiveLag>>
        + Unpin,
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
    let text = event
        .body
        .as_ref()
        .and_then(Body::as_text)
        .unwrap_or("<non-text body>");
    println!(
        "[{kind:?}] {sender} → {channel}: {text}",
        kind = event.kind,
        sender = event.peer_id,
        channel = event.room_id,
    );
}

/// Build the runtime `PeerKeyRegistry` from persistent peers
/// (`<home>/peers.json`) + ad-hoc `--peer` flags. Self is always
/// enroled. Ad-hoc unions on top of persistent — if the same peer_id
/// appears in both, the ad-hoc pubkey wins (matches "this invocation
/// is authoritative" intuition).
fn build_combined_registry(
    home: &Path,
    identity: &LocalIdentity,
    adhoc: &[PeerSpec],
) -> Result<Arc<RwLock<PeerKeyRegistry>>, Box<dyn std::error::Error>> {
    let mut registry = PeerKeyRegistry::new();
    registry.enrol(identity.peer_id, 0, identity.keypair.public_bytes())?;
    for stored in peers_store::load(home)? {
        registry.enrol(stored.peer_id, 0, stored.pubkey_bytes()?)?;
    }
    for spec in adhoc {
        registry.enrol(spec.peer_id, 0, spec.pubkey)?;
    }
    Ok(Arc::new(RwLock::new(registry)))
}

/// `peer add <spec>` — persist a peer to `<home>/peers.json` via
/// `Airc::add_peer`. If a daemon is running on the given socket,
/// also tells it via the AddPeer RPC so the in-memory registry
/// stays in sync.
pub async fn run_peer_add(
    home: &Path,
    spec: PeerSpec,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let pubkey_b64 = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        spec.pubkey,
    );
    let peer_id = spec.peer_id;
    airc.add_peer(spec).await?;
    println!("enroled peer_id={peer_id} (pubkey 32 bytes)");

    // Best-effort daemon sync. If the daemon isn't running, that's
    // fine — it'll pick up peers.json on next start.
    let client = DaemonClient::new(socket);
    match client
        .add_peer(AddPeerRequest {
            peer_id,
            pubkey_b64,
        })
        .await
    {
        Ok(()) => println!("daemon: in-memory registry updated."),
        Err(_) => {
            println!("daemon: not running (peers.json updated; daemon will load on next start).")
        }
    }
    Ok(())
}

/// `peer list` — print enroled peers via `Airc::peers`. The daemon
/// writes the same `<home>/peers.json`, so this view stays
/// consistent whether the daemon is running or not.
pub async fn run_peer_list(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    let peers = airc.peers().await?;
    if peers.is_empty() {
        println!("(no enroled peers — use `airc peer add <spec>` to enrol)");
        return Ok(());
    }
    for peer in &peers {
        println!("{}  {}", peer.peer_id, peer.pubkey_b64);
    }
    println!();
    println!("{} peer(s) enroled at {}", peers.len(), home.display());
    Ok(())
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
    use super::{should_attach_after_join_from, should_attach_after_join_with_client};

    #[test]
    fn join_attach_decision_streams_for_codex_agent() {
        assert!(should_attach_after_join_from(
            [("CODEX_SESSION_ID", "thread-1")],
            false
        ));
    }

    #[test]
    fn join_attach_decision_streams_for_claude_agent() {
        assert!(should_attach_after_join_from(
            [("CLAUDE_CODE_SESSION_ID", "session-1")],
            false
        ));
    }

    #[test]
    fn join_attach_decision_streams_for_interactive_tty() {
        assert!(should_attach_after_join_from(
            std::iter::empty::<(&str, &str)>(),
            true
        ));
    }

    #[test]
    fn join_attach_decision_streams_for_detected_runtime_client() {
        assert!(should_attach_after_join_with_client(
            std::iter::empty::<(&str, &str)>(),
            false,
            true
        ));
    }

    #[test]
    fn join_attach_decision_exits_for_cargo_context() {
        assert!(!should_attach_after_join_with_client(
            [
                ("CLAUDE_CODE_SESSION_ID", "session-1"),
                ("CARGO_BIN_EXE_airc", "/tmp/airc"),
            ],
            true,
            true
        ));
    }

    #[test]
    fn join_attach_decision_internal_opt_out_wins() {
        assert!(!should_attach_after_join_with_client(
            [("CODEX_SESSION_ID", "thread-1"), ("AIRC_NO_ATTACH", "1")],
            true,
            true
        ));
    }

    #[test]
    fn join_attach_decision_exits_for_noninteractive_script() {
        assert!(!should_attach_after_join_from(
            std::iter::empty::<(&str, &str)>(),
            false
        ));
    }
}
