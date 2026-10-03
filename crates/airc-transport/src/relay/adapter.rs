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

/// Called once when a live relay session ends (read or write side), with the
/// relay's peer id, so the owner can redial at once instead of on its next tick.
pub type RelayDisconnectObserver = Arc<dyn Fn(airc_core::PeerId) + Send + Sync>;

struct Inner {
    config: RelayClientConfig,
    /// Outbound channel sender — `Some` only after [`connect`] has
    /// installed the write loop.
    outbound: Mutex<Option<mpsc::Sender<Vec<u8>>>>,
    subscribers: Mutex<Vec<SubscriberHandle>>,
    next_sub_id: AtomicU64,
    on_disconnect: std::sync::Mutex<Option<RelayDisconnectObserver>>,
    /// The session's read and write loops. `close` aborts them, which drops both TLS
    /// halves and so closes the socket; clearing the sender alone left the read loop,
    /// and the connection, alive.
    loops: std::sync::Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

#[derive(Clone)]
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
                on_disconnect: std::sync::Mutex::new(None),
                loops: std::sync::Mutex::new(Vec::new()),
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

        let writer = tokio::spawn(write_loop(Arc::clone(&self.inner), write_half, outbound_rx));
        let reader = tokio::spawn(read_loop(Arc::clone(&self.inner), read_half));
        if let Ok(mut loops) = self.inner.loops.lock() {
            loops.extend([writer, reader]);
        }

        Ok(())
    }

    /// Whether the session is live: `false` once either loop saw the connection end.
    /// A closed adapter is never reused — the owner dials a fresh one.
    pub async fn is_connected(&self) -> bool {
        self.inner.outbound.lock().await.is_some()
    }

    pub fn relay_addr(&self) -> std::net::SocketAddr {
        self.inner.config.relay_addr
    }

    pub fn relay_peer_id(&self) -> airc_core::PeerId {
        self.inner.config.relay_peer_id
    }

    /// End this session without announcing a disconnect, for an adapter its owner
    /// decided not to install (a concurrent dial won): the loops are aborted, so both
    /// TLS halves drop and the socket closes.
    pub async fn close(&self) {
        self.inner.outbound.lock().await.take();
        let loops = self
            .inner
            .loops
            .lock()
            .map(|mut loops| std::mem::take(&mut *loops))
            .unwrap_or_default();
        for task in loops {
            task.abort();
        }
    }

    /// Register the callback fired once when this session ends. A later call replaces it.
    pub fn set_disconnect_observer(&self, observer: RelayDisconnectObserver) {
        if let Ok(mut slot) = self.inner.on_disconnect.lock() {
            *slot = Some(observer);
        }
    }
}

/// End the session: later sends surface `NotConnected`, and the owner hears about it
/// exactly once (whichever loop notices first).
async fn close_session(inner: &Arc<Inner>) {
    let was_live = inner.outbound.lock().await.take().is_some();
    if !was_live {
        return;
    }
    let observer = inner
        .on_disconnect
        .lock()
        .ok()
        .and_then(|slot| slot.clone());
    if let Some(observer) = observer {
        observer(inner.config.relay_peer_id);
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
            close_session(&inner).await;
            return;
        }
        let len = u32::from_be_bytes(len_bytes);
        if len > MAX_FRAME_BYTES {
            close_session(&inner).await;
            return;
        }
        let mut payload = vec![0u8; len as usize];
        if read_half.read_exact(&mut payload).await.is_err() {
            close_session(&inner).await;
            return;
        }
        let frame: Frame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(_) => {
                close_session(&inner).await;
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

async fn write_loop<W>(
    inner: Arc<Inner>,
    mut write_half: W,
    mut outbound_rx: mpsc::Receiver<Vec<u8>>,
) where
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    while let Some(payload) = outbound_rx.recv().await {
        let len = (payload.len() as u32).to_be_bytes();
        let written = async {
            write_half.write_all(&len).await?;
            write_half.write_all(&payload).await?;
            write_half.flush().await
        };
        if written.await.is_err() {
            // A write that fails is a dead session even if the read side has not
            // noticed yet; without this the adapter read as connected forever.
            close_session(&inner).await;
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
