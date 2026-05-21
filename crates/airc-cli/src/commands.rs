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
use std::sync::Arc;
use std::sync::RwLock;

use airc_core::{ClientId, EventId, PeerId, TranscriptCursor};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy};
use futures::stream::StreamExt;

use airc_daemon::{
    peers_store, run as run_daemon_server, AddPeerRequest, DaemonClient, DaemonState, LocalIdentity,
};
use airc_lib::{Airc, Body, PeerSpec};
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
        }
    }
    Ok(())
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
    airc.say(text).await?;
    let peer_count = airc.peers().await?.len();
    if peer_count == 0 {
        println!(
            "stored locally in {} ({}) - no enrolled peers; not delivered to another agent.",
            current.name, current.channel
        );
    } else {
        println!(
            "sent to {} ({}) for {peer_count} enrolled peer(s).",
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
    airc.say(text).await?;
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
    let airc = Airc::attach(home, socket).await?;
    let current = airc.current_room().await?;
    airc.say(text).await?;
    let peer_count = airc.peers().await?.len();
    if peer_count == 0 {
        println!(
            "stored locally in {} ({}) - no enrolled peers; not delivered to another agent.",
            current.name, current.channel
        );
    } else {
        println!(
            "sent to {} ({}) for {peer_count} enrolled peer(s).",
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

async fn print_event_stream_until_signal(
    stream: &mut airc_lib::EventStream,
) -> Result<(), Box<dyn std::error::Error>> {
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
