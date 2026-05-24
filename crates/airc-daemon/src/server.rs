//! Cross-platform IPC listener + accept loop.
//!
//! Each accepted connection is handled in its own task: read one
//! length-framed request, dispatch, write one length-framed response,
//! close. Attach streams keep writing response frames on the same
//! connection.
//!
//! Transport varies by platform via `IpcListener`:
//!   - Unix: Unix-domain socket at `<home>/daemon.sock`
//!   - Windows: named pipe at `\\.\pipe\airc-core-<home>`
//!
//! Shutdown: the state's `shutdown` notifier wakes the accept loop;
//! the loop runs the transport's `cleanup` (unlinks the socket file
//! on Unix; no-op on Windows) and returns.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use fs2::FileExt;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::handlers::dispatch;
use crate::ipc::codec::{read_frame, write_frame};
use crate::ipc::request::{AttachRequest, Request};
use crate::ipc::response::Response;
use crate::ipc::transport::{IpcListener, IpcStream};
use crate::state::DaemonState;

/// What can go wrong running the daemon.
#[derive(Debug)]
pub enum DaemonError {
    /// Another daemon already owns this IPC endpoint.
    AlreadyRunning(PathBuf),
    /// Socket bind / accept I/O failure.
    Io(std::io::Error),
    /// Could not remove a stale socket file from a prior daemon
    /// instance.
    StaleSocket(std::io::Error),
}

impl std::fmt::Display for DaemonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DaemonError::AlreadyRunning(path) => {
                write!(f, "daemon already running on {}", path.display())
            }
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
            DaemonError::AlreadyRunning(_) => None,
            DaemonError::Io(error) | DaemonError::StaleSocket(error) => Some(error),
        }
    }
}

impl From<std::io::Error> for DaemonError {
    fn from(error: std::io::Error) -> Self {
        DaemonError::Io(error)
    }
}

/// Run the daemon: bind the IPC listener, serve connections until
/// shutdown. Returns when the shutdown notifier fires (typically from
/// a Stop request handler) or the listener errors.
pub async fn run(state: Arc<DaemonState>, socket_path: PathBuf) -> Result<(), DaemonError> {
    let _guard = DaemonBindGuard::acquire(&socket_path)?;
    cleanup_stale_socket(&socket_path).map_err(DaemonError::StaleSocket)?;
    let listener = IpcListener::bind(&socket_path).await?;

    loop {
        tokio::select! {
            biased;
            _ = state.shutdown.notified() => {
                break;
            }
            accept = listener.accept() => {
                let stream = accept?;
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        eprintln!("daemon connection error: {error}");
                    }
                });
            }
        }
    }

    // Best-effort transport cleanup. On Unix this unlinks the
    // socket file; on Windows named pipes are GCd when handles
    // close, so the call is a no-op.
    listener.cleanup();
    Ok(())
}

struct DaemonBindGuard {
    file: std::fs::File,
}

impl DaemonBindGuard {
    fn acquire(socket_path: &Path) -> Result<Self, DaemonError> {
        let lock_dir = std::env::temp_dir().join("airc-daemon-locks");
        std::fs::create_dir_all(&lock_dir)?;
        let lock_path = lock_dir.join(format!("{}.lock", socket_lock_id(socket_path)));
        let file = std::fs::OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .truncate(false)
            .open(lock_path)?;
        if let Err(error) = file.try_lock_exclusive() {
            if is_lock_contended(&error) {
                return Err(DaemonError::AlreadyRunning(socket_path.to_path_buf()));
            }
            return Err(DaemonError::Io(error));
        }
        Ok(Self { file })
    }
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::PermissionDenied
    ) || error.raw_os_error() == Some(33)
}

impl Drop for DaemonBindGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

fn socket_lock_id(socket_path: &Path) -> Uuid {
    const LOCK_NAMESPACE: Uuid = Uuid::from_bytes([
        0xa1, 0xc2, 0x70, 0x1b, 0xe0, 0x05, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x03,
    ]);
    Uuid::new_v5(&LOCK_NAMESPACE, socket_path.to_string_lossy().as_bytes())
}

/// If the previous daemon left a stale socket file behind, unlink
/// it. If a process is actively holding it (live daemon), bail with
/// AddrInUse rather than silently steal the listener.
///
/// Unix only — named pipes on Windows don't leave a filesystem
/// entry, so there's nothing to clean and the OS itself rejects
/// duplicate binders.
#[cfg(unix)]
fn cleanup_stale_socket(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        return Ok(());
    }
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

#[cfg(not(unix))]
fn cleanup_stale_socket(_path: &Path) -> std::io::Result<()> {
    // Windows named pipes self-clean; duplicate-binder protection
    // comes from `ServerOptions::first_pipe_instance(true)` set in
    // the transport layer.
    Ok(())
}

/// Handle one connection: read one length-framed request, dispatch,
/// write one length-framed response, drop. No half-close on the read
/// side — Unix sockets can `shutdown` to signal EOF, but Windows named
/// pipes have no half-close.
async fn handle_connection(stream: IpcStream, state: Arc<DaemonState>) -> Result<(), DaemonError> {
    let (reader, mut writer) = tokio::io::split(stream);
    let mut reader = reader;

    let request: Request = match read_frame(&mut reader).await {
        Ok(Some(request)) => request,
        Ok(None) => return Ok(()),
        Err(error) => {
            let response = Response::Error {
                message: format!("could not parse request: {error}"),
            };
            write_response(&mut writer, &response).await?;
            return Ok(());
        }
    };

    if let Request::Attach(attach) = request {
        return stream_attach(writer, state, attach).await;
    }

    let response = dispatch(state, request).await;
    write_response(&mut writer, &response).await?;
    // Drop reader+writer (and thus the underlying stream) so the
    // client's read sees EOF promptly.
    Ok(())
}

async fn stream_attach<W>(
    mut writer: W,
    state: Arc<DaemonState>,
    attach: AttachRequest,
) -> Result<(), DaemonError>
where
    W: AsyncWriteExt + Unpin,
{
    write_response(&mut writer, &Response::Ok).await?;
    let mut rx = state.live_tx.subscribe();
    loop {
        let event = match rx.recv().await {
            Ok(event) => event,
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return Ok(()),
        };
        if attach
            .channel
            .is_some_and(|channel| event.room_id != channel)
        {
            continue;
        }
        write_response(
            &mut writer,
            &Response::Event {
                event: Box::new(event),
            },
        )
        .await?;
    }
}

async fn write_response<W>(writer: &mut W, response: &Response) -> Result<(), DaemonError>
where
    W: AsyncWriteExt + Unpin,
{
    write_frame(writer, response).await.map_err(DaemonError::Io)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::client::DaemonClient;
    use airc_core::PeerId;
    use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
    use std::time::Duration;

    fn fresh_state() -> Arc<DaemonState> {
        let peer_id = PeerId::from_u128(0xa1);
        let keypair = PeerKeypair::generate();
        let registry = PeerKeyRegistry::new();
        registry.enrol(peer_id, 0, keypair.public_bytes()).unwrap();
        let registry = Arc::new(registry);
        // Test home — leaked so it lives until process exit.
        let home = tempfile::TempDir::new().unwrap();
        let home_path = home.path().to_path_buf();
        std::mem::forget(home);
        let store: Arc<dyn airc_store::EventStore> =
            Arc::new(airc_store::InMemoryEventStore::new());
        Arc::new(DaemonState::new(
            peer_id,
            keypair,
            registry,
            VerificationPolicy::Strict,
            home_path,
            store,
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
    async fn subscribe_then_send_then_inbox_round_trips() {
        // The big daemon e2e: subscribe to a wire, send a frame
        // (daemon's own send writes to the wire), inbox returns it.
        // Proves the daemon's send + buffered-subscribe paths
        // compose correctly.
        use crate::ipc::request::{InboxRequest, SendRequest, SubscribeRequest};
        use tempfile::TempDir;

        let state = fresh_state();
        let socket = unique_socket();
        let server_state = state.clone();
        let server_socket = socket.clone();
        let server_handle = tokio::spawn(async move { run(server_state, server_socket).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let dir = TempDir::new().unwrap();
        let wire = dir.path().to_path_buf();
        let channel = uuid::Uuid::nil();

        let client = DaemonClient::new(socket);
        // Subscribe first so the daemon starts the drain task.
        client
            .subscribe(SubscribeRequest { wire: wire.clone() })
            .await
            .unwrap();
        // Send through the daemon (daemon signs + writes to the wire).
        client
            .send(SendRequest {
                wire: wire.clone(),
                channel,
                text: "hello from daemon".to_string(),
                headers: airc_core::Headers::new(),
            })
            .await
            .unwrap();

        // Inbox MAY need a brief moment for the subscriber task to
        // drain the new frame from the wire's tail loop into the
        // event store.
        let mut attempts = 0;
        let inbox = loop {
            let response = client
                .inbox(InboxRequest {
                    since: None,
                    channel: None,
                    limit: None,
                })
                .await
                .unwrap();
            if !response.events.is_empty() {
                break response;
            }
            attempts += 1;
            if attempts > 20 {
                panic!(
                    "inbox never saw the sent event (attempts={attempts}, newest={:?})",
                    response.newest
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        };
        assert_eq!(inbox.events.len(), 1);
        assert_eq!(
            inbox.events[0]
                .body
                .as_ref()
                .and_then(airc_core::Body::as_text)
                .unwrap(),
            "hello from daemon"
        );

        // `newest` cursor should let us "advance past" — second
        // inbox call returns empty.
        let cursor = inbox.newest.clone().unwrap();
        let after = client
            .inbox(InboxRequest {
                since: Some(cursor),
                channel: None,
                limit: None,
            })
            .await
            .unwrap();
        assert!(
            after.events.is_empty(),
            "after the cursor, inbox must be empty"
        );

        client.stop().await.unwrap();
        let _ = tokio::time::timeout(Duration::from_secs(2), server_handle).await;
    }

    #[tokio::test]
    async fn second_daemon_refuses_to_steal_live_socket() {
        // A second daemon must fail before binding the endpoint. The
        // bind guard normalizes platform lock errors into the daemon
        // contract so callers do not need OS-specific error matching.
        let state = fresh_state();
        let socket = unique_socket();
        let first = state.clone();
        let socket_for_first = socket.clone();
        let first_handle = tokio::spawn(async move { run(first, socket_for_first).await });
        tokio::time::sleep(Duration::from_millis(50)).await;

        let second_result =
            tokio::time::timeout(Duration::from_secs(2), run(fresh_state(), socket.clone()))
                .await
                .expect("second daemon bind attempt must return promptly");
        assert!(
            matches!(second_result, Err(DaemonError::AlreadyRunning(_))),
            "second daemon must refuse to steal a live socket; got {second_result:?}"
        );

        // Clean up first daemon.
        let client = DaemonClient::new(socket);
        client.stop().await.unwrap();
        let _ = first_handle.await;
    }
}
