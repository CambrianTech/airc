//! Subcommand handlers — keep them small and direct. Each function
//! takes the parsed CLI context and runs.

use std::path::Path;
use std::str::FromStr;

use airc_core::{
    headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
    TranscriptCursor,
};
use airc_protocol::{Envelope, Frame, FrameKind, Signature, Subscription, VerificationPolicy};
use airc_transport::{LanTcpAdapter, LocalFsAdapter, SignedTransport, Transport};
use futures::stream::StreamExt;
use uuid::Uuid;

use crate::identity::load_or_generate;
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

    // SignedTransport wraps the inner once connected.
    let transport = SignedTransport::new(
        inner,
        keypair,
        peer_id,
        registry,
        VerificationPolicy::Strict,
    );

    // Give the post-handshake spawn task time to install the
    // outbound channel before send.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

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
