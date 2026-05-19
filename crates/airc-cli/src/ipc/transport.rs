//! Cross-platform IPC primitives — Unix sockets on Unix, Windows
//! named pipes on Windows. The daemon's accept loop and the CLI
//! client both use `IpcListener` / `IpcStream` so the wire-protocol
//! code stays platform-agnostic.
//!
//! Why typed enums instead of trait objects: tokio's `UnixStream` and
//! `NamedPipeServer` both implement `AsyncRead + AsyncWrite` but
//! aren't `dyn`-friendly out of the box (they want concrete reads).
//! An enum + matching impl gives us a zero-cost dispatch without
//! `Box<dyn ...>` ceremony, and the platform-specific arms are gated
//! by `cfg(unix)` / `cfg(windows)` so the binary compiles on both.
//!
//! Path conventions:
//!   - Unix: a filesystem path like `<home>/daemon.sock`.
//!   - Windows: a named-pipe path like `\\.\pipe\airc-rs-daemon`. If
//!     the caller passes a plain filesystem path, the resolver
//!     converts it to a per-home pipe name so multiple homes can
//!     coexist on one machine.

use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

/// One IPC connection — read/write the wire bytes, no protocol
/// knowledge.
pub enum IpcStream {
    #[cfg(unix)]
    Unix(tokio::net::UnixStream),
    #[cfg(windows)]
    WindowsServer(tokio::net::windows::named_pipe::NamedPipeServer),
    #[cfg(windows)]
    WindowsClient(tokio::net::windows::named_pipe::NamedPipeClient),
}

impl IpcStream {
    /// Dial the daemon at the given path.
    pub async fn connect(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            tokio::net::UnixStream::connect(path)
                .await
                .map(IpcStream::Unix)
        }
        #[cfg(windows)]
        {
            let pipe_name = resolve_pipe_name(path);
            tokio::net::windows::named_pipe::ClientOptions::new()
                .open(&pipe_name)
                .map(IpcStream::WindowsClient)
        }
    }
}

impl AsyncRead for IpcStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(stream) => Pin::new(stream).poll_read(cx, buf),
            #[cfg(windows)]
            IpcStream::WindowsServer(stream) => Pin::new(stream).poll_read(cx, buf),
            #[cfg(windows)]
            IpcStream::WindowsClient(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for IpcStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(stream) => Pin::new(stream).poll_write(cx, buf),
            #[cfg(windows)]
            IpcStream::WindowsServer(stream) => Pin::new(stream).poll_write(cx, buf),
            #[cfg(windows)]
            IpcStream::WindowsClient(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(windows)]
            IpcStream::WindowsServer(stream) => Pin::new(stream).poll_flush(cx),
            #[cfg(windows)]
            IpcStream::WindowsClient(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            #[cfg(unix)]
            IpcStream::Unix(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(windows)]
            IpcStream::WindowsServer(stream) => Pin::new(stream).poll_shutdown(cx),
            #[cfg(windows)]
            IpcStream::WindowsClient(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// Daemon-side listener.
///
/// On Unix this wraps a `UnixListener` bound to `<path>`.
///
/// On Windows there is no "listener" object in the Unix sense for
/// named pipes — each call to `NamedPipeServer::create` returns one
/// pre-allocated server instance that's waiting for a single client
/// connection. To get accept-loop semantics we keep the bound path
/// and re-create a fresh server instance per accept call. The first
/// instance is created with `first_pipe_instance(true)` so two
/// daemons trying to bind the same name surface as an OS error
/// (matches the Unix "address in use" behavior).
pub enum IpcListener {
    #[cfg(unix)]
    Unix {
        listener: tokio::net::UnixListener,
        path: std::path::PathBuf,
    },
    #[cfg(windows)]
    Windows {
        pipe_name: String,
        /// The next server instance, pre-created and ready to
        /// `.connect().await` for accept. On accept, we move it out,
        /// `.connect()`, then create a new server instance for the
        /// NEXT accept call. `first_pipe_instance` is only set on
        /// the very first creation to fail loudly on duplicate
        /// daemons.
        next: tokio::sync::Mutex<Option<tokio::net::windows::named_pipe::NamedPipeServer>>,
    },
}

impl IpcListener {
    /// Bind a listener at `path`. On Unix this `bind(2)`s the socket
    /// file; on Windows this creates the first named-pipe server
    /// instance.
    pub async fn bind(path: &Path) -> std::io::Result<Self> {
        #[cfg(unix)]
        {
            let listener = tokio::net::UnixListener::bind(path)?;
            Ok(IpcListener::Unix {
                listener,
                path: path.to_path_buf(),
            })
        }
        #[cfg(windows)]
        {
            let pipe_name = resolve_pipe_name(path);
            let first = tokio::net::windows::named_pipe::ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)?;
            Ok(IpcListener::Windows {
                pipe_name,
                next: tokio::sync::Mutex::new(Some(first)),
            })
        }
    }

    /// Accept one connection. Returns an `IpcStream` ready for the
    /// wire protocol.
    pub async fn accept(&self) -> std::io::Result<IpcStream> {
        match self {
            #[cfg(unix)]
            IpcListener::Unix { listener, .. } => {
                let (stream, _addr) = listener.accept().await?;
                Ok(IpcStream::Unix(stream))
            }
            #[cfg(windows)]
            IpcListener::Windows { pipe_name, next } => {
                // Take the pre-created server, wait for a client,
                // then create the next server instance so subsequent
                // accept() calls find one waiting.
                let mut guard = next.lock().await;
                let server = guard.take().ok_or_else(|| {
                    std::io::Error::other("ipc listener: no next pipe instance prepared")
                })?;
                server.connect().await?;
                // Prepare next instance before returning.
                *guard =
                    Some(tokio::net::windows::named_pipe::ServerOptions::new().create(pipe_name)?);
                Ok(IpcStream::WindowsServer(server))
            }
        }
    }

    /// Best-effort cleanup of the listener's filesystem footprint.
    /// On Unix this unlinks the socket file. On Windows named pipes
    /// have no persistent FS entry — no-op.
    pub fn cleanup(&self) {
        match self {
            #[cfg(unix)]
            IpcListener::Unix { path, .. } => {
                let _ = std::fs::remove_file(path);
            }
            #[cfg(windows)]
            IpcListener::Windows { .. } => {
                // Named pipes are GCd when the last handle closes.
            }
        }
    }
}

/// Convert a filesystem-style path into a Windows named-pipe path.
///
/// If the path already looks like a pipe (`\\.\pipe\...`) it's
/// returned as-is. Otherwise the path's last component is sanitised
/// and prefixed: `<home>/daemon.sock` → `\\.\pipe\airc-rs-daemon-<home>`.
/// Multiple homes get distinct pipes so concurrent daemons coexist.
#[cfg(windows)]
fn resolve_pipe_name(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw.starts_with(r"\\.\pipe\") {
        return raw.into_owned();
    }
    // Build a stable per-path pipe name. Use the immediate parent's
    // basename + the file basename so different homes don't collide.
    let file = path
        .file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "daemon".to_string());
    let parent_tag = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| "default".to_string());
    let sanitised = sanitise_pipe_token(&format!("{parent_tag}-{file}"));
    format!(r"\\.\pipe\airc-rs-{sanitised}")
}

#[cfg(windows)]
fn sanitise_pipe_token(input: &str) -> String {
    input
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[cfg(unix)]
    #[tokio::test]
    async fn unix_round_trip_via_ipc_listener_and_stream() {
        // Verify the abstraction layer carries bytes correctly on
        // Unix (where we can actually run a listener in test). On
        // Windows the same code path applies but isn't exercised
        // from this CI host — gated CI on a Windows runner would
        // cover that path.
        let dir = TempDir::new().unwrap();
        let socket = dir.path().join("test.sock");

        let listener = IpcListener::bind(&socket).await.unwrap();
        let socket_clone = socket.clone();
        let server_task = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut buf = [0u8; 5];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(b"PONG").await.unwrap();
            stream.shutdown().await.unwrap();
            buf
        });

        // Tiny delay for the bind/listen to settle.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let mut client = IpcStream::connect(&socket_clone).await.unwrap();
        client.write_all(b"PINGX").await.unwrap();
        client.shutdown().await.unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).await.unwrap();

        let received = server_task.await.unwrap();
        assert_eq!(&received, b"PINGX");
        assert_eq!(response, b"PONG");
    }
}
