//! `LanTcpAdapter` — secure same-LAN wire over TLS-wrapped TCP.
//!
//! Module layout (one concern per file):
//!   - [`error`] — `LanTcpError` and `From` impls
//!   - [`inner`] — shared state (Inner, SubscriberHandle, constants)
//!   - [`connection`] — per-connection lifecycle (accept/dial →
//!     resolve peer-id → install outbound → spawn read/write loops)
//!   - [`dispatch`] — subscriber fan-out (snapshot-then-await pattern
//!     so a slow consumer doesn't block other subscribers)
//!   - `mod.rs` (this file) — `LanTcpAdapter` public surface +
//!     `Transport` impl + integration tests
//!
//! Scope:
//!   - **Multi-peer**: N concurrent connections, keyed by `PeerId`.
//!     `listen()` accepts indefinitely; `connect()` allows multiple
//!     dials to different peers; `send()` broadcasts to all.
//!   - **Multi-subscriber fan-out**: one adapter, many local
//!     `Transport::subscribe` callers.
//!   - Length-prefixed JSON frames over TLS.
//!   - FrameKind delivery split inherited from the Transport trait:
//!     Message/Control durable+backpressure, Event lossy.
//!   - Mutual TLS via `tls_config` builders; peer identity bound
//!     post-handshake by reading the peer cert and looking up via
//!     `PeerKeyRegistry::find_peer`.
//!
//! What's deferred:
//!   - mDNS/discovery (separate concern).
//!   - Reconnect / retry (robustness PR).
//!   - Per-frame ack/replay-cursor (bridge layer).
//!   - `send_to(peer_id, frame)` for targeted unicast.

mod connection;
mod dispatch;
mod error;
mod inner;

pub use error::LanTcpError;

use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::sync::RwLock;

use async_trait::async_trait;
use futures::stream::Stream;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::{TlsAcceptor, TlsConnector};

use airc_core::PeerId;
use airc_protocol::{Frame, PeerKeyRegistry, PeerKeypair, Subscription};

use crate::lan_tcp::adapter::connection::{handle_client_connection, handle_server_connection};
use crate::lan_tcp::adapter::inner::{
    Inner, OutboundTx, SubscriberHandle, MAX_FRAME_BYTES, SUBSCRIBER_CHANNEL_DEPTH,
};
use crate::lan_tcp::tls_config::{build_client_config, build_server_config};
use crate::transport::{FrameStream, Transport};

/// Secure same-LAN adapter.
#[derive(Clone)]
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
                connections: Mutex::new(HashMap::new()),
                listening: Mutex::new(false),
                subscribers: Mutex::new(Vec::new()),
                next_sub_id: AtomicU64::new(0),
            }),
        })
    }

    /// Snapshot of currently-connected peers. Useful for diagnostics
    /// (`airc-core peers`) + tests.
    pub async fn connected_peers(&self) -> Vec<PeerId> {
        self.inner
            .connections
            .lock()
            .await
            .keys()
            .copied()
            .collect()
    }

    /// Bind a TCP listener and accept incoming connections
    /// indefinitely. The returned `SocketAddr` is the actual bound
    /// address (useful when `bind_addr.port() == 0` and the OS
    /// assigns).
    ///
    /// Returns after the listener is bound, NOT after a peer has
    /// connected — callers await delivery via `subscribe`.
    pub async fn listen(&self, bind_addr: SocketAddr) -> Result<SocketAddr, LanTcpError> {
        {
            let mut listening = self.inner.listening.lock().await;
            if *listening {
                return Err(LanTcpError::AlreadyListening);
            }
            *listening = true;
        }

        let listener = TcpListener::bind(bind_addr).await?;
        let actual = listener.local_addr()?;
        let inner = self.inner.clone();

        tokio::spawn(async move {
            // Accept indefinitely — each accepted peer gets its own
            // per-connection task. The accept loop runs until the
            // listener errors (e.g. the adapter drops the OS socket).
            loop {
                let (tcp_stream, _peer_addr) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_error) => return,
                };
                let inner_for_conn = inner.clone();
                let acceptor = TlsAcceptor::from(inner.server_config.clone());
                tokio::spawn(async move {
                    match acceptor.accept(tcp_stream).await {
                        Ok(tls_stream) => {
                            let _ = handle_server_connection(inner_for_conn, tls_stream).await;
                        }
                        Err(_error) => {
                            // Handshake rejected (e.g. unenrolled
                            // client cert). The pinned verifier
                            // already refused; no further action.
                        }
                    }
                });
            }
        });

        Ok(actual)
    }

    /// Dial a peer over TLS. Returns after the TLS handshake completes
    /// and the outbound channel is installed — subsequent `send()`
    /// calls find an active connection without races.
    pub async fn connect(
        &self,
        peer_addr: SocketAddr,
        expected_peer: PeerId,
    ) -> Result<(), LanTcpError> {
        {
            let connections = self.inner.connections.lock().await;
            if connections.contains_key(&expected_peer) {
                return Err(LanTcpError::AlreadyConnectedTo(expected_peer));
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

        // Install outbound + spawn loops synchronously before returning.
        handle_client_connection(self.inner.clone(), tls_stream).await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for LanTcpAdapter {
    type Error = LanTcpError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        // Pre-validate BEFORE enqueueing so any failure (serialize
        // error, oversized payload) returns synchronously rather
        // than being silently dropped by the write loop after the
        // caller already saw Ok. Grievance §9: `send()` must mean
        // something measurable.
        let payload = serde_json::to_vec(&frame)?;
        if payload.len() > MAX_FRAME_BYTES as usize {
            return Err(LanTcpError::FrameTooLarge {
                announced: u32::try_from(payload.len()).unwrap_or(u32::MAX),
                limit: MAX_FRAME_BYTES,
            });
        }

        // Snapshot under lock, drop, dispatch — never hold the
        // connections mutex across awaits.
        let targets: Vec<(PeerId, OutboundTx)> = {
            let connections = self.inner.connections.lock().await;
            if connections.is_empty() {
                return Err(LanTcpError::NoActivePeers);
            }
            connections
                .iter()
                .map(|(peer, tx)| (*peer, tx.clone()))
                .collect()
        };

        // Broadcast: send to each connected peer. Per-peer failures
        // tolerated (peer may have dropped between snapshot + send;
        // the read loop will GC the dead entry). Success if at least
        // one peer received.
        let mut delivered_any = false;
        let mut last_error: Option<LanTcpError> = None;
        for (_peer, tx) in targets {
            match tx.send(payload.clone()).await {
                Ok(()) => delivered_any = true,
                Err(_closed) => {
                    last_error = Some(LanTcpError::NoActivePeers);
                }
            }
        }
        if delivered_any {
            Ok(())
        } else {
            Err(last_error.unwrap_or(LanTcpError::NoActivePeers))
        }
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
        let (alice_id, alice, _bob_id, bob) = make_paired_adapters();
        let loopback = SocketAddr::from(([127, 0, 0, 1], 0));
        let bound = alice.listen(loopback).await.unwrap();

        let mut bob_stream = bob
            .subscribe(Subscription {
                channel: None,
                ..Default::default()
            })
            .await
            .unwrap();

        bob.connect(bound, alice_id).await.unwrap();
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
        // Security guarantee: a peer not in Alice's registry cannot
        // get a connection installed; a subsequent alice.send() must
        // fail NoActivePeers.
        ensure_crypto_provider();
        let alice_id = PeerId::from_u128(0xa1);
        let alice_kp = PeerKeypair::generate();
        let mut alice_registry = PeerKeyRegistry::new();
        alice_registry
            .enrol(alice_id, 0, alice_kp.public_bytes())
            .unwrap();
        let alice_registry = Arc::new(RwLock::new(alice_registry));
        let alice = LanTcpAdapter::new(alice_id, alice_kp, alice_registry).unwrap();

        let stranger_id = PeerId::from_u128(0xdeadbeef);
        let stranger_kp = PeerKeypair::generate();
        // Stranger needs Alice's pubkey to verify Alice's server
        // cert. Extract directly.
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

        // Stranger's dial may return Ok (TLS handshake completes on
        // the client side before the server-side rejection
        // propagates), but Alice never installs the connection.
        let _ = stranger.connect(bound, alice_id).await;
        tokio::time::sleep(Duration::from_millis(300)).await;

        let channel = RoomId::from_u128(0xc0ffee);
        let send_result = alice
            .send(frame_at(1, channel, "should never arrive"))
            .await;
        assert!(
            matches!(send_result, Err(LanTcpError::NoActivePeers)),
            "Alice must have no installed connection after refusing stranger's handshake; got {send_result:?}"
        );
    }

    #[tokio::test]
    async fn three_peers_all_connect_and_alice_broadcasts() {
        // Multi-peer contract: Alice listens; Bob + Charlie both
        // dial; Alice sees both connected; broadcast reaches both.
        ensure_crypto_provider();
        let alice_id = PeerId::from_u128(0xa1);
        let bob_id = PeerId::from_u128(0xb2);
        let charlie_id = PeerId::from_u128(0xcc);
        let alice_kp = PeerKeypair::generate();
        let bob_kp = PeerKeypair::generate();
        let charlie_kp = PeerKeypair::generate();

        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(alice_id, 0, alice_kp.public_bytes())
            .unwrap();
        registry.enrol(bob_id, 0, bob_kp.public_bytes()).unwrap();
        registry
            .enrol(charlie_id, 0, charlie_kp.public_bytes())
            .unwrap();
        let registry = Arc::new(RwLock::new(registry));

        let alice = LanTcpAdapter::new(alice_id, alice_kp, registry.clone()).unwrap();
        let bob = LanTcpAdapter::new(bob_id, bob_kp, registry.clone()).unwrap();
        let charlie = LanTcpAdapter::new(charlie_id, charlie_kp, registry).unwrap();

        let bound = alice
            .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();

        let mut bob_stream = bob
            .subscribe(Subscription {
                channel: None,
                ..Default::default()
            })
            .await
            .unwrap();
        let mut charlie_stream = charlie
            .subscribe(Subscription {
                channel: None,
                ..Default::default()
            })
            .await
            .unwrap();

        bob.connect(bound, alice_id).await.unwrap();
        charlie.connect(bound, alice_id).await.unwrap();
        tokio::time::sleep(Duration::from_millis(200)).await;

        let alice_peers = alice.connected_peers().await;
        assert_eq!(alice_peers.len(), 2);
        assert!(alice_peers.contains(&bob_id));
        assert!(alice_peers.contains(&charlie_id));

        let channel = RoomId::from_u128(0xc0ffee);
        alice
            .send(frame_at(1, channel, "broadcast to all"))
            .await
            .unwrap();

        let recv_bob = tokio::time::timeout(Duration::from_secs(3), bob_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let recv_charlie = tokio::time::timeout(Duration::from_secs(3), charlie_stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(recv_bob.envelope.lamport, 1);
        assert_eq!(recv_charlie.envelope.lamport, 1);
    }

    #[tokio::test]
    async fn connect_to_same_peer_twice_returns_typed_error() {
        let (alice_id, alice, _bob_id, bob) = make_paired_adapters();
        let bound = alice
            .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        bob.connect(bound, alice_id).await.unwrap();
        let second = bob.connect(bound, alice_id).await;
        assert!(
            matches!(second, Err(LanTcpError::AlreadyConnectedTo(peer)) if peer == alice_id),
            "expected AlreadyConnectedTo(alice), got {second:?}"
        );
    }

    #[tokio::test]
    async fn send_with_no_active_peers_returns_typed_error() {
        let (_alice_id, alice, _bob_id, _bob) = make_paired_adapters();
        let channel = RoomId::from_u128(0xc0ffee);
        let result = alice.send(frame_at(1, channel, "into the void")).await;
        assert!(matches!(result, Err(LanTcpError::NoActivePeers)));
    }

    #[tokio::test]
    async fn send_oversized_frame_returns_err_synchronously_without_dispatch() {
        // The defensible proof for grievance §9 / "no post-acceptance
        // silent drops": when the frame's serialized form exceeds the
        // per-frame size cap, `send()` must surface `FrameTooLarge`
        // *before* enqueueing — so the caller learns about the
        // failure rather than seeing Ok and never observing the drop
        // on the wire. The bob-side never receives the frame.
        let (alice_id, alice, bob_id, bob) = make_paired_adapters();
        let bound = alice
            .listen(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .unwrap();
        bob.connect(bound, alice_id).await.unwrap();

        let mut bob_stream = bob.subscribe(Subscription::default()).await.unwrap();

        // Build a frame whose serialized JSON is guaranteed past the
        // 16 MiB cap: a 17 MiB ASCII body alone exceeds the limit.
        let oversized_body = "a".repeat((MAX_FRAME_BYTES as usize) + 1024 * 1024);
        let channel = RoomId::from_u128(0xc0ffee);
        let huge_frame = frame_at(1, channel, &oversized_body);

        let send_result = alice.send(huge_frame).await;
        assert!(
            matches!(
                send_result,
                Err(LanTcpError::FrameTooLarge { announced, limit })
                    if announced > limit && limit == MAX_FRAME_BYTES
            ),
            "expected FrameTooLarge with announced > limit, got {send_result:?}"
        );

        // Cross-check: bob's subscriber must not see the frame
        // (because send() rejected it before enqueueing). Give the
        // network a generous tick to surface anything in flight.
        let bob_saw = tokio::time::timeout(Duration::from_millis(200), bob_stream.next()).await;
        assert!(
            bob_saw.is_err(),
            "bob must not have received an oversized frame on the wire — got {bob_saw:?}"
        );

        // peer_id parameter is otherwise unused but the assertion is
        // here to make the test's intent obvious in the diff.
        let _ = bob_id;
    }
}
