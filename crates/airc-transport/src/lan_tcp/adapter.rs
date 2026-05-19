//! `LanTcpAdapter` — secure same-LAN wire over TLS-wrapped TCP.
//!
//! MVP scope (PR-3c):
//!   - Point-to-peer: one active connection per adapter (either
//!     accepted-from listener or dialed via connect). PR-3d expands
//!     to N-peer fan-out.
//!   - Multi-subscriber fan-out (one adapter, many local
//!     `Transport::subscribe` callers).
//!   - Length-prefixed JSON frames over TLS.
//!   - FrameKind delivery split inherited from the Transport trait:
//!     Message/Control durable+backpressure, Event lossy.
//!   - Mutual TLS via `tls_config` builders; peer identity bound
//!     post-handshake by reading the peer cert and looking up via
//!     `PeerKeyRegistry::find_peer`.
//!
//! What's deferred to follow-ups:
//!   - Multi-peer concurrent connections (PR-3d).
//!   - mDNS/discovery (separate concern).
//!   - Reconnect / retry logic (robustness PR).
//!   - Per-frame ack/replay-cursor (PR-3e or bridge layer).

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use async_trait::async_trait;
use futures::stream::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

use airc_core::PeerId;
use airc_protocol::{Frame, FrameKind, PeerKeyRegistry, PeerKeypair, Subscription};

use crate::lan_tcp::cert::extract_ed25519_pubkey;
use crate::lan_tcp::tls_config::{build_client_config, build_server_config, TlsConfigError};
use crate::transport::{FrameStream, Transport};

/// Per-frame payload size limit. Defense against a malicious or
/// misconfigured peer sending an absurd length prefix. Honest senders
/// stay well under via the body-lift policy (default 16 KiB ceiling).
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Outbound channel depth per connection. Slow remote → senders
/// applying backpressure pile up here before the Transport's
/// kind-aware dispatch kicks in.
const OUTBOUND_CHANNEL_DEPTH: usize = 256;

/// Subscriber inbound channel depth — matches local-fs.
const SUBSCRIBER_CHANNEL_DEPTH: usize = 64;

/// LAN transport errors.
#[derive(Debug)]
pub enum LanTcpError {
    Io(std::io::Error),
    Json(serde_json::Error),
    TlsConfig(TlsConfigError),
    TlsHandshake(std::io::Error),
    /// Post-handshake peer-id binding failed: cert presented didn't
    /// resolve to a known peer.
    PeerNotInRegistry,
    /// Length prefix exceeded `MAX_FRAME_BYTES` — likely hostile or
    /// misconfigured.
    FrameTooLarge {
        announced: u32,
        limit: u32,
    },
    /// `send()` called with no connection established.
    NotConnected,
    /// `connect()` or `listen()` called twice on the same adapter.
    AlreadyHasConnection,
}

impl std::fmt::Display for LanTcpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LanTcpError::Io(error) => write!(f, "lan-tcp I/O: {error}"),
            LanTcpError::Json(error) => write!(f, "lan-tcp frame parse: {error}"),
            LanTcpError::TlsConfig(error) => write!(f, "lan-tcp TLS config: {error}"),
            LanTcpError::TlsHandshake(error) => write!(f, "lan-tcp TLS handshake: {error}"),
            LanTcpError::PeerNotInRegistry => write!(
                f,
                "post-handshake: peer cert pubkey is not in the registry (this should have been caught at handshake)"
            ),
            LanTcpError::FrameTooLarge { announced, limit } => write!(
                f,
                "lan-tcp refused frame with announced size {announced} bytes (limit {limit})"
            ),
            LanTcpError::NotConnected => {
                write!(f, "lan-tcp adapter has no active connection")
            }
            LanTcpError::AlreadyHasConnection => write!(
                f,
                "lan-tcp adapter already has an active connection — point-to-peer for MVP, PR-3d adds multi-peer"
            ),
        }
    }
}

impl std::error::Error for LanTcpError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            LanTcpError::Io(error) | LanTcpError::TlsHandshake(error) => Some(error),
            LanTcpError::Json(error) => Some(error),
            LanTcpError::TlsConfig(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for LanTcpError {
    fn from(error: std::io::Error) -> Self {
        LanTcpError::Io(error)
    }
}

impl From<serde_json::Error> for LanTcpError {
    fn from(error: serde_json::Error) -> Self {
        LanTcpError::Json(error)
    }
}

impl From<TlsConfigError> for LanTcpError {
    fn from(error: TlsConfigError) -> Self {
        LanTcpError::TlsConfig(error)
    }
}

/// Active connection's sender half — write-task receives Frames from
/// this and pushes them onto the TLS stream.
type OutboundTx = mpsc::Sender<Frame>;

/// One subscriber's filtered inbound channel + matching predicate.
struct SubscriberHandle {
    id: u64,
    subscription: Subscription,
    tx: mpsc::Sender<Result<Frame, LanTcpError>>,
}

struct Inner {
    self_peer_id: PeerId,
    keypair: PeerKeypair,
    registry: Arc<RwLock<PeerKeyRegistry>>,
    server_config: Arc<rustls::ServerConfig>,
    /// At most one active connection per adapter in MVP. PR-3d
    /// promotes this to `HashMap<PeerId, OutboundTx>`.
    connection: Mutex<Option<OutboundTx>>,
    subscribers: Mutex<Vec<SubscriberHandle>>,
    next_sub_id: AtomicU64,
}

/// Secure same-LAN adapter.
pub struct LanTcpAdapter {
    inner: Arc<Inner>,
}

impl LanTcpAdapter {
    /// Construct an adapter. No network activity yet — call
    /// `listen()` or `connect()` to establish a connection.
    pub fn new(
        self_peer_id: PeerId,
        keypair: PeerKeypair,
        registry: Arc<RwLock<PeerKeyRegistry>>,
    ) -> Result<Self, LanTcpError> {
        let server_config = build_server_config(self_peer_id, &keypair, registry.clone())?;
        Ok(Self {
            inner: Arc::new(Inner {
                self_peer_id,
                keypair,
                registry,
                server_config,
                connection: Mutex::new(None),
                subscribers: Mutex::new(Vec::new()),
                next_sub_id: AtomicU64::new(0),
            }),
        })
    }

    /// Bind a TCP listener and accept ONE incoming connection. The
    /// returned `SocketAddr` is the actual bound address (useful when
    /// `bind_addr.port() == 0` and the OS assigns).
    ///
    /// Internally spawns the accept loop + post-handshake read/write
    /// tasks. Returns after the listener is bound, NOT after a peer
    /// has connected — callers await delivery via `subscribe`.
    pub async fn listen(&self, bind_addr: SocketAddr) -> Result<SocketAddr, LanTcpError> {
        // Refuse if already connected; MVP is point-to-peer.
        {
            let conn = self.inner.connection.lock().await;
            if conn.is_some() {
                return Err(LanTcpError::AlreadyHasConnection);
            }
        }

        let listener = TcpListener::bind(bind_addr).await?;
        let actual = listener.local_addr()?;
        let inner = self.inner.clone();

        tokio::spawn(async move {
            // Accept ONE connection; subsequent attempts ignored in
            // MVP. PR-3d turns this into a multi-accept loop.
            match listener.accept().await {
                Ok((tcp_stream, _peer_addr)) => {
                    let acceptor = TlsAcceptor::from(inner.server_config.clone());
                    match acceptor.accept(tcp_stream).await {
                        Ok(tls_stream) => {
                            // Errors here mean the cert pubkey didn't
                            // resolve in the registry — the verifier
                            // should have caught this; if it didn't,
                            // we bail rather than installing a bogus
                            // connection.
                            let _ = handle_server_connection(inner, tls_stream).await;
                        }
                        Err(_error) => {
                            // Handshake rejected (e.g. unenrolled
                            // client cert). The pinned client
                            // verifier already refused; no further
                            // action needed.
                        }
                    }
                }
                Err(_error) => {
                    // Listener failed — no recovery in MVP.
                }
            }
        });

        Ok(actual)
    }

    /// Dial a peer over TLS. Returns after the TLS handshake completes
    /// successfully (or fails). Frames flow once subscribe + send are
    /// in use.
    pub async fn connect(
        &self,
        peer_addr: SocketAddr,
        expected_peer: PeerId,
    ) -> Result<(), LanTcpError> {
        {
            let conn = self.inner.connection.lock().await;
            if conn.is_some() {
                return Err(LanTcpError::AlreadyHasConnection);
            }
        }

        let client_config = build_client_config(
            self.inner.self_peer_id,
            &self.inner.keypair,
            expected_peer,
            self.inner.registry.clone(),
        )?;
        let connector = TlsConnector::from(client_config);

        let tcp_stream = TcpStream::connect(peer_addr).await?;

        // ServerName for SNI — uses the expected_peer's UUID string.
        // The pinned verifier ignores the server_name argument, so
        // this is purely for the protocol-level SNI extension.
        let server_name = rustls_pki_types::ServerName::try_from(expected_peer.to_string())
            .map_err(|e| {
                LanTcpError::TlsHandshake(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    e.to_string(),
                ))
            })?;

        let tls_stream = connector
            .connect(server_name, tcp_stream)
            .await
            .map_err(LanTcpError::TlsHandshake)?;

        // Install the outbound channel + spawn read/write loops
        // synchronously. By the time `connect()` returns, `send()` is
        // guaranteed to find an active connection — no sleep hack in
        // the caller.
        handle_client_connection(self.inner.clone(), tls_stream).await?;

        Ok(())
    }
}

/// Post-handshake server-side connection handler: bind the peer
/// identity from the cert, install the outbound channel
/// **synchronously**, spawn read + write loops. Returns Ok after the
/// outbound channel is installed so callers know send is ready.
async fn handle_server_connection(
    inner: Arc<Inner>,
    tls_stream: ServerTlsStream<TcpStream>,
) -> Result<(), LanTcpError> {
    let peer_id = resolve_peer_from_server_stream(&inner, &tls_stream)
        .ok_or(LanTcpError::PeerNotInRegistry)?;
    let (read_half, write_half) = tokio::io::split(tls_stream);
    install_and_spawn_loops(inner, peer_id, read_half, write_half).await;
    Ok(())
}

/// Post-handshake client-side connection handler. Same semantics as
/// `handle_server_connection` — installs the outbound channel before
/// returning so the caller's subsequent `send()` finds an active
/// connection.
async fn handle_client_connection(
    inner: Arc<Inner>,
    tls_stream: ClientTlsStream<TcpStream>,
) -> Result<(), LanTcpError> {
    let peer_id = resolve_peer_from_client_stream(&inner, &tls_stream)
        .ok_or(LanTcpError::PeerNotInRegistry)?;
    let (read_half, write_half) = tokio::io::split(tls_stream);
    install_and_spawn_loops(inner, peer_id, read_half, write_half).await;
    Ok(())
}

fn resolve_peer_from_server_stream(
    inner: &Arc<Inner>,
    tls_stream: &ServerTlsStream<TcpStream>,
) -> Option<PeerId> {
    let certs = tls_stream.get_ref().1.peer_certificates()?;
    let cert = certs.first()?;
    let pubkey = extract_ed25519_pubkey(cert).ok()?;
    let registry = inner.registry.read().ok()?;
    registry.find_peer(&pubkey).map(|(peer, _key_id)| peer)
}

fn resolve_peer_from_client_stream(
    inner: &Arc<Inner>,
    tls_stream: &ClientTlsStream<TcpStream>,
) -> Option<PeerId> {
    let certs = tls_stream.get_ref().1.peer_certificates()?;
    let cert = certs.first()?;
    let pubkey = extract_ed25519_pubkey(cert).ok()?;
    let registry = inner.registry.read().ok()?;
    registry.find_peer(&pubkey).map(|(peer, _key_id)| peer)
}

/// Install the outbound channel into `inner` **before** spawning the
/// read/write loops, then spawn them. This is the readiness-fix per
/// Codex's #671 review finding: previously the outbound channel was
/// set inside a spawned task, so callers (and the CLI) could call
/// `send()` after `connect()` returned and find no connection
/// installed yet. Now the install is awaited inline.
async fn install_and_spawn_loops<R, W>(
    inner: Arc<Inner>,
    peer_id: PeerId,
    read_half: R,
    write_half: W,
) where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let (outbound_tx, outbound_rx) = mpsc::channel::<Frame>(OUTBOUND_CHANNEL_DEPTH);
    // Install synchronously — when this function returns, the
    // connection IS ready to receive sends.
    *inner.connection.lock().await = Some(outbound_tx);

    // Spawn the I/O loops; they continue to drive the wire after
    // this function returns.
    tokio::spawn(write_loop(write_half, outbound_rx));
    tokio::spawn(read_loop(inner, peer_id, read_half));
}

/// Read loop: pull length-prefixed JSON frames off the TLS stream
/// and fan out to subscribers per the Transport trait's lag policy.
async fn read_loop<R>(inner: Arc<Inner>, _peer_id: PeerId, mut read_half: R)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    loop {
        // Read 4-byte BE length prefix.
        let mut len_bytes = [0u8; 4];
        if read_half.read_exact(&mut len_bytes).await.is_err() {
            // Connection closed (clean EOF or error). Drop the
            // outbound channel so senders see "not connected."
            *inner.connection.lock().await = None;
            return;
        }
        let len = u32::from_be_bytes(len_bytes);
        if len > MAX_FRAME_BYTES {
            // Hostile or misconfigured peer — drop the connection.
            *inner.connection.lock().await = None;
            return;
        }
        let mut payload = vec![0u8; len as usize];
        if read_half.read_exact(&mut payload).await.is_err() {
            *inner.connection.lock().await = None;
            return;
        }

        let frame: Frame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(_error) => {
                // Malformed payload — drop the connection rather
                // than silently skipping.
                *inner.connection.lock().await = None;
                return;
            }
        };

        // Fan out to subscribers; same lag policy as local-fs.
        dispatch_to_subscribers(&inner, frame).await;
    }
}

/// Fan out a received frame to all matching subscribers.
///
/// Per Codex's #671 review finding: previously we held the subscribers
/// mutex across `.send().await` for Message/Control kinds, which lets
/// one slow subscriber block dispatch to all others AND prevent new
/// subscribers from registering. Fix: snapshot the matching sender
/// handles under the lock, drop the lock, then await sends.
///
/// Trade-off: cloning the senders + frame for the snapshot costs a
/// bit of memory per dispatch, but `Sender` clones are cheap (Arc
/// underneath) and the frame clone was happening per-subscriber
/// before anyway.
async fn dispatch_to_subscribers(inner: &Arc<Inner>, frame: Frame) {
    // 1. Snapshot under lock — drop the lock as soon as we have the
    //    sender handles we need.
    type Target = (u64, mpsc::Sender<Result<Frame, LanTcpError>>);
    let targets: Vec<Target> = {
        let subs = inner.subscribers.lock().await;
        subs.iter()
            .filter(|sub| subscription_matches_with_cursor(&sub.subscription, &frame))
            .map(|sub| (sub.id, sub.tx.clone()))
            .collect()
    };

    // 2. Dispatch outside the lock — slow consumers no longer block
    //    other subscribers or new registrations.
    let mut dead_ids: Vec<u64> = Vec::new();
    for (id, tx) in targets {
        let send_result = match frame.kind {
            FrameKind::Message | FrameKind::Control => {
                // Backpressure-bearing — block on slow consumer.
                tx.send(Ok(frame.clone())).await.map_err(|_| ())
            }
            FrameKind::Event => {
                // Lossy — drop on full subscriber buffer.
                match tx.try_send(Ok(frame.clone())) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
                    Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
                }
            }
        };
        if send_result.is_err() {
            dead_ids.push(id);
        }
    }

    // 3. Reap dead subscribers under a brief lock.
    if !dead_ids.is_empty() {
        let mut subs = inner.subscribers.lock().await;
        subs.retain(|sub| !dead_ids.contains(&sub.id));
    }
}

/// Cursor predicate (mirrors local-fs's check) — frames at or before
/// the cursor are skipped for replay subscribers.
fn subscription_matches_with_cursor(sub: &Subscription, frame: &Frame) -> bool {
    if !sub.matches(frame) {
        return false;
    }
    if let Some(cursor) = &sub.from_cursor {
        let envelope = &frame.envelope;
        let after = envelope.lamport > cursor.lamport
            || (envelope.lamport == cursor.lamport
                && envelope.event_id.as_uuid() > cursor.event_id.as_uuid());
        if !after {
            return false;
        }
    }
    true
}

/// Write loop: drain the outbound channel, write framed bytes to TLS.
async fn write_loop<W>(mut write_half: W, mut outbound_rx: mpsc::Receiver<Frame>)
where
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    while let Some(frame) = outbound_rx.recv().await {
        let payload = match serde_json::to_vec(&frame) {
            Ok(bytes) => bytes,
            Err(_) => continue, // skip malformed frame
        };
        if payload.len() > MAX_FRAME_BYTES as usize {
            // Drop oversized frames — caller should be lifting bodies.
            continue;
        }
        let len = (payload.len() as u32).to_be_bytes();
        if write_half.write_all(&len).await.is_err() {
            return;
        }
        if write_half.write_all(&payload).await.is_err() {
            return;
        }
        if write_half.flush().await.is_err() {
            return;
        }
    }
}

#[async_trait]
impl Transport for LanTcpAdapter {
    type Error = LanTcpError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        let conn = self.inner.connection.lock().await;
        let tx = conn.as_ref().ok_or(LanTcpError::NotConnected)?;
        tx.send(frame).await.map_err(|_| LanTcpError::NotConnected)
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_DEPTH);
        let id = self.inner.next_sub_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut subs = self.inner.subscribers.lock().await;
            subs.push(SubscriberHandle {
                id,
                subscription,
                tx,
            });
        }
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{
        headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
    };
    use airc_protocol::{ChannelId, Envelope, Frame, FrameKind, Signature, Subscription};
    use futures::stream::StreamExt;
    use std::time::Duration;

    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    fn frame_at(lamport: u64, channel: ChannelId, body: &str) -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id: EventId::from_u128(lamport as u128),
                sender: PeerId::from_u128(0xa1),
                sender_client: ClientId::from_u128(0xc1),
                channel,
                target: MentionTarget::All,
                lamport,
                occurred_at_ms: 1_700_000_000_000 + lamport,
                reply_to: None,
                headers: Headers::new(),
                body: Some(Body::text(body)),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    /// Build (Alice, Bob) — two peers, each with a keypair, both
    /// enrolled in a shared registry. Returns (peer_id_a, adapter_a,
    /// peer_id_b, adapter_b).
    fn make_paired_adapters() -> (PeerId, LanTcpAdapter, PeerId, LanTcpAdapter) {
        ensure_crypto_provider();
        let alice_id = PeerId::from_u128(0xa1);
        let bob_id = PeerId::from_u128(0xb2);
        let alice_kp = PeerKeypair::generate();
        let bob_kp = PeerKeypair::generate();

        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(alice_id, 0, alice_kp.public_bytes())
            .unwrap();
        registry.enrol(bob_id, 0, bob_kp.public_bytes()).unwrap();
        let registry = Arc::new(RwLock::new(registry));

        let alice = LanTcpAdapter::new(alice_id, alice_kp, registry.clone()).unwrap();
        let bob = LanTcpAdapter::new(bob_id, bob_kp, registry).unwrap();
        (alice_id, alice, bob_id, bob)
    }

    #[tokio::test]
    async fn two_paired_peers_round_trip_a_message() {
        // The core e2e test: Alice listens, Bob dials. TLS handshake
        // succeeds (both enrolled). Alice sends a Message frame; Bob
        // receives it via subscribe.
        let (alice_id, alice, _bob_id, bob) = make_paired_adapters();

        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let bound = alice.listen(loopback).await.unwrap();

        // Bob subscribes BEFORE the connection completes so the
        // arrival is observable.
        let mut bob_stream = bob
            .subscribe(Subscription {
                channel: None,
                ..Default::default()
            })
            .await
            .unwrap();

        bob.connect(bound, alice_id).await.unwrap();

        // Give the post-handshake spawn tasks a moment to install
        // the connection handle on Alice's side.
        tokio::time::sleep(Duration::from_millis(100)).await;

        let channel = RoomId::from_u128(0xc0ffee);
        alice
            .send(frame_at(1, channel, "hello from alice"))
            .await
            .unwrap();

        let received = tokio::time::timeout(Duration::from_secs(3), bob_stream.next())
            .await
            .expect("must yield within 3s")
            .expect("stream must yield Some")
            .expect("frame must parse");
        assert_eq!(received.envelope.lamport, 1);
        assert_eq!(
            received
                .envelope
                .body
                .as_ref()
                .and_then(Body::as_text)
                .unwrap(),
            "hello from alice"
        );
    }

    #[tokio::test]
    async fn unenrolled_peer_cannot_handshake() {
        // The security guard: an unenrolled peer's dial is rejected
        // at the TLS handshake — no frames can ever flow.
        ensure_crypto_provider();
        let alice_id = PeerId::from_u128(0xa1);
        let alice_kp = PeerKeypair::generate();
        // Alice's registry has ONLY herself enrolled.
        let mut alice_registry = PeerKeyRegistry::new();
        alice_registry
            .enrol(alice_id, 0, alice_kp.public_bytes())
            .unwrap();
        let alice_registry = Arc::new(RwLock::new(alice_registry));
        let alice = LanTcpAdapter::new(alice_id, alice_kp, alice_registry).unwrap();

        // Stranger: not in Alice's registry. Has Alice in HER
        // registry though (otherwise she couldn't verify Alice's
        // server cert).
        let stranger_id = PeerId::from_u128(0xdeadbeef);
        let stranger_kp = PeerKeypair::generate();
        // Stranger needs Alice's pubkey to verify Alice's server cert;
        // we extract it directly here. The reverse direction — Alice
        // accepting stranger — is what this test actually pins.
        let alice_pub_for_stranger: [u8; 32] = {
            use crate::lan_tcp::cert::{extract_ed25519_pubkey, generate_self_signed_cert};
            let (cert, _) = generate_self_signed_cert(
                &PeerKeypair::from_secret_bytes(&alice.inner.keypair.secret_bytes()),
                alice_id,
            )
            .unwrap();
            extract_ed25519_pubkey(&cert).unwrap()
        };
        let mut stranger_registry = PeerKeyRegistry::new();
        stranger_registry
            .enrol(alice_id, 0, alice_pub_for_stranger)
            .unwrap();
        stranger_registry
            .enrol(stranger_id, 0, stranger_kp.public_bytes())
            .unwrap();
        let stranger_registry = Arc::new(RwLock::new(stranger_registry));
        let stranger = LanTcpAdapter::new(stranger_id, stranger_kp, stranger_registry).unwrap();

        let bound = alice
            .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();

        // Stranger dials Alice. Alice's `PinnedClientVerifier` rejects
        // the stranger's cert (unenrolled). Note that the *client's*
        // `connect()` may return `Ok` because the TLS handshake
        // completes from the client side before the server's
        // CertVerify rejection is fully propagated — that's a TLS
        // protocol property, not a substrate bug. The substrate
        // guarantee is that no frames can flow: Alice never installs
        // a connection, so a subsequent `alice.send()` MUST fail with
        // `NotConnected`.
        let _ = stranger.connect(bound, alice_id).await;
        // Give the server side a moment to observe the rejected
        // handshake and tear the connection down.
        tokio::time::sleep(Duration::from_millis(300)).await;

        let channel = RoomId::from_u128(0xc0ffee);
        let send_result = alice
            .send(frame_at(1, channel, "should never arrive"))
            .await;
        assert!(
            matches!(send_result, Err(LanTcpError::NotConnected)),
            "Alice must have no installed connection after refusing stranger's handshake; got {send_result:?}"
        );
    }

    #[tokio::test]
    async fn send_without_connection_returns_not_connected() {
        // Sending before listen/connect should surface a typed error,
        // not panic or block forever.
        let (_alice_id, alice, _bob_id, _bob) = make_paired_adapters();
        let channel = RoomId::from_u128(0xc0ffee);
        let result = alice.send(frame_at(1, channel, "into the void")).await;
        assert!(matches!(result, Err(LanTcpError::NotConnected)));
    }
}
