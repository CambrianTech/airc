//! Subcommand handlers.
//!
//! Local-substrate commands (`init`, `send`, `listen`, `room`,
//! `peer add`, `peer list`) route through `airc_lib::Airc` — the
//! CLI is a thin client of the same API consumers embed. Closes
//! grievance §5 / Codex audit finding #4.
//!
//! LAN-TCP and daemon-host commands construct lower-level handles
//! (`LanTcpAdapter`, `DaemonState`) directly — they're not in
//! airc-lib's surface yet. Daemon-client commands (`ping`, `status`,
//! `stop`, `msg`, `inbox`) use `airc_daemon::DaemonClient` directly.
//!
//! `VerificationPolicy::Strict` is the only policy used in CLI paths.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use airc_core::{
    headers::Headers, transcript::MentionTarget, ClientId, EventId, PeerId, RoomId,
    TranscriptCursor,
};
use airc_protocol::{
    Envelope, Frame, FrameKind, PeerKeyRegistry, Signature, Subscription, VerificationPolicy,
};
use airc_transport::{LanTcpAdapter, SignedTransport, Transport};
use futures::stream::StreamExt;

use airc_daemon::{
    peers_store, run as run_daemon_server, AddPeerRequest, DaemonClient, DaemonState, InboxRequest,
    LocalIdentity, SendRequest, SubscribeRequest,
};
use airc_lib::{room, Airc, Body, PeerSpec};
use airc_store::{EventStore, SqliteEventStore};

/// `init` — open the substrate at `<home>`. `Airc::open` loads or
/// generates the identity, opens the event store, applies any
/// pending migrations, and primes the peer registry. The CLI then
/// prints the local peer's spec so the user can share it.
pub async fn run_init(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let airc = Airc::open(home).await?;
    // Auto-join "default" room on first init so subsequent
    // `airc-rs msg` / `say` work without explicit room selection.
    if airc.current_room().await?.name != "default" {
        // load_or_default returns the synthesised default when
        // room.json is missing; if a different room name comes back
        // the user has already joined something — leave it.
    } else if !airc.home().join("room.json").exists() {
        airc.join("default").await?;
    }
    let current = airc.current_room().await?;
    println!("home:        {}", airc.home().display());
    println!("peer_id:     {}", airc.peer_id());
    println!("client_id:   {}", airc.client_id());
    println!("room:        {} ({})", current.name, current.channel);
    println!("peer_spec:   {}", airc.peer_spec());
    println!();
    println!(
        "Share peer_spec with peers; enrol theirs via `airc-rs peer add <spec>`. \
         Use `airc-rs room <name>` to switch rooms; `airc-rs msg \"hi\"` sends \
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
    println!("sent to {} ({}).", current.name, current.channel);
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
    identity: &LocalIdentity,
    peers: Vec<PeerSpec>,
    to: std::net::SocketAddr,
    expected_peer: PeerId,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = room::load_or_default(home)?;
    let registry = build_combined_registry(home, identity, &peers)?;
    let inner = LanTcpAdapter::new(identity.peer_id, identity.keypair.clone(), registry.clone())?;
    inner.connect(to, expected_peer).await?;
    let transport = SignedTransport::new(
        inner,
        identity.keypair.clone(),
        identity.peer_id,
        registry,
        VerificationPolicy::Strict,
    );
    let frame = build_message_frame(identity, current.channel, text);
    transport.send(frame).await?;
    println!(
        "sent over lan-tcp to {} ({}).",
        current.name, current.channel
    );
    Ok(())
}

/// `lan-listen` — bind a TLS server, accept peers, print frames.
pub async fn run_lan_listen(
    home: &Path,
    identity: &LocalIdentity,
    peers: Vec<PeerSpec>,
    bind: std::net::SocketAddr,
    replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let registry = build_combined_registry(home, identity, &peers)?;
    let inner = LanTcpAdapter::new(identity.peer_id, identity.keypair.clone(), registry.clone())?;
    let actual = inner.listen(bind).await?;
    println!("listening on {actual} (peer_id {}) …", identity.peer_id);
    let transport = SignedTransport::new(
        inner,
        identity.keypair.clone(),
        identity.peer_id,
        registry,
        VerificationPolicy::Strict,
    );
    // LAN listen accepts frames on any channel — no filter.
    let from_cursor = replay.then(|| TranscriptCursor {
        lamport: 0,
        event_id: EventId::from_u128(0),
    });
    let subscription = Subscription {
        channel: None,
        from_cursor,
        ..Default::default()
    };
    let mut stream = transport.subscribe(subscription).await?;
    print_until_signal(&mut stream).await
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
        "airc-rs daemon: peer_id={} listening on {}",
        identity.peer_id,
        socket.display()
    );
    run_daemon_server(state, socket).await?;
    println!("airc-rs daemon: stopped.");
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
    let current = room::load_or_default(home)?;
    let client = DaemonClient::new(socket);
    client
        .send(SendRequest {
            wire: current.wire,
            channel: current.channel.as_uuid(),
            text: text.to_string(),
        })
        .await?;
    println!("sent to {} ({}).", current.name, current.channel);
    Ok(())
}

pub async fn run_inbox(
    home: &Path,
    socket: PathBuf,
    since_lamport: Option<u64>,
    since_event_id: Option<String>,
    limit: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = room::load_or_default(home)?;
    let client = DaemonClient::new(socket);
    client
        .subscribe(SubscribeRequest {
            wire: current.wire.clone(),
        })
        .await?;
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
    let inbox = client
        .inbox(InboxRequest {
            since,
            channel: Some(current.channel),
            limit,
        })
        .await?;
    if inbox.events.is_empty() {
        match &inbox.newest {
            Some(c) => println!(
                "(no new events; cursor lamport={} event_id={})",
                c.lamport, c.event_id
            ),
            None => println!("(no events yet — store is empty)"),
        }
        return Ok(());
    }
    for event in &inbox.events {
        print_event(event);
    }
    if let Some(cursor) = inbox.newest {
        println!();
        println!(
            "cursor: lamport={} event_id={} — pass both as --since-lamport / --since-event-id",
            cursor.lamport, cursor.event_id
        );
    }
    Ok(())
}

// ---- Shared helpers (LAN commands) ---------------------------------

fn build_message_frame(identity: &LocalIdentity, channel: RoomId, text: &str) -> Frame {
    let lamport = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Frame {
        kind: FrameKind::Message,
        envelope: Envelope {
            event_id: EventId::new(),
            sender: identity.peer_id,
            // Stable ClientId from the persisted identity — multi-tab
            // disambiguation, replay records cite this.
            sender_client: identity.client_id,
            channel,
            target: MentionTarget::All,
            lamport,
            occurred_at_ms: lamport,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text(text)),
            // SignedTransport replaces this with Ed25519 on the way out.
            signature: Signature::Unsigned,
            media: Vec::new(),
        },
    }
}

async fn print_until_signal<S, E>(stream: &mut S) -> Result<(), Box<dyn std::error::Error>>
where
    S: futures::Stream<Item = Result<Frame, E>> + Unpin,
    E: std::error::Error + Send + Sync + 'static,
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
                    Some(Ok(frame)) => print_frame(&frame),
                    Some(Err(error)) => eprintln!("verification failed: {error}"),
                    None => {
                        println!("stream closed; exiting.");
                        return Ok(());
                    }
                }
            }
        }
    }
}

fn print_frame(frame: &Frame) {
    let text = frame
        .envelope
        .body
        .as_ref()
        .and_then(Body::as_text)
        .unwrap_or("<non-text body>");
    println!(
        "[{kind:?}] {sender} → {channel}: {text}",
        kind = frame.kind,
        sender = frame.envelope.sender,
        channel = frame.envelope.channel,
    );
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
        println!("(no enroled peers — use `airc-rs peer add <spec>` to enrol)");
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
