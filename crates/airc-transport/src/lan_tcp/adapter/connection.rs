//! Per-connection lifecycle: accept/dial → resolve peer-id from cert
//! → install the outbound channel synchronously → spawn read + write
//! loops. The functions here are the only ones that touch the TLS
//! stream directly; everything upstream (`LanTcpAdapter::listen` /
//! `connect()`) is a thin wrapper that hands the TLS stream off.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;

use airc_core::PeerId;
use airc_protocol::Frame;

use crate::lan_tcp::adapter::dispatch::dispatch_to_subscribers;
use crate::lan_tcp::adapter::error::LanTcpError;
use crate::lan_tcp::adapter::inner::{Inner, MAX_FRAME_BYTES, OUTBOUND_CHANNEL_DEPTH};
use crate::lan_tcp::cert::extract_ed25519_pubkey;

/// Post-handshake server-side connection handler: bind the peer
/// identity from the cert, install the outbound channel
/// **synchronously**, spawn read + write loops. Returns Ok after the
/// outbound channel is installed so callers know send is ready.
pub(super) async fn handle_server_connection(
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
pub(super) async fn handle_client_connection(
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
    inner
        .registry
        .find_peer(&pubkey)
        .map(|(peer, _key_id)| peer)
}

fn resolve_peer_from_client_stream(
    inner: &Arc<Inner>,
    tls_stream: &ClientTlsStream<TcpStream>,
) -> Option<PeerId> {
    let certs = tls_stream.get_ref().1.peer_certificates()?;
    let cert = certs.first()?;
    let pubkey = extract_ed25519_pubkey(cert).ok()?;
    inner
        .registry
        .find_peer(&pubkey)
        .map(|(peer, _key_id)| peer)
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
    let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_CHANNEL_DEPTH);
    // Install synchronously — when this function returns, the
    // connection IS ready to receive sends.
    inner.connections.lock().await.insert(peer_id, outbound_tx);

    // Spawn the I/O loops; they continue to drive the wire after
    // this function returns.
    tokio::spawn(write_loop(write_half, outbound_rx));
    tokio::spawn(read_loop(inner, peer_id, read_half));
}

/// Read loop: pull length-prefixed JSON frames off the TLS stream
/// and fan out to subscribers per the Transport trait's lag policy.
/// Removes this peer's entry from `connections` on any termination
/// (clean EOF, I/O error, malformed payload, oversized frame).
async fn read_loop<R>(inner: Arc<Inner>, peer_id: PeerId, mut read_half: R)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    loop {
        // Read 4-byte BE length prefix.
        let mut len_bytes = [0u8; 4];
        if read_half.read_exact(&mut len_bytes).await.is_err() {
            inner.connections.lock().await.remove(&peer_id);
            return;
        }
        let len = u32::from_be_bytes(len_bytes);
        if len > MAX_FRAME_BYTES {
            inner.connections.lock().await.remove(&peer_id);
            return;
        }
        let mut payload = vec![0u8; len as usize];
        if read_half.read_exact(&mut payload).await.is_err() {
            inner.connections.lock().await.remove(&peer_id);
            return;
        }

        let frame: Frame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(_error) => {
                // Malformed payload — drop the connection rather
                // than silently skipping.
                inner.connections.lock().await.remove(&peer_id);
                return;
            }
        };

        dispatch_to_subscribers(&inner, frame).await;
    }
}

/// Write loop: drain the outbound channel, length-prefix the
/// already-validated payload, write framed bytes to TLS.
///
/// The payload is pre-serialized and size-checked by
/// `LanTcpAdapter::send` before it lands in the channel — by the
/// time it reaches this loop the bytes are known-valid, so the
/// only failure mode is a dead socket (which terminates the loop).
/// No silent drops.
async fn write_loop<W>(mut write_half: W, mut outbound_rx: mpsc::Receiver<Vec<u8>>)
where
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    while let Some(payload) = outbound_rx.recv().await {
        // Defense-in-depth assertion: the sender already enforced
        // this, but if a future regression bypasses pre-validation
        // we'd rather fail loudly here than silently truncate.
        debug_assert!(
            payload.len() <= MAX_FRAME_BYTES as usize,
            "write_loop received oversized payload ({} bytes, limit {})",
            payload.len(),
            MAX_FRAME_BYTES
        );
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
