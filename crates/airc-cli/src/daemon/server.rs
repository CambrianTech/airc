//! Unix socket listener + accept loop.
//!
//! Each accepted connection is handled in its own task: read one
//! request, dispatch, write one response, close. Independent
//! connections so a slow handler doesn't block the listener.
//!
//! Shutdown: the state's `shutdown` notifier wakes the accept loop;
//! the loop unbinds the socket file and returns. In-flight handlers
//! complete normally.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

use crate::daemon::handlers::dispatch;
use crate::daemon::state::DaemonState;
use crate::ipc::request::Request;
use crate::ipc::response::Response;

/// What can go wrong running the daemon.
#[derive(Debug)]
pub enum DaemonError {
    /// Socket bind / accept I/O failure.
    Io(std::io::Error),
    /// Could not remove a stale socket file from a prior daemon
    /// instance.
    StaleSocket(std::io::Error),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::Io(error) => write!(f, "daemon I/O: {error}"),
            DaemonError::StaleSocket(error) => {
                write!(f, "stale socket cleanup: {error}")
            }
        }
    }
}

impl std::error::Error for DaemonError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            DaemonError::Io(error) | DaemonError::StaleSocket(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(error: std::io::Error) -> Self {
        DaemonError::Io(error)
    }
}

/// Run the daemon: bind the socket, serve connections until
/// shutdown. Returns when the shutdown notifier fires (typically from
/// a Stop request handler) or the listener errors.
pub async fn run(state: Arc<DaemonState>, socket_path: PathBuf) -> Result<(), DaemonError> {
    cleanup_stale_socket(&socket_path).map_err(DaemonError::StaleSocket)?;
    let listener = UnixListener::bind(&socket_path)?;

    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.notified() => {
                break;
            }
            accept = listener.accept() => {
                let (stream, _addr) = accept?;
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        // Surface to stderr — a real deployment can
                        // pipe this to a log. We don't swallow.
                        eprintln!("daemon connection error: {error}");
                    }
                });
            }
        }
    }

    // Best-effort cleanup. Listener drop unlinks the socket file on
    // some platforms but not all — call out the path explicitly so
    // subsequent daemon starts don't trip on a stale file.
    let _ = std::fs::remove_file(&socket_path);
    Ok(())
}

/// If a socket file exists from a prior daemon and no process is
/// holding it, unlink it. If a process IS holding it, the subsequent
/// `bind` will fail with EADDRINUSE — that error propagates up so the
/// user sees it instead of us silently stealing the socket.
fn cleanup_stale_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
    // Try connecting; if that succeeds, the daemon is live → don't
    // touch the file. If it fails with ConnectionRefused, the socket
    // is stale.
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            std::io::ErrorKind::AddrInUse,
            format!("daemon already running on {}", path.display()),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            std::fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

/// Handle one connection: read a single newline-or-EOF-terminated
/// JSON request, dispatch, write the JSON response, close.
async fn handle_connection(
    mut stream: UnixStream,
    state: Arc<DaemonState>,
) -> Result<(), DaemonError> {
    let mut request_bytes = Vec::new();
    stream.read_to_end(&mut request_bytes).await?;

    let request: Request = match serde_json::from_slice(&request_bytes) {
        Ok(request) => request,
        Err(error) => {
            let response = Response::Error {
                message: format!("could not parse request: {error}"),
            };
            write_response(&mut stream, &response).await?;
            return Ok(());
        }
    };

    let response = dispatch(state, request).await;
    write_response(&mut stream, &response).await?;
    Ok(())
}

async fn write_response(stream: &mut UnixStream, response: &Response) -> Result<(), DaemonError> {
    let mut payload = serde_json::to_vec(response).map_err(|error| {
        DaemonError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            error.to_string(),
        ))
    })?;
    payload.push(b'\n');
    stream.write_all(&payload).await?;
    stream.shutdown().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::client::DaemonClient;
    use airc_core::PeerId;
    use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
    use std::sync::RwLock;
    use std::time::Duration;

    fn fresh_state() -> Arc<DaemonState> {
        let peer_id = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let mut registry = PeerKeyRegistry::new();
        registry.enrol(peer_id, 0, keypair.public_bytes()).unwrap();
        let registry = Arc::new(RwLock::new(registry));
        Arc::new(DaemonState::new(
            peer_id,
            keypair,
            registry,
            VerificationPolicy::Strict,
        ))
    }

    fn unique_socket() -> PathBuf {
        // /tmp (not env::temp_dir which on macOS resolves to a long
        // /var/folders/.../T/ path) keeps the path well under SUN_LEN
        // (104 bytes on macOS). Short suffix from the low bits of a
        // fresh UUID — collision odds negligible across one test run.
        let suffix = uuid::Uuid::new_v4().as_u128() as u32;
        PathBuf::from(format!("/tmp/arc{:x}.sock", suffix))
    }

    #[tokio::test]
    async fn client_ping_round_trips_via_real_socket() {
        // The integration test: spawn a real daemon on a Unix socket
        // and confirm a DaemonClient::ping completes the round-trip.
        let state = fresh_state();
        let socket = unique_socket();

        let server_state = state.clone();
        let server_socket = socket.clone();
        let server_handle = tokio::spawn(async move { run(server_state, server_socket).await });

        // Tiny delay for the listener to bind.
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = DaemonClient::new(socket.clone());
        client.ping().await.expect("ping must succeed");

        // Shut down the daemon for clean test teardown.
        client.stop().await.expect("stop must succeed");
        tokio::time::timeout(Duration::from_secs(2), server_handle)
            .await
            .expect("daemon must exit within 2s of stop")
            .expect("join handle")
            .expect("daemon must exit Ok");
    }

    #[tokio::test]
    async fn status_returns_peer_id() {
        let state = fresh_state();
        let expected_peer_id = state.peer_id.to_string();
        let socket = unique_socket();

        let server_state = state.clone();
        let server_socket = socket.clone();
        let server_handle = tokio::spawn(async move { run(server_state, server_socket).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let client = DaemonClient::new(socket);
        let status = client.status().await.expect("status must succeed");
        assert_eq!(status.peer_id, expected_peer_id);

        client.stop().await.unwrap();
        tokio::time::timeout(Duration::from_secs(2), server_handle)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn second_daemon_refuses_to_steal_live_socket() {
        // Pin the cleanup_stale_socket contract: if a daemon is
        // already live on the path, a second run() returns AddrInUse
        // rather than silently taking over.
        let state = fresh_state();
        let socket = unique_socket();
        let first = state.clone();
        let socket_for_first = socket.clone();
        let first_handle = tokio::spawn(async move { run(first, socket_for_first).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let second_result = run(fresh_state(), socket.clone()).await;
        assert!(
            matches!(second_result, Err(DaemonError::StaleSocket(_))),
            "second daemon must refuse to steal a live socket; got {second_result:?}"
        );

        // Clean up first daemon.
        let client = DaemonClient::new(socket);
        client.stop().await.unwrap();
        let _ = first_handle.await;
    }
}
