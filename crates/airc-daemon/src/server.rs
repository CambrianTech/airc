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
use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use airc_bus::envelope::{Cursor, DeliveryClass, Kind};
use airc_bus::{Filter, Seq};
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_ipc::codec::{read_frame, write_frame};
use airc_ipc::request::{AttachRequest, IpcDelivery, IpcKind, Request};
use airc_ipc::response::Response;
use airc_ipc::transport::{IpcListener, IpcStream};

use crate::handlers::dispatch;
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

    // Keep ONE `Notified` future alive across loop iterations. `select!`
    // otherwise creates and drops a fresh `notified()` each turn, leaving
    // a window between iterations where no waiter is registered. `Stop`
    // signals shutdown with `notify_waiters()`, which wakes only the
    // waiters registered at that instant and stores no permit — so a
    // notify landing in that window is LOST and the daemon never exits
    // (`accept()` then blocks forever waiting for a connection that never
    // comes). A persistently-registered pinned waiter cannot miss it.
    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => {
                break;
            }
            accept = listener.accept() => {
                let stream = accept?;
                let state = state.clone();
                tokio::spawn(async move {
                    if let Err(error) = handle_connection(stream, state).await {
                        StderrJsonDiagnosticSink.emit(
                            DiagnosticEvent::error(
                                DiagnosticComponent::Daemon,
                                DiagnosticCode::ConnectionError,
                                "daemon connection error",
                            )
                            .with_field("error", error),
                        );
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
        // The lock sits beside the socket in the machine-account home
        // (`~/.airc/daemon-v<N>.sock.lock`) — no temp dir, no hashing.
        // Same owner ⇒ same socket path ⇒ same lock ⇒ one daemon.
        let lock_path = lock_path_for(socket_path);
        if let Some(parent) = lock_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
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

/// `<socket>.lock` beside the socket. The socket path is already unique
/// per machine account, so the lock inherits that uniqueness — no temp
/// dir, no hashing.
fn lock_path_for(socket_path: &Path) -> PathBuf {
    let mut raw = socket_path.as_os_str().to_os_string();
    raw.push(".lock");
    PathBuf::from(raw)
}

fn map_ipc_kind(kind: IpcKind) -> Kind {
    match kind {
        IpcKind::Message => Kind::Message,
        IpcKind::Event => Kind::Event,
        IpcKind::Command => Kind::Command,
        IpcKind::CommandResult => Kind::CommandResult,
        IpcKind::Signal => Kind::Signal,
        IpcKind::StreamChunk => Kind::StreamChunk,
        IpcKind::Control => Kind::Control,
    }
}

fn map_ipc_delivery(delivery: IpcDelivery) -> DeliveryClass {
    match delivery {
        IpcDelivery::Durable => DeliveryClass::Durable,
        IpcDelivery::EphemeralLatest => DeliveryClass::EphemeralLatest,
        IpcDelivery::EphemeralWindow => DeliveryClass::EphemeralWindow,
        IpcDelivery::RequestResponse => DeliveryClass::RequestResponse,
        IpcDelivery::StreamChunk => DeliveryClass::StreamChunk,
    }
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
    // The owner-core router subscribes per channel (no global table to
    // scan). A client attaches once per room it cares about.
    let channel = match attach.channel {
        Some(channel) => channel,
        None => {
            return write_response(
                &mut writer,
                &Response::Error {
                    message: "attach requires a channel in the owner-core model".to_string(),
                },
            )
            .await;
        }
    };
    // Resume strictly after the client's cursor (replay the gap missed
    // while detached), then go live with no dup at the seam. `from` is
    // advanced as we send, so a re-subscribe after a lag drop resumes
    // exactly where we left off.
    //
    // Card 7d5b6a65: `from_now` overrides any cursor the client passed
    // with the channel's current head — the agent-Monitor live-tail
    // shape. The router's `subscribe_with_lag` interprets a
    // forward-pointing cursor as "nothing newer than this yet," so the
    // ring snapshot + deep replay legs return empty and we go straight
    // to live.
    let mut from = if attach.from_now {
        state.router.head_cursor(channel)
    } else {
        attach
            .from
            .map(|c| Cursor::new(Seq::new(c.epoch, c.counter), c.event_id))
    };

    // Card 7d5b6a65: `coalesce_backlog` lets the daemon collapse all
    // historical catch-up into ONE `AttachCursorAdvanced` summary
    // frame instead of streaming each event individually. We track the
    // ring-snapshot's high-water cursor; everything at or before it is
    // backlog (collapsed), everything after it is live (streamed
    // event-by-event as before).
    let coalesce_backlog = attach.coalesce_backlog && !attach.from_now;

    // Compile the consumer's kind/delivery/header filters into the router
    // filter, applied ROUTER-SIDE — the daemon never fans out an event a
    // consumer would discard (Hermes → Command/CommandResult; Continuum →
    // scoped `forge.*` headers; a media tap → StreamChunk only).
    let mut filter = Filter::channel(channel);
    if let Some(kinds) = attach.kinds {
        filter = filter.with_kinds(kinds.into_iter().map(map_ipc_kind).collect());
    }
    if let Some(delivery) = attach.delivery {
        filter = filter.with_delivery(delivery.into_iter().map(map_ipc_delivery).collect());
    }
    filter = filter.with_headers(attach.headers);

    // Subscribe BEFORE acking. Once the client sees `Ok`, the
    // subscription is already registered at the live edge, so a publish
    // can't race in between the ack and the subscription (the gap that
    // would drop early events under concurrent senders). `subscribe_with_lag`
    // also keeps a slow IPC client from stalling fan-out to other
    // subscribers (§3.5); on lag we re-subscribe from `from`.
    let mut pending = Some(state.router.subscribe_with_lag(filter.clone(), from));
    write_response(&mut writer, &Response::Ok).await?;

    // Pin one shutdown waiter across re-subscribes so a `notify_waiters`
    // can't be lost between iterations (same discipline as `run`).
    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);

    // Card 7d5b6a65 catch-up tracking. When `coalesce_backlog` is set,
    // we count events until the ring's live-edge cursor (captured at
    // subscribe time) is reached, then emit ONE summary frame and
    // switch to per-event live streaming.
    let mut catchup = if coalesce_backlog {
        Some(BacklogCatchup::new(state.router.head_cursor(channel)))
    } else {
        None
    };

    loop {
        let (stream, lag) = pending
            .take()
            .unwrap_or_else(|| state.router.subscribe_with_lag(filter.clone(), from));
        tokio::pin!(stream);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => return Ok(()),
                next = stream.next() => match next {
                    Some(env) => {
                        from = Some(env.cursor());
                        let suppressed = match catchup.as_mut() {
                            Some(c) => c.observe(env.cursor()),
                            None => false,
                        };
                        if suppressed {
                            // Inside catch-up window — count and skip,
                            // a single AttachCursorAdvanced will flush
                            // when we cross the live edge.
                        } else {
                            // Flush a pending summary BEFORE the first
                            // live event so the client sees the seam.
                            if let Some(summary) =
                                catchup.as_mut().and_then(BacklogCatchup::take_summary)
                            {
                                if summary.skipped > 0 {
                                    write_response(&mut writer, &summary.into_response())
                                        .await?;
                                }
                            }
                            write_response(
                                &mut writer,
                                &Response::Event {
                                    envelope: airc_wire::encode(&env).to_vec(),
                                },
                            )
                            .await?;
                        }
                        if lag.is_lagged() {
                            // Dropped a live push — break to re-resume
                            // from the last cursor we sent (no gap).
                            break;
                        }
                    }
                    None => return Ok(()),
                },
            }
        }
    }
}

/// Card 7d5b6a65: tracks the catch-up phase of an `attach` with
/// `coalesce_backlog: true`. Counts envelopes at or before the
/// snapshot live edge (captured at subscribe time) so the daemon can
/// emit ONE `Response::AttachCursorAdvanced` summary at the live seam
/// instead of forwarding each historical envelope.
struct BacklogCatchup {
    /// Cursor of the most recent envelope in the ring at subscribe
    /// time. Anything at or before is backlog; anything after is live.
    /// `None` means the channel was empty at subscribe — there is no
    /// backlog phase to coalesce; the first event is live.
    live_edge: Option<Cursor>,
    /// Number of envelopes suppressed during catch-up so far.
    skipped: u64,
    /// Cursor of the most recent suppressed envelope; advances as we
    /// observe more backlog. Reported in the summary so the client
    /// can persist it for future reconnects.
    last_skipped_cursor: Option<Cursor>,
    /// Set once when we cross the live edge so subsequent events skip
    /// the per-cursor comparison and stream as live.
    crossed: bool,
}

impl BacklogCatchup {
    fn new(live_edge: Option<Cursor>) -> Self {
        Self {
            live_edge,
            skipped: 0,
            last_skipped_cursor: None,
            crossed: live_edge.is_some(),
        }
    }

    /// Observe one envelope's cursor. Returns `true` when the envelope
    /// is inside the catch-up window (caller should suppress it) and
    /// `false` once we've crossed the live edge.
    fn observe(&mut self, cursor: Cursor) -> bool {
        if !self.crossed {
            // No live_edge means the channel was empty at subscribe,
            // so EVERYTHING that arrives is by definition live (no
            // backlog phase).
            return false;
        }
        if let Some(edge) = self.live_edge {
            if cursor.is_after(&edge) {
                self.crossed = false; // we've moved past catchup
                return false;
            }
            self.skipped = self.skipped.saturating_add(1);
            self.last_skipped_cursor = Some(cursor);
            return true;
        }
        false
    }

    /// Pull the catch-up summary once we've crossed the live edge.
    /// Returns `None` if there's nothing pending (already taken or
    /// catch-up never had backlog).
    fn take_summary(&mut self) -> Option<BacklogSummary> {
        if self.crossed {
            return None;
        }
        // crossed=false at this point means either (a) we observed
        // something past the edge — pull the summary OR (b) we never
        // had a live_edge to begin with. Mark crossed so we don't
        // re-emit.
        let skipped = self.skipped;
        let advanced_to = self.last_skipped_cursor;
        self.skipped = 0;
        self.last_skipped_cursor = None;
        self.crossed = true;
        advanced_to.map(|cursor| BacklogSummary { skipped, cursor })
    }
}

struct BacklogSummary {
    skipped: u64,
    cursor: Cursor,
}

impl BacklogSummary {
    fn into_response(self) -> Response {
        Response::AttachCursorAdvanced {
            skipped: self.skipped,
            advanced_to: airc_ipc::request::IpcCursor {
                epoch: self.cursor.seq.epoch,
                counter: self.cursor.seq.counter,
                event_id: self.cursor.event_id,
            },
        }
    }
}

async fn write_response<W>(writer: &mut W, response: &Response) -> Result<(), DaemonError>
where
    W: AsyncWriteExt + Unpin,
{
    write_frame(writer, response).await.map_err(DaemonError::Io)
}
