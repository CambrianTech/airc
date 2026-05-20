//! Per-client connection lifecycle. Resolve PeerId from cert, install
//! the outbound channel synchronously, spawn read + write loops, route
//! inbound frames to other connected peers.
//!
//! The relay never decodes frame bodies for routing decisions — only
//! the envelope's `sender` (excluded from broadcast) and `target`
//! (broadcast vs. unicast). Frame signatures are end-to-end and the
//! relay does not re-sign.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::server::TlsStream;

use airc_core::{MentionTarget, PeerId};
use airc_protocol::Frame;
use airc_transport::lan_tcp::extract_ed25519_pubkey;

use crate::error::RelayServerError;
use crate::server::{Inner, OutboundTx, MAX_FRAME_BYTES, OUTBOUND_CHANNEL_DEPTH};

pub(crate) async fn handle_client(
    inner: Arc<Inner>,
    tls_stream: TlsStream<TcpStream>,
) -> Result<(), RelayServerError> {
    let peer_id = resolve_peer_from_stream(&inner, &tls_stream)
        .ok_or(RelayServerError::PeerCertNotEd25519)?;

    let (outbound_tx, outbound_rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_CHANNEL_DEPTH);

    // Install the connection synchronously. By the time we spawn the
    // I/O loops, frames arriving for this PeerId from OTHER clients
    // can already be routed in via this channel.
    inner.connections.lock().await.insert(peer_id, outbound_tx);

    let (read_half, write_half) = tokio::io::split(tls_stream);

    tokio::spawn(write_loop(write_half, outbound_rx));
    tokio::spawn(read_loop(Arc::clone(&inner), peer_id, read_half));

    Ok(())
}

fn resolve_peer_from_stream(
    inner: &Arc<Inner>,
    tls_stream: &TlsStream<TcpStream>,
) -> Option<PeerId> {
    let (_, conn) = tls_stream.get_ref();
    let cert = conn.peer_certificates()?.first()?;
    let pubkey = extract_ed25519_pubkey(cert).ok()?;
    let registry = inner.registry.read().ok()?;
    registry
        .find_peer(&pubkey)
        .map(|(peer_id, _key_version)| peer_id)
}

async fn read_loop<R>(inner: Arc<Inner>, sender: PeerId, mut read_half: R)
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    loop {
        let mut len_bytes = [0u8; 4];
        if read_half.read_exact(&mut len_bytes).await.is_err() {
            inner.connections.lock().await.remove(&sender);
            return;
        }
        let len = u32::from_be_bytes(len_bytes);
        if len > MAX_FRAME_BYTES {
            // Hostile or buggy client. Drop, log, move on.
            tracing::warn!(
                peer = %sender,
                len,
                limit = MAX_FRAME_BYTES,
                "relay client sent oversized length prefix; dropping connection",
            );
            inner.connections.lock().await.remove(&sender);
            return;
        }
        let mut payload = vec![0u8; len as usize];
        if read_half.read_exact(&mut payload).await.is_err() {
            inner.connections.lock().await.remove(&sender);
            return;
        }

        // The relay decodes the envelope to read routing fields but
        // does not re-encode — we forward the EXACT bytes the sender
        // signed, so receivers verify against canonical bytes.
        let frame: Frame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(error) => {
                tracing::warn!(
                    peer = %sender,
                    %error,
                    "relay client sent malformed frame; dropping connection",
                );
                inner.connections.lock().await.remove(&sender);
                return;
            }
        };

        route_frame(&inner, sender, &frame, &payload).await;
    }
}

/// Decide where the frame goes and push the original bytes to each
/// recipient's outbound channel. The relay forwards bytes, not a
/// re-encoded frame — preserves the signed envelope exactly.
async fn route_frame(inner: &Arc<Inner>, sender: PeerId, frame: &Frame, raw: &[u8]) {
    let recipients = recipients_for(inner, sender, frame).await;
    for recipient in recipients {
        if let Some(tx) = inner.connections.lock().await.get(&recipient).cloned() {
            // `try_send` so a slow recipient applies backpressure on
            // its own outbound channel only — it does NOT block
            // routing for other recipients. Per the Transport trait's
            // lag policy: Event frames are lossy on a full channel,
            // Message/Control kinds accept the same drop here in
            // baseline. PR follow-up will add a durable mailbox.
            if let Err(e) = tx.try_send(raw.to_vec()) {
                tracing::warn!(
                    %recipient,
                    error = %e,
                    "relay dropped frame: recipient outbound channel full",
                );
            }
        }
    }
}

async fn recipients_for(inner: &Arc<Inner>, sender: PeerId, frame: &Frame) -> Vec<PeerId> {
    match frame.envelope.target {
        MentionTarget::Peer(target) => {
            // Direct: forward only to that peer (if connected). Never
            // echo back to sender even if target == sender (a peer
            // talking to itself goes through its own loopback, not
            // the relay).
            if target == sender {
                Vec::new()
            } else if inner.connections.lock().await.contains_key(&target) {
                vec![target]
            } else {
                Vec::new()
            }
        }
        MentionTarget::All | MentionTarget::Room(_) => {
            // Broadcast to every connected peer except the sender.
            // Room-targeted broadcasts are treated identically at the
            // relay layer; subscribers filter by channel locally.
            inner
                .connections
                .lock()
                .await
                .keys()
                .copied()
                .filter(|peer| *peer != sender)
                .collect()
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

// Silence the "unused" lint on OutboundTx if rustc complains; the
// type alias is used in server.rs.
#[allow(dead_code)]
type _OutboundTxKeepInGraph = OutboundTx;
