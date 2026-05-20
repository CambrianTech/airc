//! `RelayServer` — accepts mTLS-pinned client connections and routes
//! frames between them by inspecting the envelope's `target` field.
//!
//! The server is intentionally a *forwarder*. It does not interpret
//! frame bodies, does not subscribe to anything, and does not
//! re-sign. Frame signatures travel end-to-end between peers
//! unchanged by the relay.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::TcpListener;
use tokio::sync::{mpsc, Mutex};
use tokio_rustls::TlsAcceptor;

use airc_core::PeerId;
use airc_protocol::{PeerKeyRegistry, PeerKeypair};
use airc_transport::lan_tcp::build_server_config;

use crate::connection::handle_client;
use crate::error::RelayServerError;

/// Per-frame payload limit on the wire — defense against a misbehaving
/// or hostile client sending an absurd length prefix. Matches
/// `lan_tcp`'s ceiling (16 MiB) so the wire shape stays uniform.
pub(crate) const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

/// Bounded per-client outbound channel. Slow client = backpressure on
/// the routing loop for that target, not unbounded growth.
pub(crate) const OUTBOUND_CHANNEL_DEPTH: usize = 256;

/// Configuration the embedder hands to [`RelayServer::start`].
pub struct RelayServerConfig {
    /// The relay's own `PeerId`. Goes in the relay's server cert so
    /// clients can identify which relay they connected to.
    pub peer_id: PeerId,
    /// The relay's own Ed25519 identity. Clients pin this pubkey.
    pub keypair: PeerKeypair,
    /// Allowlist of clients permitted to connect. mTLS client cert is
    /// resolved to a `PeerId` via `extract_ed25519_pubkey`; only entries
    /// in this registry may connect. Unknown certs fail-closed.
    pub registry: Arc<std::sync::RwLock<PeerKeyRegistry>>,
    /// Bind address. `0.0.0.0:0` is allowed in tests to let the OS
    /// pick a free port (the actual address is reported by
    /// [`RelayServer::local_addr`]).
    pub bind: SocketAddr,
}

/// Outbound channel half handed to the routing loop. The write task
/// owns the receiver and pushes length-prefixed bytes onto the TLS
/// stream.
pub(crate) type OutboundTx = mpsc::Sender<Vec<u8>>;

/// Shared per-server state. Held inside `Arc` and reachable from the
/// accept loop and every connection task.
pub(crate) struct Inner {
    pub(crate) registry: Arc<std::sync::RwLock<PeerKeyRegistry>>,
    pub(crate) connections: Mutex<HashMap<PeerId, OutboundTx>>,
}

/// Running relay server. Owns its accept loop; drop to stop accepting
/// new connections (existing connections drain naturally as clients
/// disconnect).
pub struct RelayServer {
    inner: Arc<Inner>,
    local_addr: SocketAddr,
    accept_task: tokio::task::JoinHandle<()>,
}

impl RelayServer {
    /// Bind, accept the first listener, and spawn the accept loop. The
    /// returned `RelayServer` is live; new connections are routed as
    /// soon as their TLS handshake completes.
    pub async fn start(config: RelayServerConfig) -> Result<Self, RelayServerError> {
        let server_config = build_server_config(
            config.peer_id,
            &config.keypair,
            Arc::clone(&config.registry),
        )?;
        let acceptor = TlsAcceptor::from(server_config);

        let listener = TcpListener::bind(config.bind).await?;
        let local_addr = listener.local_addr()?;

        let inner = Arc::new(Inner {
            registry: config.registry,
            connections: Mutex::new(HashMap::new()),
        });

        let inner_for_task = Arc::clone(&inner);
        let accept_task = tokio::spawn(async move {
            loop {
                let (tcp, _peer_addr) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(e) => {
                        tracing::warn!(error = %e, "relay accept failed");
                        continue;
                    }
                };
                let acceptor = acceptor.clone();
                let inner = Arc::clone(&inner_for_task);
                tokio::spawn(async move {
                    match acceptor.accept(tcp).await {
                        Ok(tls) => {
                            if let Err(e) = handle_client(inner, tls).await {
                                tracing::warn!(error = %e, "relay client session ended");
                            }
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "relay TLS handshake failed");
                        }
                    }
                });
            }
        });

        Ok(Self {
            inner,
            local_addr,
            accept_task,
        })
    }

    /// Bound listener address — useful in tests where `bind` was
    /// `0.0.0.0:0` and the caller needs the actual port.
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Snapshot of currently-connected peer IDs. Intended for telemetry
    /// and tests; the live set changes underneath this call.
    pub async fn connected_peers(&self) -> Vec<PeerId> {
        self.inner
            .connections
            .lock()
            .await
            .keys()
            .copied()
            .collect()
    }

    /// Stop accepting new connections. Existing connections drain on
    /// their own when their TLS stream closes. Drop semantics also
    /// trigger this.
    pub fn shutdown(self) {
        self.accept_task.abort();
    }
}
