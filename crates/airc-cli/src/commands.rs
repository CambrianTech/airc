//! Subcommand handlers — keep them small and direct. Each function
//! takes a `LocalIdentity` (loaded once per invocation by `main`) +
//! command-specific args.
//!
//! `VerificationPolicy::Strict` is the only policy used in CLI paths.
//! There is no opt-in for `AllowUnsigned` here — production rules.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
    TranscriptCursor,
};
use airc_protocol::{
    Envelope, Frame, FrameKind, PeerKeyRegistry, Signature, Subscription, VerificationPolicy,
};
use airc_transport::{LanTcpAdapter, LocalFsAdapter, SignedTransport, Transport};
use futures::stream::StreamExt;

use crate::daemon::{run as run_daemon_server, DaemonState};
use crate::identity::LocalIdentity;
use crate::ipc::request::{AddPeerRequest, InboxRequest, SubscribeRequest};
use crate::ipc::{DaemonClient, SendRequest};
use crate::peers_store;
use crate::registry::{format_peer_spec, PeerSpec};
use crate::room::{self, Room};

/// `init` — create or load the persisted identity under `<home>`,
/// auto-create the default room, then print the peer spec.
pub fn run_init(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let identity = LocalIdentity::load_or_generate(home)?;
    // Auto-join "default" room on first init so subsequent
    // `airc-rs msg` calls work without `--wire`/`--channel`.
    if !room::path_in(home).exists() {
        let default_room = Room::default_for(home);
        room::save(home, &default_room)?;
    }
    let current = room::load_or_default(home)?;
    println!("home:        {}", home.display());
    println!("peer_id:     {}", identity.peer_id);
    println!("client_id:   {}", identity.client_id);
    println!("room:        {} ({})", current.name, current.channel);
    println!(
        "peer_spec:   {}",
        format_peer_spec(identity.peer_id, &identity.keypair.public_bytes())
    );
    println!();
    println!(
        "Share peer_spec with peers; enrol theirs via `airc-rs peer add <spec>`. \
         Use `airc-rs room <name>` to switch rooms; `airc-rs msg \"hi\"` sends \
         to the current room."
    );
    Ok(())
}

/// `room` — print current room. `room <name>` — switch to a
/// deterministic room derived from `<name>` (same name across peers
/// = same channel UUID, so they don't need to share it). `--wire`
/// overrides the per-home default wire dir; used for shared-wire
/// setups (e.g. local-fs tests with two processes on one machine).
pub fn run_room(
    home: &Path,
    name: Option<String>,
    wire: Option<PathBuf>,
) -> Result<(), Box<dyn std::error::Error>> {
    match name {
        Some(name) => {
            let mut next = Room::from_name(home, &name);
            if let Some(wire) = wire {
                next.wire = wire;
            }
            room::save(home, &next)?;
            println!("switched room: {}", next.name);
            println!("  wire:    {}", next.wire.display());
            println!("  channel: {}", next.channel);
        }
        None => {
            let current = room::load_or_default(home)?;
            println!("room:    {}", current.name);
            println!("wire:    {}", current.wire.display());
            println!("channel: {}", current.channel);
        }
    }
    Ok(())
}

/// `send` — local-fs single-shot send to the current room, signed
/// under Strict.
pub async fn run_send(
    home: &Path,
    identity: &LocalIdentity,
    peers: Vec<PeerSpec>,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = room::load_or_default(home)?;
    let registry = build_combined_registry(home, identity, &peers)?;
    let inner = LocalFsAdapter::new(&current.wire);
    let transport = SignedTransport::new(
        inner,
        identity.keypair.clone(),
        identity.peer_id,
        registry,
        VerificationPolicy::Strict,
    );
    let frame = build_message_frame(identity, current.channel, text);
    transport.send(frame).await?;
    println!("sent to {} ({}).", current.name, current.channel);
    Ok(())
}

/// `listen` — local-fs subscribe loop on the current room. Prints
/// frames until Ctrl-C.
pub async fn run_listen(
    home: &Path,
    identity: &LocalIdentity,
    peers: Vec<PeerSpec>,
    replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = room::load_or_default(home)?;
    let registry = build_combined_registry(home, identity, &peers)?;
    let inner = LocalFsAdapter::new(&current.wire);
    let transport = SignedTransport::new(
        inner,
        identity.keypair.clone(),
        identity.peer_id,
        registry,
        VerificationPolicy::Strict,
    );
    let subscription = subscription_with_channel(current.channel, replay);
    let mut stream = transport.subscribe(subscription).await?;

    println!(
        "listening on {} ({}, peer_id {}) …",
        current.name,
        current.wire.display(),
        identity.peer_id
    );
    print_until_signal(&mut stream).await
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

    let state = Arc::new(DaemonState::new(
        identity.peer_id,
        identity.keypair,
        registry,
        VerificationPolicy::Strict,
        home.to_path_buf(),
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
    limit: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let current = room::load_or_default(home)?;
    let client = DaemonClient::new(socket);
    client
        .subscribe(SubscribeRequest {
            wire: current.wire.clone(),
        })
        .await?;
    let inbox = client
        .inbox(InboxRequest {
            wire: current.wire,
            since_lamport,
            limit,
        })
        .await?;
    if inbox.frames.is_empty() {
        println!("(no new frames; newest_lamport={})", inbox.newest_lamport);
        return Ok(());
    }
    for frame in &inbox.frames {
        print_frame(frame);
    }
    println!();
    println!(
        "newest_lamport={} — pass as --since-lamport on the next call",
        inbox.newest_lamport
    );
    Ok(())
}

// ---- Shared helpers -------------------------------------------------

fn subscription_with_channel(channel: RoomId, replay: bool) -> Subscription {
    let from_cursor = replay.then(|| TranscriptCursor {
        lamport: 0,
        event_id: EventId::from_u128(0),
    });
    Subscription {
        channel: Some(channel),
        from_cursor,
        ..Default::default()
    }
}

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

/// `peer add <spec>` — persist a peer to `<home>/peers.json`. If a
/// daemon is running on the given socket, also tells it via the
/// AddPeer RPC so the in-memory registry stays in sync.
pub async fn run_peer_add(
    home: &Path,
    spec: PeerSpec,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let stored = peers_store::add(home, spec.peer_id, spec.pubkey)?;
    println!("enroled peer_id={} (pubkey 32 bytes)", stored.peer_id);

    // Best-effort daemon sync. If the daemon isn't running, that's
    // fine — it'll pick up peers.json on next start.
    let client = DaemonClient::new(socket);
    match client
        .add_peer(AddPeerRequest {
            peer_id: stored.peer_id,
            pubkey_b64: stored.pubkey_b64.clone(),
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

/// `peer list` — print enroled peers from `<home>/peers.json`. The
/// daemon writes the same file, so this is a stable view whether the
/// daemon is running or not.
pub fn run_peer_list(home: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let peers = peers_store::load(home)?;
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
