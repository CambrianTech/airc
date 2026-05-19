//! Subcommand handlers — keep them small and direct. Each function
//! takes the parsed CLI context and runs.

use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
    TranscriptCursor,
};
use airc_protocol::{Envelope, Frame, FrameKind, Signature, Subscription, VerificationPolicy};
use airc_transport::{LanTcpAdapter, LocalFsAdapter, SignedTransport, Transport};
use futures::stream::StreamExt;
use uuid::Uuid;

use crate::daemon::{run as run_daemon_server, DaemonState};
use crate::identity::load_or_generate;
use crate::ipc::request::{InboxRequest, SubscribeRequest};
use crate::ipc::{DaemonClient, SendRequest};
use crate::registry::{build_registry, format_peer_spec, PeerSpec};

/// `init` — load or generate the identity, print the peer spec.
pub fn run_init(identity_file: &Path, peer_id: Option<PeerId>) -> std::io::Result<()> {
    let keypair = load_or_generate(identity_file)?;
    let peer_id = peer_id.unwrap_or_default();
    println!("identity_file: {}", identity_file.display());
    println!("peer_id:       {peer_id}");
    println!(
        "peer_spec:     {}",
        format_peer_spec(peer_id, &keypair.public_bytes())
    );
    println!();
    println!("Share `peer_spec` with the other side, and pass it back as");
    println!("`--peer <spec>` plus `--peer-id {peer_id}` on subsequent commands.");
    Ok(())
}

/// `send` — local-fs single-shot send, signed under Strict.
pub async fn run_send(
    identity_file: &Path,
    peer_id: PeerId,
    peers: Vec<PeerSpec>,
    wire: &Path,
    channel: &str,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let keypair = load_or_generate(identity_file)?;
    let registry = build_registry(peer_id, keypair.public_bytes(), &peers)?;

    let inner = LocalFsAdapter::new(wire);
    let transport = SignedTransport::new(
        inner,
        keypair,
        peer_id,
        registry,
        VerificationPolicy::Strict,
    );

    let channel_id = parse_channel(channel)?;
    let frame = build_message_frame(peer_id, channel_id, text);
    transport.send(frame).await?;
    println!("sent.");
    Ok(())
}

/// `listen` — local-fs subscribe loop. Prints frames until Ctrl-C.
pub async fn run_listen(
    identity_file: &Path,
    peer_id: PeerId,
    peers: Vec<PeerSpec>,
    wire: &Path,
    channel: Option<String>,
    replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let keypair = load_or_generate(identity_file)?;
    let registry = build_registry(peer_id, keypair.public_bytes(), &peers)?;

    let inner = LocalFsAdapter::new(wire);
    let transport = SignedTransport::new(
        inner,
        keypair,
        peer_id,
        registry,
        VerificationPolicy::Strict,
    );

    let subscription = subscription_for(channel.as_deref(), replay)?;
    let mut stream = transport.subscribe(subscription).await?;

    println!("listening on {} (peer_id {peer_id}) …", wire.display());
    print_until_signal(&mut stream).await
}

/// `lan-send` — TLS-wrapped single-shot send to a remote peer.
pub async fn run_lan_send(
    identity_file: &Path,
    peer_id: PeerId,
    peers: Vec<PeerSpec>,
    to: std::net::SocketAddr,
    expected_peer: PeerId,
    channel: &str,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let keypair = load_or_generate(identity_file)?;
    let registry = build_registry(peer_id, keypair.public_bytes(), &peers)?;

    let inner = LanTcpAdapter::new(peer_id, keypair.clone(), registry.clone())?;
    inner.connect(to, expected_peer).await?;

    // `connect()` now installs the outbound channel synchronously
    // before returning (Codex's #671 readiness-race fix), so no
    // sleep needed before sending.
    let transport = SignedTransport::new(
        inner,
        keypair,
        peer_id,
        registry,
        VerificationPolicy::Strict,
    );

    let channel_id = parse_channel(channel)?;
    let frame = build_message_frame(peer_id, channel_id, text);
    transport.send(frame).await?;
    println!("sent over lan-tcp.");
    Ok(())
}

/// `lan-listen` — bind a TLS server, accept ONE peer, print frames.
pub async fn run_lan_listen(
    identity_file: &Path,
    peer_id: PeerId,
    peers: Vec<PeerSpec>,
    bind: std::net::SocketAddr,
    replay: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let keypair = load_or_generate(identity_file)?;
    let registry = build_registry(peer_id, keypair.public_bytes(), &peers)?;

    let inner = LanTcpAdapter::new(peer_id, keypair.clone(), registry.clone())?;
    let actual = inner.listen(bind).await?;
    println!("listening on {actual} (peer_id {peer_id}) …");

    let transport = SignedTransport::new(
        inner,
        keypair,
        peer_id,
        registry,
        VerificationPolicy::Strict,
    );

    let subscription = subscription_for(None, replay)?;
    let mut stream = transport.subscribe(subscription).await?;
    print_until_signal(&mut stream).await
}

fn parse_channel(channel: &str) -> Result<RoomId, uuid::Error> {
    let uuid = Uuid::from_str(channel)?;
    Ok(RoomId::from_uuid(uuid))
}

fn subscription_for(
    channel: Option<&str>,
    replay: bool,
) -> Result<Subscription, Box<dyn std::error::Error>> {
    let channel_id = match channel {
        Some(s) => Some(parse_channel(s)?),
        None => None,
    };
    let from_cursor = if replay {
        Some(TranscriptCursor {
            lamport: 0,
            event_id: EventId::from_u128(0),
        })
    } else {
        None
    };
    Ok(Subscription {
        channel: channel_id,
        from_cursor,
        ..Default::default()
    })
}

fn build_message_frame(sender: PeerId, channel: RoomId, text: &str) -> Frame {
    let lamport = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    Frame {
        kind: FrameKind::Message,
        envelope: Envelope {
            event_id: EventId::new(),
            sender,
            sender_client: ClientId::new(),
            channel,
            target: MentionTarget::All,
            lamport,
            occurred_at_ms: lamport,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text(text)),
            media: Vec::new(),
            signature: Signature::Unsigned,
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

/// `daemon` — run the long-lived daemon process on the given socket.
pub async fn run_daemon(
    identity_file: &Path,
    peer_id: PeerId,
    peers: Vec<PeerSpec>,
    socket: PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let keypair = load_or_generate(identity_file)?;
    let registry = build_registry(peer_id, keypair.public_bytes(), &peers)?;

    // Ensure the socket's parent directory exists; the daemon won't
    // create it for the user.
    if let Some(parent) = socket.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }

    let state = Arc::new(DaemonState::new(
        peer_id,
        keypair,
        registry,
        VerificationPolicy::Strict,
    ));
    println!(
        "airc-rs daemon: peer_id={peer_id} listening on {}",
        socket.display()
    );
    run_daemon_server(state, socket).await?;
    println!("airc-rs daemon: stopped.");
    Ok(())
}

/// `ping` — RPC the daemon. Prints "pong" on success.
pub async fn run_ping(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.ping().await?;
    println!("pong");
    Ok(())
}

/// `status` — fetch daemon's health snapshot.
pub async fn run_status(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    let status = client.status().await?;
    println!("peer_id:        {}", status.peer_id);
    println!("uptime_seconds: {}", status.uptime_seconds);
    Ok(())
}

/// `stop` — ask the daemon to shut down gracefully.
pub async fn run_stop(socket: PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    client.stop().await?;
    println!("daemon: stop requested.");
    Ok(())
}

/// `inbox` — Subscribe-then-Inbox via the daemon. Prints any
/// buffered frames newer than `since_lamport`. The first call for a
/// wire kicks off the daemon's subscription; subsequent calls are
/// pure reads.
pub async fn run_inbox(
    socket: PathBuf,
    wire: PathBuf,
    since_lamport: Option<u64>,
    limit: Option<usize>,
) -> Result<(), Box<dyn std::error::Error>> {
    let client = DaemonClient::new(socket);
    // Idempotent — daemon ignores duplicate subscribes for the wire.
    client
        .subscribe(SubscribeRequest { wire: wire.clone() })
        .await?;
    let inbox = client
        .inbox(InboxRequest {
            wire,
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

/// `msg` — send via the daemon.
pub async fn run_msg(
    socket: PathBuf,
    wire: PathBuf,
    channel: &str,
    text: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let channel_uuid = Uuid::from_str(channel)?;
    let client = DaemonClient::new(socket);
    client
        .send(SendRequest {
            wire,
            channel: channel_uuid,
            text: text.to_string(),
        })
        .await?;
    println!("sent.");
    Ok(())
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
