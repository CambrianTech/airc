//! `RelayAdapter` — one outbound TLS connection to a relay server,
//! with the same length-prefixed JSON frame format as `lan_tcp`.
//!
//! Lifecycle:
//!   1. [`RelayAdapter::new`] — store config; no I/O.
//!   2. [`RelayAdapter::connect`] — TLS dial the relay, install
//!      outbound channel synchronously, spawn read + write loops.
//!   3. `Transport::send` / `Transport::subscribe` — usable.
//!
//! Frame routing on the wire: this adapter sends the EXACT serialized
//! envelope bytes to the relay; the relay does NOT re-sign. Receivers
//! verify signatures against canonical envelope bytes — the relay
//! cannot tamper without breaking signature verification at the
//! recipient.

use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::Stream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsConnector;

use airc_protocol::{Frame, Subscription};

use crate::lan_tcp::build_client_config;
use crate::relay::config::RelayClientConfig;
use crate::relay::error::RelayClientError;
use crate::transport::{FrameStream, Transport};

/// Per-frame wire limit — same as `lan_tcp`.
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;
const OUTBOUND_CHANNEL_DEPTH: usize = 256;
const SUBSCRIBER_CHANNEL_DEPTH: usize = 64;

struct SubscriberHandle {
    /// Monotonic id retained for future use (subscription unregister
    /// on stream drop is a follow-up; lan_tcp has the same shape).
    #[allow(dead_code)]
    id: u64,
    subscription: Subscription,
    tx: mpsc::Sender<Result<Frame, RelayClientError>>,
}

struct Inner {
    config: RelayClientConfig,
    /// Outbound channel sender — `Some` only after [`connect`] has
    /// installed the write loop.
    outbound: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    subscribers: Mutex<Vec<SubscriberHandle>>,
    next_sub_id: AtomicU64,
}

pub struct RelayAdapter {
    inner: Arc<Inner>,
}

impl RelayAdapter {
    pub fn new(config: RelayClientConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                outbound: Mutex::new(None),
                subscribers: Mutex::new(Vec::new()),
                next_sub_id: AtomicU64::new(0),
            }),
        }
    }

    /// Dial the relay, perform mTLS handshake (relay pubkey pinned via
    /// `registry`), install the outbound channel synchronously, spawn
    /// read + write loops. After this returns Ok, `send` and
    /// `subscribe` are usable.
    pub async fn connect(&self) -> Result<(), RelayClientError> {
        let mut guard = self.inner.outbound.lock().await;
        if guard.is_some() {
            return Err(RelayClientError::AlreadyConnected);
        }

        let client_config = build_client_config(
            self.inner.config.self_peer_id,
            &self.inner.config.self_keypair,
            self.inner.config.relay_peer_id,
            Arc::clone(&self.inner.config.registry),
        )?;
        let connector = TlsConnector::from(client_config);

        let tcp = TcpStream::connect(self.inner.config.relay_addr).await?;
        // The relay's DNS name in its cert is `<relay_peer_id>.airc.local`
        // (cf. lan_tcp `generate_self_signed_cert`). rustls requires a
        // SNI / server-name on the client side that matches the cert SAN.
        let server_name = relay_server_name(self.inner.config.relay_peer_id)?;
        let tls = connector
            .connect(server_name, tcp)
            .await
            .map_err(|e| RelayClientError::Io(std::io::Error::other(e)))?;

        let (read_half, write_half) = tokio::io::split(tls);
        let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_CHANNEL_DEPTH);

        *guard = Some(outbound_tx);
        drop(guard);

        tokio::spawn(write_loop(write_half, outbound_rx));
        tokio::spawn(read_loop(Arc::clone(&self.inner), read_half));

        Ok(())
    }
}

fn relay_server_name(
    relay_peer_id: airc_core::PeerId,
) -> Result<rustls::pki_types::ServerName<'static>, RelayClientError> {
    // The relay's self-signed cert SANs are produced by
    // `airc_transport::lan_tcp::cert::generate_self_signed_cert`, which
    // emits a single DNS name of the form `<peer-id>.airc.local`.
    let host = format!("{}.airc.local", relay_peer_id);
    rustls::pki_types::ServerName::try_from(host)
        .map_err(|error| RelayClientError::InvalidServerName(error.to_string()))
}

async fn read_loop<R>(inner: Arc<Inner>, mut read_half: R)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    loop {
        let mut len_bytes = [0u8; 4];
        if read_half.read_exact(&mut len_bytes).await.is_err() {
            // Connection closed — drop outbound so subsequent sends
            // surface NotConnected.
            *inner.outbound.lock().await = None;
            return;
        }
        let len = u32::from_be_bytes(len_bytes);
        if len > MAX_FRAME_BYTES {
            *inner.outbound.lock().await = None;
            return;
        }
        let mut payload = vec![0u8; len as usize];
        if read_half.read_exact(&mut payload).await.is_err() {
            *inner.outbound.lock().await = None;
            return;
        }
        let frame: Frame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(_) => {
                *inner.outbound.lock().await = None;
                return;
            }
        };
        dispatch(&inner, frame).await;
    }
}

async fn dispatch(inner: &Arc<Inner>, frame: Frame) {
    // Snapshot the matching subscribers then release the lock before
    // awaiting on each. A slow subscriber slows only itself.
    let snapshot: Vec<(usize, mpsc::Sender<Result<Frame, RelayClientError>>)> = {
        let subs = inner.subscribers.lock().await;
        subs.iter()
            .enumerate()
            .filter(|(_, h)| h.subscription.matches(&frame))
            .map(|(idx, h)| (idx, h.tx.clone()))
            .collect()
    };
    for (_idx, tx) in snapshot {
        match frame.kind {
            airc_protocol::FrameKind::Event => {
                // Lossy: drop on full per the Transport trait.
                let _ = tx.try_send(Ok(frame.clone()));
            }
            airc_protocol::FrameKind::Message | airc_protocol::FrameKind::Control => {
                // Durable: backpressure (await capacity).
                let _ = tx.send(Ok(frame.clone())).await;
            }
        }
    }
}

async fn write_loop<W>(mut write_half: W, mut outbound_rx: mpsc::Receiver<Vec<u8>>)
where
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    while let Some(payload) = outbound_rx.recv().await {
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
impl Transport for RelayAdapter {
    type Error = RelayClientError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        let bytes = serde_json::to_vec(&frame)?;
        if bytes.len() > MAX_FRAME_BYTES as usize {
            return Err(RelayClientError::FrameTooLarge {
                actual: bytes.len(),
                limit: MAX_FRAME_BYTES,
            });
        }
        let tx = {
            let guard = self.inner.outbound.lock().await;
            guard
                .as_ref()
                .ok_or(RelayClientError::NotConnected)?
                .clone()
        };
        tx.send(bytes)
            .await
            .map_err(|_| RelayClientError::ConnectionClosed)?;
        Ok(())
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        let (tx, rx) = mpsc::channel::<Result<Frame, RelayClientError>>(SUBSCRIBER_CHANNEL_DEPTH);
        let id = self.inner.next_sub_id.fetch_add(1, Ordering::Relaxed);
        self.inner.subscribers.lock().await.push(SubscriberHandle {
            id,
            subscription,
            tx,
        });
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}
