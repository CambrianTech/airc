use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::Stream;
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, Mutex, RwLock};

use airc_core::transcript::MentionTarget;
use airc_protocol::{Frame, FrameKind, Subscription};

use crate::transport::{FrameStream, Transport};
use crate::udp::config::UdpConfig;
use crate::udp::error::UdpError;

const MAX_DATAGRAM_BYTES: usize = 60 * 1024;
const SUBSCRIBER_CHANNEL_DEPTH: usize = 64;

struct SubscriberHandle {
    #[allow(dead_code)]
    id: u64,
    subscription: Subscription,
    tx: mpsc::Sender<Result<Frame, UdpError>>,
}

struct Inner {
    config: UdpConfig,
    socket: RwLock<Option<Arc<UdpSocket>>>,
    subscribers: Mutex<Vec<SubscriberHandle>>,
    next_sub_id: AtomicU64,
}

/// Low-latency UDP frame adapter.
#[derive(Clone)]
pub struct UdpAdapter {
    inner: Arc<Inner>,
}

impl UdpAdapter {
    pub fn new(config: UdpConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                config,
                socket: RwLock::new(None),
                subscribers: Mutex::new(Vec::new()),
                next_sub_id: AtomicU64::new(0),
            }),
        }
    }

    /// Bind the UDP socket and spawn the receive loop. Returns the
    /// actual local address, including the OS-assigned port when the
    /// configured port is 0.
    pub async fn bind(&self) -> Result<SocketAddr, UdpError> {
        {
            let guard = self.inner.socket.read().await;
            if guard.is_some() {
                return Err(UdpError::AlreadyBound);
            }
        }

        let socket = Arc::new(UdpSocket::bind(self.inner.config.bind_addr).await?);
        let local_addr = socket.local_addr()?;

        {
            let mut guard = self.inner.socket.write().await;
            if guard.is_some() {
                return Err(UdpError::AlreadyBound);
            }
            *guard = Some(Arc::clone(&socket));
        }

        tokio::spawn(read_loop(Arc::clone(&self.inner), socket));
        Ok(local_addr)
    }

    pub async fn local_addr(&self) -> Result<SocketAddr, UdpError> {
        let socket = self.bound_socket().await?;
        Ok(socket.local_addr()?)
    }

    async fn bound_socket(&self) -> Result<Arc<UdpSocket>, UdpError> {
        self.inner
            .socket
            .read()
            .await
            .as_ref()
            .cloned()
            .ok_or(UdpError::NotBound)
    }

    fn destinations(&self, frame: &Frame) -> Result<Vec<SocketAddr>, UdpError> {
        match frame.envelope.target {
            MentionTarget::Peer(peer_id) => self
                .inner
                .config
                .peer_endpoints
                .get(&peer_id)
                .copied()
                .map(|addr| vec![addr])
                .ok_or(UdpError::UnknownPeerEndpoint(peer_id)),
            MentionTarget::All | MentionTarget::Room(_) => {
                let endpoints: Vec<SocketAddr> =
                    self.inner.config.peer_endpoints.values().copied().collect();
                if endpoints.is_empty() {
                    Err(UdpError::NoPeerEndpoints)
                } else {
                    Ok(endpoints)
                }
            }
        }
    }
}

async fn read_loop(inner: Arc<Inner>, socket: Arc<UdpSocket>) {
    let mut buffer = vec![0u8; MAX_DATAGRAM_BYTES];
    loop {
        let (len, _source) = match socket.recv_from(&mut buffer).await {
            Ok(received) => received,
            Err(error) => {
                dispatch_error(&inner, UdpError::Io(error)).await;
                return;
            }
        };
        let frame = match serde_json::from_slice::<Frame>(&buffer[..len]) {
            Ok(frame) => frame,
            Err(error) => {
                dispatch_error(&inner, UdpError::Json(error)).await;
                continue;
            }
        };
        dispatch_frame(&inner, frame).await;
    }
}

async fn dispatch_frame(inner: &Arc<Inner>, frame: Frame) {
    let subscribers = {
        let subscribers = inner.subscribers.lock().await;
        subscribers
            .iter()
            .filter(|subscriber| subscriber.subscription.matches(&frame))
            .map(|subscriber| subscriber.tx.clone())
            .collect::<Vec<_>>()
    };

    for tx in subscribers {
        let _ = tx.try_send(Ok(frame.clone()));
    }
}

async fn dispatch_error(inner: &Arc<Inner>, error: UdpError) {
    let subscribers = {
        let subscribers = inner.subscribers.lock().await;
        subscribers
            .iter()
            .map(|subscriber| subscriber.tx.clone())
            .collect::<Vec<_>>()
    };

    for tx in subscribers {
        let _ = tx.try_send(Err(match &error {
            UdpError::Io(io) => UdpError::Io(std::io::Error::new(io.kind(), io.to_string())),
            UdpError::Json(json) => UdpError::Json(serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                json.to_string(),
            ))),
            UdpError::FrameTooLarge { actual, limit } => UdpError::FrameTooLarge {
                actual: *actual,
                limit: *limit,
            },
            UdpError::UnsupportedDurableKind(kind) => UdpError::UnsupportedDurableKind(*kind),
            UdpError::UnknownPeerEndpoint(peer) => UdpError::UnknownPeerEndpoint(*peer),
            UdpError::NoPeerEndpoints => UdpError::NoPeerEndpoints,
            UdpError::AlreadyBound => UdpError::AlreadyBound,
            UdpError::NotBound => UdpError::NotBound,
        }));
    }
}

#[async_trait]
impl Transport for UdpAdapter {
    type Error = UdpError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        if !matches!(frame.kind, FrameKind::Event) {
            return Err(UdpError::UnsupportedDurableKind(frame.kind));
        }

        let payload = serde_json::to_vec(&frame)?;
        if payload.len() > MAX_DATAGRAM_BYTES {
            return Err(UdpError::FrameTooLarge {
                actual: payload.len(),
                limit: MAX_DATAGRAM_BYTES,
            });
        }

        let socket = self.bound_socket().await?;
        for destination in self.destinations(&frame)? {
            socket.send_to(&payload, destination).await?;
        }
        Ok(())
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_DEPTH);
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
