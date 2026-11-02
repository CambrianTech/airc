//! Per-connection lifecycle: accept/dial → resolve peer-id from cert
//! → install the outbound channel synchronously → spawn read + write
//! loops. The functions here are the only ones that touch the TLS
//! stream directly; everything upstream (`LanTcpAdapter::listen` /
//! `connect()`) is a thin wrapper that hands the TLS stream off.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_rustls::client::TlsStream as ClientTlsStream;
use tokio_rustls::server::TlsStream as ServerTlsStream;

use airc_core::PeerId;
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_protocol::Frame;

use crate::lan_tcp::adapter::dispatch::dispatch_to_subscribers;
use crate::lan_tcp::adapter::error::LanTcpError;
use crate::lan_tcp::adapter::inner::{
    Inner, Outbound, Session, MAX_FRAME_BYTES, OUTBOUND_CHANNEL_DEPTH,
};
use crate::lan_tcp::cert::extract_ed25519_pubkey;

/// Post-handshake server-side connection handler: bind the peer
/// identity from the cert, install the outbound channel
/// **synchronously**, spawn read + write loops. Returns Ok after the
/// outbound channel is installed so callers know send is ready.
pub(super) async fn handle_server_connection(
    inner: Arc<Inner>,
    tls_stream: ServerTlsStream<TcpStream>,
    peer_addr: std::net::SocketAddr,
) -> Result<(), LanTcpError> {
    let (peer_id, authenticated_key) = resolve_peer_from_server_stream(&inner, &tls_stream)
        .ok_or(LanTcpError::PeerNotInRegistry)?;
    // #9: the peer just proved (via the authenticated handshake) it's
    // reachable at `peer_addr.ip()`. Report it to the learn-observer if one
    // is registered — the layer above pairs this real IP with the peer's
    // stable advertised port so a peer whose published endpoint went stale is
    // still dialable. The source PORT is the peer's ephemeral outbound port,
    // NOT its listener, so we surface only the IP. Lock is brief + never held
    // across an await.
    let observer = inner.on_inbound.lock().ok().and_then(|guard| guard.clone());
    if let Some(observer) = observer {
        observer(peer_id, peer_addr.ip());
    }
    let (read_half, write_half) = tokio::io::split(tls_stream);
    install_and_spawn_loops(inner, peer_id, authenticated_key, read_half, write_half).await;
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
    let (peer_id, authenticated_key) = resolve_peer_from_client_stream(&inner, &tls_stream)
        .ok_or(LanTcpError::PeerNotInRegistry)?;
    let (read_half, write_half) = tokio::io::split(tls_stream);
    install_and_spawn_loops(inner, peer_id, authenticated_key, read_half, write_half).await;
    Ok(())
}

fn resolve_peer_from_server_stream(
    inner: &Arc<Inner>,
    tls_stream: &ServerTlsStream<TcpStream>,
) -> Option<(PeerId, [u8; 32])> {
    let certs = tls_stream.get_ref().1.peer_certificates()?;
    let cert = certs.first()?;
    let pubkey = extract_ed25519_pubkey(cert).ok()?;
    inner
        .registry
        .find_peer(&pubkey)
        .map(|(peer, _key_id)| (peer, pubkey))
}

fn resolve_peer_from_client_stream(
    inner: &Arc<Inner>,
    tls_stream: &ClientTlsStream<TcpStream>,
) -> Option<(PeerId, [u8; 32])> {
    let certs = tls_stream.get_ref().1.peer_certificates()?;
    let cert = certs.first()?;
    let pubkey = extract_ed25519_pubkey(cert).ok()?;
    inner
        .registry
        .find_peer(&pubkey)
        .map(|(peer, _key_id)| (peer, pubkey))
}

/// Install the outbound channel into `inner` **before** spawning the
/// read/write loops, then spawn them. This is the readiness-fix per
/// Codex's #671 review finding: previously the outbound channel was
/// set inside a spawned task, so callers (and the CLI) could call
/// `send()` after `connect()` returned and find no connection
/// installed yet. Now the install is awaited inline.
///
/// A session it replaces (the same peer, redialed) is ended here, not left running: its
/// outbound sender drops with the map entry (closing its write loop) and its reader is
/// aborted, so both halves of the old TLS stream drop and the socket closes.
pub(super) async fn install_and_spawn_loops<R, W>(
    inner: Arc<Inner>,
    peer_id: PeerId,
    authenticated_key: [u8; 32],
    read_half: R,
    write_half: W,
) where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    let (outbound_tx, outbound_rx) = mpsc::channel::<Outbound>(OUTBOUND_CHANNEL_DEPTH);
    let id = inner
        .next_session_id
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let (writer_ready, writer_installed) = oneshot::channel::<()>();
    let writer_inner = Arc::clone(&inner);
    let writer = tokio::spawn(async move {
        if writer_installed.await.is_err() {
            return;
        }
        write_loop(write_half, outbound_rx).await;
        disconnect_session(&writer_inner, peer_id, id).await;
    });
    // The reader starts only once its session is in the map, so a read that ends at once
    // cannot remove an entry that does not exist yet and leave a dead session installed.
    let (installed_tx, installed_rx) = oneshot::channel::<()>();
    let reader = tokio::spawn(read_loop(
        Arc::clone(&inner),
        peer_id,
        id,
        read_half,
        installed_rx,
    ));
    // Install synchronously — when this function returns, the
    // connection IS ready to receive sends.
    let replaced = inner.connections.lock().await.insert(
        peer_id,
        Session {
            authenticated_key,
            outbound: outbound_tx,
            id,
            reader: reader.abort_handle(),
            writer: writer.abort_handle(),
        },
    );
    let _ = installed_tx.send(());
    let _ = writer_ready.send(());
    if let Some(old) = replaced {
        old.end();
    }
    let observer = inner.on_connect.lock().ok().and_then(|guard| guard.clone());
    if let Some(observer) = observer {
        observer(peer_id);
    }
}

/// Either I/O loop ending: remove this peer's entry only if it is still THIS session.
/// A newer session for the same peer (a redial) is not this reader's to end.
async fn disconnect_session(inner: &Arc<Inner>, peer_id: PeerId, id: u64) {
    let removed = {
        let mut connections = inner.connections.lock().await;
        match connections.get(&peer_id) {
            Some(session) if session.id == id => connections.remove(&peer_id),
            _ => None,
        }
    };
    if let Some(session) = removed {
        // Both split halves must end. Aborting this task takes effect at its
        // next suspension; notification below is synchronous and remains visible.
        session.end();
        notify_disconnect(inner, peer_id);
    }
}

pub(super) fn notify_disconnect(inner: &Arc<Inner>, peer_id: PeerId) {
    let observer = inner
        .on_disconnect
        .lock()
        .ok()
        .and_then(|guard| guard.clone());
    if let Some(observer) = observer {
        observer(peer_id);
    }
}

/// Read loop: pull length-prefixed JSON frames off the TLS stream
/// and fan out to subscribers per the Transport trait's lag policy.
/// Removes this peer's entry from `connections` on any termination
/// (clean EOF, I/O error, malformed payload, oversized frame).
async fn read_loop<R>(
    inner: Arc<Inner>,
    peer_id: PeerId,
    id: u64,
    mut read_half: R,
    installed: oneshot::Receiver<()>,
) where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    let _ = installed.await;
    loop {
        // Read 4-byte BE length prefix.
        let mut len_bytes = [0u8; 4];
        if read_half.read_exact(&mut len_bytes).await.is_err() {
            disconnect_session(&inner, peer_id, id).await;
            return;
        }
        let len = u32::from_be_bytes(len_bytes);
        if len > MAX_FRAME_BYTES {
            disconnect_session(&inner, peer_id, id).await;
            return;
        }
        let mut payload = vec![0u8; len as usize];
        if read_half.read_exact(&mut payload).await.is_err() {
            disconnect_session(&inner, peer_id, id).await;
            return;
        }

        let frame: Frame = match serde_json::from_slice(&payload) {
            Ok(frame) => frame,
            Err(error) => {
                // Malformed payload — drop the connection rather than
                // silently skipping. Card 39d37629: this is a frame the
                // sender saw flushed ("sent") that will never reach a
                // transcript — it MUST be loud. Build skew between
                // peers (frame schema drift) lands exactly here.
                StderrJsonDiagnosticSink.emit(
                    DiagnosticEvent::error(
                        DiagnosticComponent::Transport,
                        DiagnosticCode::FrameUndeliverable,
                        "inbound lan-tcp frame did not decode — frame dropped, connection closed",
                    )
                    .with_field("reason", "decode_failure")
                    .with_field("peer", peer_id)
                    .with_field("payload_bytes", payload.len())
                    .with_field("error", error),
                );
                disconnect_session(&inner, peer_id, id).await;
                return;
            }
        };

        dispatch_to_subscribers(&inner, frame).await;
    }
}

/// Write loop: drain the outbound channel, length-prefix the
/// already-validated payload, write framed bytes to TLS, then signal
/// the sender that the frame is flushed to the wire.
///
/// The payload is pre-serialized and size-checked by
/// `LanTcpAdapter::send` before it lands in the channel — by the
/// time it reaches this loop the bytes are known-valid, so the
/// write failures terminate the loop. Caller cancellation discards queued
/// work, or closes the stream if writing may already have started.
///
/// The `flushed` signal is fired ONLY after a successful `flush()`, so a
/// caller awaiting it knows the bytes are in the kernel send buffer and
/// will survive the caller exiting. On any write/flush error the loop
/// returns and drops the remaining `flushed` senders — their awaiting
/// `send()` calls observe the closed channel and report non-delivery
/// rather than a false success.
async fn write_loop<W>(mut write_half: W, mut outbound_rx: mpsc::Receiver<Outbound>)
where
    W: tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    while let Some(Outbound {
        payload,
        mut flushed,
    }) = outbound_rx.recv().await
    {
        if flushed.is_closed() {
            continue;
        }
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
        let written = tokio::select! {
            biased;
            // Cancellation may interrupt a partial frame. End this stream;
            // never continue with another frame on a corrupted boundary.
            _ = flushed.closed() => return,
            result = async {
                write_half.write_all(&len).await?;
                write_half.write_all(&payload).await?;
                write_half.flush().await
            } => result,
        };
        if written.is_err() {
            return;
        }
        // Completion means bytes reached the kernel, not consumer execution.
        let _ = flushed.send(());
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn cancelled_queue_item_emits_nothing_and_next_item_survives() {
        let (writer, mut reader) = tokio::io::duplex(64);
        let (tx, rx) = mpsc::channel(2);
        let (cancelled, wait) = tokio::sync::oneshot::channel();
        drop(wait);
        assert!(tx
            .send(Outbound {
                payload: vec![9; 10],
                flushed: cancelled,
            })
            .await
            .is_ok());
        let (flushed, done) = tokio::sync::oneshot::channel();
        assert!(tx
            .send(Outbound {
                payload: vec![7],
                flushed,
            })
            .await
            .is_ok());
        drop(tx);
        let task = tokio::spawn(write_loop(writer, rx));
        let mut bytes = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reader.read_to_end(&mut bytes),
        )
        .await
        .unwrap()
        .unwrap();
        task.await.unwrap();
        done.await.unwrap();
        assert_eq!(bytes, [0, 0, 0, 1, 7]);
    }

    #[tokio::test]
    async fn cancelling_blocked_write_closes_partial_stream() {
        let (writer, mut reader) = tokio::io::duplex(1);
        let (tx, rx) = mpsc::channel(1);
        let (flushed, wait) = tokio::sync::oneshot::channel();
        assert!(tx
            .send(Outbound {
                payload: vec![7; 64],
                flushed,
            })
            .await
            .is_ok());
        let task = tokio::spawn(write_loop(writer, rx));
        let mut byte = [0];
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            reader.read_exact(&mut byte),
        )
        .await
        .unwrap()
        .unwrap();
        drop(wait);
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        let mut remainder = Vec::new();
        reader.read_to_end(&mut remainder).await.unwrap();
        assert!(
            remainder.len() < 67,
            "cancelled frame must not finish writing"
        );
        assert!(tx.is_closed());
    }
}
