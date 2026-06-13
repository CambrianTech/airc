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
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;

use airc_bus::envelope::{Cursor, DeliveryClass, Kind};
use airc_bus::{Filter, Seq};
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_ipc::codec::{read_frame, write_frame};
use airc_ipc::request::{AttachRequest, AttachStart, IpcDelivery, IpcKind, Request};
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
/// a Stop request handler), the temp-home idle watchdog trips (card
/// f122b5b5), or the listener errors.
pub async fn run(state: Arc<DaemonState>, socket_path: PathBuf) -> Result<(), DaemonError> {
    let _guard = DaemonBindGuard::acquire(&socket_path)?;
    cleanup_stale_socket(&socket_path).map_err(DaemonError::StaleSocket)?;
    let listener = IpcListener::bind(&socket_path).await?;

    // Card f122b5b5: write `<home>/daemon.pid` once the bind guard is
    // held (only the WINNING daemon for this socket writes), so test
    // harnesses and operators have a portable kill handle for daemons
    // they spawned. Removed on graceful exit by the guard below.
    let _pid_file = PidFileGuard::write(&state.home);

    // Card f122b5b5 belt-and-braces: a daemon whose home is temp-rooted
    // (#1150 detection) is a hermetic test daemon — if its test runner
    // dies without tearing it down (SIGKILL escapes every Drop guard),
    // it must exit BY ITSELF once no client has been connected for the
    // idle window. Production homes never start this watchdog.
    let idle_tracker = IdleTracker::new();
    let watchdog = spawn_temp_home_idle_watchdog(&state, &idle_tracker);

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
                let connection = idle_tracker.connection_opened();
                tokio::spawn(async move {
                    // Held for the connection's whole life; dropping it
                    // stamps the idle clock for the watchdog.
                    let _connection = connection;
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
    if let Some(watchdog) = watchdog {
        watchdog.abort();
    }
    Ok(())
}

/// Default idle window for the temp-home self-exit policy: five
/// minutes without a single connected client. Long enough that a
/// healthy test (bounded waits, card d2ba719c) never trips it;
/// short enough that an orphaned daemon frees its RAM + event loop
/// promptly instead of accumulating by the hundreds (card f122b5b5:
/// 800+ leaked temp-home daemons killed by hand in one session).
const TEMP_HOME_IDLE_EXIT_DEFAULT: Duration = Duration::from_secs(300);

/// Env override for the idle window, in milliseconds
/// (`AIRC_TEMP_HOME_IDLE_EXIT_MS`). Exists so the self-exit tests can
/// run the policy in seconds, not minutes; a malformed value falls
/// back to the default LOUDLY rather than disabling the policy.
const TEMP_HOME_IDLE_EXIT_ENV: &str = "AIRC_TEMP_HOME_IDLE_EXIT_MS";

fn temp_home_idle_exit_window() -> Duration {
    let Some(raw) = std::env::var_os(TEMP_HOME_IDLE_EXIT_ENV) else {
        return TEMP_HOME_IDLE_EXIT_DEFAULT;
    };
    match raw.to_str().and_then(|v| v.trim().parse::<u64>().ok()) {
        Some(ms) if ms > 0 => Duration::from_millis(ms),
        _ => {
            eprintln!(
                "airc daemon: ignoring malformed {TEMP_HOME_IDLE_EXIT_ENV}={raw:?} — \
                 using default {TEMP_HOME_IDLE_EXIT_DEFAULT:?}"
            );
            TEMP_HOME_IDLE_EXIT_DEFAULT
        }
    }
}

/// Card f122b5b5: when the daemon's home is temp-rooted (#1150's
/// detection — hermetic test/CI daemon, never production), spawn a
/// watchdog that fires the shutdown notifier once no client has been
/// connected for the idle window. Returns `None` for production homes
/// — the policy cannot touch them by construction.
fn spawn_temp_home_idle_watchdog(
    state: &Arc<DaemonState>,
    tracker: &Arc<IdleTracker>,
) -> Option<tokio::task::JoinHandle<()>> {
    if !airc_core::scope_home_is_temp_rooted(&state.home) {
        return None;
    }
    let window = temp_home_idle_exit_window();
    eprintln!(
        "airc daemon: temp-home idle self-exit policy ACTIVE (card f122b5b5) — home {} is \
         temp-rooted; exiting after {window:?} with no connected client \
         (override: {TEMP_HOME_IDLE_EXIT_ENV})",
        state.home.display()
    );
    let state = state.clone();
    let tracker = tracker.clone();
    // Poll often enough that a test-configured sub-second window trips
    // promptly, but never busier than 20Hz and never lazier than 5s.
    let poll = (window / 10).clamp(Duration::from_millis(50), Duration::from_secs(5));
    Some(tokio::spawn(async move {
        loop {
            tokio::time::sleep(poll).await;
            let Some(idle) = tracker.idle_for() else {
                continue; // a client is connected — never exit under it
            };
            if idle >= window {
                eprintln!(
                    "airc daemon: temp-home idle self-exit (card f122b5b5) — no client \
                     connected for {idle:?} (window {window:?}) and home {} is temp-rooted; \
                     shutting down",
                    state.home.display()
                );
                state.shutdown.notify_waiters();
                return;
            }
        }
    }))
}

/// Tracks live connections + the instant the daemon last went idle,
/// for the temp-home self-exit watchdog. Time is stored as millis
/// elapsed since `start` so the hot paths stay lock-free atomics.
struct IdleTracker {
    start: Instant,
    connections: AtomicUsize,
    last_activity_ms: AtomicU64,
}

impl IdleTracker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            connections: AtomicUsize::new(0),
            last_activity_ms: AtomicU64::new(0),
        })
    }

    /// Register a newly accepted connection. The returned guard MUST
    /// live as long as the connection task — its Drop is what marks
    /// the connection closed and stamps the idle clock.
    fn connection_opened(self: &Arc<Self>) -> ConnectionGuard {
        self.connections.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard(self.clone())
    }

    /// `Some(duration since the daemon last had a client)` when no
    /// client is connected; `None` while any connection is live.
    fn idle_for(&self) -> Option<Duration> {
        if self.connections.load(Ordering::SeqCst) > 0 {
            return None;
        }
        let last = Duration::from_millis(self.last_activity_ms.load(Ordering::SeqCst));
        Some(self.start.elapsed().saturating_sub(last))
    }

    fn stamp_activity(&self) {
        let elapsed_ms = u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX);
        self.last_activity_ms.store(elapsed_ms, Ordering::SeqCst);
    }
}

/// Drop = connection closed: decrement the live count and stamp the
/// idle clock so the watchdog's window starts from the LAST disconnect,
/// not daemon start.
struct ConnectionGuard(Arc<IdleTracker>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.stamp_activity();
        self.0.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Card f122b5b5: `<home>/daemon.pid` — written while the daemon runs,
/// removed on graceful exit. Best-effort and informational: readers
/// (test teardown guards, operators) must verify liveness before
/// trusting it. Failure to write is loud but non-fatal — a daemon that
/// can't record its pid still serves.
struct PidFileGuard {
    path: PathBuf,
}

impl PidFileGuard {
    fn write(home: &Path) -> Option<Self> {
        let path = home.join("daemon.pid");
        match std::fs::write(&path, format!("{}\n", std::process::id())) {
            Ok(()) => Some(Self { path }),
            Err(error) => {
                eprintln!(
                    "airc daemon: could not write pid file {} ({error}) — teardown \
                     guards will not find this daemon by pid",
                    path.display()
                );
                None
            }
        }
    }
}

impl Drop for PidFileGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
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
    // Card c0cb6cdc: the request destructures into typed parts — the
    // start position is already an `AttachStart`, decoded once in
    // `AttachRequest::start`. No flag precedence to re-derive here.
    let parts = attach.into_parts();

    // The owner-core router subscribes per channel (no global table to
    // scan). A client attaches once per room it cares about.
    let channel = match parts.channel {
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
    // Map the typed start onto the router cursor. `from` is advanced as
    // we send, so a re-subscribe after a lag drop resumes exactly where
    // we left off (replay the gap, no dup at the seam).
    //
    // `Live` (card 7d5b6a65): start at the channel's current head. The
    // router's `subscribe_with_lag` interprets a forward-pointing cursor
    // as "nothing newer than this yet," so the ring snapshot + deep
    // replay legs return empty and we go straight to live.
    //
    // Critical: when the in-memory ring is empty (fresh daemon, no
    // events yet this process-lifetime), `router.head_cursor` returns
    // None — but the SINK still has the durable transcript. Without
    // the sink fallback below, `Live` would fall through to a full
    // sink replay (the very bug card 7d5b6a65 closes). Query the sink
    // for its head when the ring is empty.
    let mut from = match parts.start {
        AttachStart::Live => match state.router.head_cursor(channel) {
            Some(c) => Some(c),
            None => state.router.sink_head_cursor(channel).await,
        },
        AttachStart::After(c) => Some(Cursor::new(Seq::new(c.epoch, c.counter), c.event_id)),
        AttachStart::FromTranscriptStart => None,
    };

    // Card 7d5b6a65: `coalesce_backlog` lets the daemon collapse all
    // historical catch-up into ONE `AttachCursorAdvanced` summary
    // frame instead of streaming each event individually. We track the
    // ring-snapshot's high-water cursor; everything at or before it is
    // backlog (collapsed), everything after it is live (streamed
    // event-by-event as before). `Live` has no backlog to coalesce.
    let coalesce_backlog = parts.coalesce_backlog && parts.start != AttachStart::Live;

    // Compile the consumer's kind/delivery/header filters into the router
    // filter, applied ROUTER-SIDE — the daemon never fans out an event a
    // consumer would discard (Hermes → Command/CommandResult; Continuum →
    // scoped `forge.*` headers; a media tap → StreamChunk only).
    let mut filter = Filter::channel(channel);
    if let Some(kinds) = parts.kinds {
        filter = filter.with_kinds(kinds.into_iter().map(map_ipc_kind).collect());
    }
    if let Some(delivery) = parts.delivery {
        filter = filter.with_delivery(delivery.into_iter().map(map_ipc_delivery).collect());
    }
    filter = filter.with_headers(parts.headers);

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
        // Same ring-then-sink fallback as the `from_now` path above so
        // a freshly-started daemon catching up on a real durable
        // backlog actually has a `live_edge` to compare against (an
        // empty ring with non-empty sink would otherwise treat every
        // historical event as live and emit no summary).
        let edge = match state.router.head_cursor(channel) {
            Some(c) => Some(c),
            None => state.router.sink_head_cursor(channel).await,
        };
        Some(BacklogCatchup::new(edge))
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
