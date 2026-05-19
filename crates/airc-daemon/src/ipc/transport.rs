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
//!   - Windows: a named-pipe path like `\\.\pipe\airc-rs-<hash>`. If
//!     the caller passes a plain filesystem path, the resolver
//!     converts it via UUIDv5 over the full path string so multiple
//!     homes on one machine cannot collide. (Earlier versions used
//!     only the parent dirname + file basename — two users both with
//!     `~/.airc-rs/daemon.sock` collided. Caught in Codex audit
//!     2026-05-19, grievance §4 / Windows Gaps.)

use std::path::Path;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

#[cfg(any(windows, test))]
use uuid::Uuid;

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

/// Namespace UUID for deriving named-pipe discriminators from socket
/// paths. Fixed so the mapping is stable across rustc versions and
/// across machines (same path → same pipe name).
#[cfg(any(windows, test))]
const PIPE_NAMESPACE: Uuid = Uuid::from_bytes([
    0xa1, 0xc2, 0x70, 0x1b, 0xe0, 0x05, 0x40, 0x00, 0x80, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
]);

/// Convert a filesystem-style path into a Windows named-pipe path.
///
/// If the path already looks like a pipe (`\\.\pipe\...`) it's
/// returned as-is. Otherwise the path is normalised (see
/// `canonicalize_for_hash`) and the result is hashed via UUIDv5
/// under `PIPE_NAMESPACE`: `C:\Users\alice\.airc-rs\daemon.sock` →
/// `\\.\pipe\airc-rs-<32-hex>`. Two distinct paths cannot collide;
/// the same path always resolves to the same pipe name regardless
/// of separator style or case-equivalent spelling (NTFS is
/// case-insensitive, so `C:\Users\Alice` and `c:\users\alice` must
/// hash to the same pipe).
///
/// Earlier implementations used only `parent_basename + file_basename`,
/// which collided when two users both used `.airc-rs/daemon.sock`.
/// Codex audit 2026-05-19, grievance §4.
#[cfg(any(windows, test))]
fn resolve_pipe_name(path: &Path) -> String {
    let raw = path.to_string_lossy();
    if raw.starts_with(r"\\.\pipe\") {
        return raw.into_owned();
    }
    let canonical = canonicalize_for_hash(path);
    let digest = Uuid::new_v5(&PIPE_NAMESPACE, canonical.as_bytes());
    format!(r"\\.\pipe\airc-rs-{}", digest.as_simple())
}

/// Reduce a path to a stable byte representation for hashing so that
/// equivalent spellings (different separators, case differences,
/// `..` segments) all collapse to the same pipe name.
///
/// Strategy:
///   1. Ask the OS to canonicalise the whole path. Succeeds when the
///      socket file already exists (the common steady-state case for
///      a client connecting to a running daemon).
///   2. If that fails, canonicalise just the parent dir (which exists
///      after `airc-rs init`) and re-join the file basename.
///   3. If that also fails, fall back to a string-level normalisation:
///      backslashes → forward slashes, lowercase. On Windows this
///      handles separator + case equivalence; on Unix the function
///      isn't used in production, only by the cross-platform tests
///      that pass synthetic non-existent `C:\...` paths.
///
/// Lowercase is correct on Windows (NTFS case-insensitive) and only
/// active in this code path because `#[cfg(any(windows, test))]`
/// gates the whole resolver. Case-sensitive file systems on Linux
/// would never reach this function in production.
#[cfg(any(windows, test))]
fn canonicalize_for_hash(path: &Path) -> String {
    if let Ok(canon) = std::fs::canonicalize(path) {
        return canon.to_string_lossy().to_lowercase();
    }
    if let Some(parent) = path.parent() {
        if let (Ok(canon_parent), Some(file)) = (std::fs::canonicalize(parent), path.file_name()) {
            return canon_parent.join(file).to_string_lossy().to_lowercase();
        }
    }
    path.to_string_lossy().replace('\\', "/").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::TempDir;

    #[test]
    fn pipe_names_differ_across_homes() {
        // The bug Codex flagged: two users both with
        // `<home>/.airc-rs/daemon.sock` collided because the old
        // resolver only used `parent_basename + file_basename`.
        // UUIDv5 over the full path string forces uniqueness.
        let alice: PathBuf = [r"C:\", "Users", "alice", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        let bob: PathBuf = [r"C:\", "Users", "bob", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        let a = resolve_pipe_name(&alice);
        let b = resolve_pipe_name(&bob);
        assert_ne!(a, b, "alice's and bob's pipes must not collide");
        assert!(
            a.starts_with(r"\\.\pipe\airc-rs-"),
            "pipe prefix preserved: {a}"
        );
        assert!(
            b.starts_with(r"\\.\pipe\airc-rs-"),
            "pipe prefix preserved: {b}"
        );
    }

    #[test]
    fn pipe_name_is_stable_across_calls() {
        // Same path → same pipe. UUIDv5 is deterministic, so the
        // daemon and client running independently produce matching
        // pipe names from the same socket path.
        let path: PathBuf = [r"C:\", "Users", "alice", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        let first = resolve_pipe_name(&path);
        let second = resolve_pipe_name(&path);
        assert_eq!(first, second);
    }

    #[test]
    fn pipe_name_passes_through_explicit_pipe_path() {
        // If someone hands the resolver an already-formed pipe path
        // (e.g. test harness, custom daemon launcher), respect it.
        let explicit = PathBuf::from(r"\\.\pipe\custom-airc-pipe");
        assert_eq!(resolve_pipe_name(&explicit), r"\\.\pipe\custom-airc-pipe");
    }

    #[test]
    fn pipe_names_handle_paths_sharing_dir_basename() {
        // Specific collision case from grievance §4: both users have
        // `<home>/.airc-rs/daemon.sock`, so `parent_basename + file`
        // alone is identical for both. Path-hash discriminates.
        let same_basename_a: PathBuf = [r"C:\", "U", "alice", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        let same_basename_b: PathBuf = [r"C:\", "U", "bob", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        assert_ne!(
            resolve_pipe_name(&same_basename_a),
            resolve_pipe_name(&same_basename_b)
        );
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn windows_two_homes_round_trip_concurrently() {
        // The defensible Windows runtime proof per grievance §4 /
        // Windows Gaps "Runtime named-pipe IPC test on Windows":
        // two daemons at distinct `<home>` dirs both bind, accept,
        // read, and write — and a client connecting to home A's
        // socket reaches home A's pipe, never home B's. Under the
        // old resolver both pipes had the same name and this would
        // either fail to bind or cross-deliver.
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let dir_a = TempDir::new().unwrap();
        let dir_b = TempDir::new().unwrap();
        let sock_a = dir_a.path().join("daemon.sock");
        let sock_b = dir_b.path().join("daemon.sock");

        let listener_a = IpcListener::bind(&sock_a).await.expect("home A must bind");
        let listener_b = IpcListener::bind(&sock_b)
            .await
            .expect("home B must bind alongside A — distinct pipe names");

        // Each server task expects a unique payload from its own
        // home's client and echoes back a home-specific reply. If
        // the pipes were colliding the server tasks would receive
        // each other's messages.
        // Use `read_exact` framing on both sides so the test doesn't
        // depend on `shutdown()` half-close semantics — Windows named
        // pipes have no half-close, so the daemon protocol switched
        // to newline framing for the same reason. Here we know each
        // side writes exactly 6 bytes.
        let server_a = tokio::spawn(async move {
            let mut stream = listener_a.accept().await.unwrap();
            let mut buf = [0u8; 6];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(b"PONG-A").await.unwrap();
            stream.flush().await.unwrap();
            buf
        });
        let server_b = tokio::spawn(async move {
            let mut stream = listener_b.accept().await.unwrap();
            let mut buf = [0u8; 6];
            stream.read_exact(&mut buf).await.unwrap();
            stream.write_all(b"PONG-B").await.unwrap();
            stream.flush().await.unwrap();
            buf
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let mut client_a = IpcStream::connect(&sock_a).await.unwrap();
        client_a.write_all(b"PING-A").await.unwrap();
        client_a.flush().await.unwrap();
        let mut reply_a = [0u8; 6];
        client_a.read_exact(&mut reply_a).await.unwrap();

        let mut client_b = IpcStream::connect(&sock_b).await.unwrap();
        client_b.write_all(b"PING-B").await.unwrap();
        client_b.flush().await.unwrap();
        let mut reply_b = [0u8; 6];
        client_b.read_exact(&mut reply_b).await.unwrap();

        let received_a = server_a.await.unwrap();
        let received_b = server_b.await.unwrap();

        // Cross-contamination check: A must see its own ping.
        assert_eq!(&received_a, b"PING-A", "home A received its own ping");
        assert_eq!(&received_b, b"PING-B", "home B received its own ping");
        assert_eq!(&reply_a, b"PONG-A", "home A client got A's pong");
        assert_eq!(&reply_b, b"PONG-B", "home B client got B's pong");
    }

    #[test]
    fn pipe_name_normalises_case_equivalent_spellings() {
        // NTFS is case-insensitive, so `C:\Users\Alice` and
        // `c:\users\alice` refer to the same path. They must resolve
        // to the same pipe so a client and daemon launched from
        // wrappers that differ in case (PowerShell vs. cmd vs. Git
        // Bash) still find each other.
        let upper: PathBuf = [r"C:\", "Users", "Alice", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        let lower: PathBuf = [r"c:\", "users", "alice", ".airc-rs", "daemon.sock"]
            .iter()
            .collect();
        assert_eq!(
            resolve_pipe_name(&upper),
            resolve_pipe_name(&lower),
            "case-equivalent paths must resolve to the same pipe"
        );
    }

    #[test]
    fn pipe_name_normalises_separator_style() {
        // Backslash-vs-forward-slash spellings of the same path
        // (PowerShell-style vs Git Bash-style) must resolve to the
        // same pipe. canonicalize_for_hash collapses both to a
        // single normalised string before hashing.
        let backslash = PathBuf::from(r"C:\Users\alice\.airc-rs\daemon.sock");
        let forward = PathBuf::from("C:/Users/alice/.airc-rs/daemon.sock");
        assert_eq!(
            resolve_pipe_name(&backslash),
            resolve_pipe_name(&forward),
            "separator style must not affect pipe identity"
        );
    }

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
