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

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs2::FileExt;
use futures::StreamExt;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use airc_bus::envelope::{Cursor, DeliveryClass, Envelope, Kind};
use airc_bus::{Filter, Seq};
use airc_diagnostics::{
    DiagnosticCode, DiagnosticComponent, DiagnosticEvent, DiagnosticSink, StderrJsonDiagnosticSink,
};
use airc_ipc::codec::{read_frame, write_encoded_frame, write_frame};
use airc_ipc::request::{AttachRequest, AttachStart, IpcDelivery, IpcKind, Request};
use airc_ipc::response::Response;
use airc_ipc::transport::{IpcAcceptError, IpcListener, IpcStream};

use crate::handlers::dispatch;
use crate::state::DaemonState;

/// What can go wrong running the daemon.
#[derive(Debug)]
pub enum DaemonError {
    /// Another daemon already owns this IPC endpoint.
    AlreadyRunning(PathBuf),
    /// Socket bind or connection I/O failure.
    Io(std::io::Error),
    /// Listener failure, retaining the accept operation and OS error.
    Accept(IpcAcceptError),
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
            DaemonError::Accept(error) => write!(f, "daemon accept: {error}"),
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
            DaemonError::Accept(error) => Some(error),
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
    run_with_startup_check(state, socket_path, || Ok(())).await
}

/// Complete caller-owned startup admission at the actual bind boundary. On any
/// bind failure the check is dropped; a rejected check cleans the new endpoint
/// before any request can be served. Embedded runtimes can use `run` directly.
pub async fn run_with_startup_check(
    state: Arc<DaemonState>,
    socket_path: PathBuf,
    bound: impl FnOnce() -> std::io::Result<()>,
) -> Result<(), DaemonError> {
    // #355: a contended lock is not automatically "already running" — the
    // holder must PROVE it serves (request-response ping). A wedged holder
    // is reclaimed via the pidfile kill-handle and the acquire retried
    // once; a responsive holder keeps the lock and we bow out as before.
    let _guard = match DaemonBindGuard::acquire(&socket_path) {
        Ok(guard) => guard,
        Err(DaemonError::AlreadyRunning(path)) => {
            match crate::reclaim::reclaim_wedged_holder(&state.home, &socket_path).await {
                Some(()) => DaemonBindGuard::acquire(&socket_path)
                    .map_err(|_| DaemonError::AlreadyRunning(path))?,
                None => return Err(DaemonError::AlreadyRunning(path)),
            }
        }
        Err(error) => return Err(error),
    };
    cleanup_stale_socket(&socket_path).map_err(DaemonError::StaleSocket)?;
    let listener = IpcListener::bind(&socket_path).await?;
    if let Err(error) = bound() {
        listener.cleanup();
        return Err(error.into());
    }

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
    let idle_tracker = IdleTracker::new(state.clone());
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
                let stream = match accept {
                    Ok(stream) => stream,
                    Err(error) => {
                        let recoverable = error.is_client_disconnect();
                        let event = if recoverable {
                            DiagnosticEvent::warn(
                                DiagnosticComponent::Daemon,
                                DiagnosticCode::IpcAcceptFailed,
                                "IPC client disconnected before accept; next pipe remains available",
                            )
                        } else {
                            DiagnosticEvent::error(
                                DiagnosticComponent::Daemon,
                                DiagnosticCode::IpcAcceptFailed,
                                "IPC listener failed; daemon is exiting",
                            )
                        };
                        let mut event = event
                            .with_field("stage", error.stage())
                            .with_field("recoverable", recoverable)
                            .with_field("error", &error);
                        if let Some(code) = error.raw_os_error() {
                            event = event.with_field("os_error", code);
                        }
                        StderrJsonDiagnosticSink.emit(event);
                        if recoverable {
                            // A new pipe awaits a new client. No retry of the
                            // failed handle, polling timer, or generic error loop.
                            continue;
                        }
                        return Err(DaemonError::Accept(error));
                    }
                };
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
    /// The live-connection count lives on `DaemonState::connections`
    /// (one fact, one place — `Status` reports the same number).
    state: Arc<DaemonState>,
    last_activity_ms: AtomicU64,
}

impl IdleTracker {
    fn new(state: Arc<DaemonState>) -> Arc<Self> {
        Arc::new(Self {
            start: Instant::now(),
            state,
            last_activity_ms: AtomicU64::new(0),
        })
    }

    /// Register a newly accepted connection. The returned guard MUST
    /// live as long as the connection task — its Drop is what marks
    /// the connection closed and stamps the idle clock.
    fn connection_opened(self: &Arc<Self>) -> ConnectionGuard {
        self.state.connections.fetch_add(1, Ordering::SeqCst);
        ConnectionGuard(self.clone())
    }

    /// `Some(duration since the daemon last had a client)` when no
    /// client is connected; `None` while any connection is live.
    fn idle_for(&self) -> Option<Duration> {
        if self.state.connections.load(Ordering::SeqCst) > 0 {
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
        self.0.state.connections.fetch_sub(1, Ordering::SeqCst);
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
        return stream_attach(reader, writer, state, attach).await;
    }

    let response = dispatch(state, request).await;
    write_response(&mut writer, &response).await?;
    // Drop reader+writer (and thus the underlying stream) so the
    // client's read sees EOF promptly.
    Ok(())
}

async fn stream_attach<R, W>(
    reader: R,
    mut writer: W,
    state: Arc<DaemonState>,
    attach: AttachRequest,
) -> Result<(), DaemonError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    // Card c0cb6cdc: the request destructures into typed parts — the
    // start position is already an `AttachStart`, decoded once in
    // `AttachRequest::start`. No flag precedence to re-derive here.
    let parts = attach.into_parts();

    // A channel SET rides one stream (`stream_attach_set`); the single
    // channel keeps the path below unchanged, so an older client's
    // request is served exactly as before.
    if parts.channels.is_some() {
        return stream_attach_set(reader, writer, state, parts).await;
    }

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
    // Live registration is atomic at the router and does not read history.
    // Explicit cursor resumes retain the existing replay/live seam.
    let mut from = match parts.start {
        AttachStart::Live | AttachStart::FromTranscriptStart => None,
        AttachStart::After(c) => Some(Cursor::new(Seq::new(c.epoch, c.counter), c.event_id)),
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
    let (stream, lag) = if parts.start == AttachStart::Live {
        let (stream, lag) = state.router.subscribe_live_with_lag(filter.clone());
        (stream.boxed(), lag)
    } else {
        let (stream, lag) = state.router.subscribe_with_lag(filter.clone(), from);
        (stream.boxed(), lag)
    };
    let mut pending = Some((stream, lag));
    write_response(&mut writer, &Response::Ok).await?;

    // Pin one shutdown waiter across re-subscribes so a `notify_waiters`
    // can't be lost between iterations (same discipline as `run`).
    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);

    // The client's half of the socket is the ONLY signal that it hung up.
    // Before this arm existed the reader sat unread for the stream's
    // whole life, so a subscriber that closed — a core that died, a
    // citizen re-opening on a membership epoch — left its daemon-side
    // socket open until the channel's NEXT event tripped EPIPE on the
    // write; on a quiet room, never. Measured 2026-09-12 (card e28889cc):
    // 4,190 sockets on one daemon, ~3,500 with no peer, climbing with
    // every core restart. Pinned once like `shutdown` so no hang-up is
    // lost across re-subscribes.
    let hangup = client_hung_up(reader);
    tokio::pin!(hangup);

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
        // "One page back" (Discord analogy): `backlog_tail` asks for
        // the N most-recent backlog events at the seam; everything
        // older stays coalesced into the summary. 0/None = today's
        // all-or-nothing coalesce.
        Some(BacklogCatchup::new(
            edge,
            parts.backlog_tail.unwrap_or(0) as usize,
        ))
    } else {
        None
    };

    // CURSOR HEARTBEAT (continuum #261, PR #2057 review): the seam summary
    // above was the ONLY `AttachCursorAdvanced` a consumer ever saw — during
    // live streaming the watermark never advanced, so a consumer persisting
    // cursors re-received the WHOLE session's events on its next attach
    // (2026-07-30: five reboots × full-session redelivery = persona echo
    // storm). After each forwarded event, emit an advance frame carrying that
    // event's cursor — throttled to one per second so a StreamChunk burst
    // (attaches subscribe kinds: None) never becomes a frame/persist storm.
    // The un-advanced tail at shutdown is now ≤1s of events instead of the
    // whole session. Initialized in the past so the FIRST event always
    // advances (a quiet room reconnecting tightens immediately).
    // Opt-in per attach (`cursor_heartbeat`): a stream that never persists a
    // cursor never receives bookkeeping it would only have to ignore.
    const ADVANCE_EVERY: std::time::Duration = std::time::Duration::from_secs(1);
    let mut last_advance = std::time::Instant::now()
        .checked_sub(ADVANCE_EVERY)
        .unwrap_or_else(std::time::Instant::now);

    loop {
        let (stream, lag) = pending.take().unwrap_or_else(|| {
            let (stream, lag) = state.router.subscribe_with_lag(filter.clone(), from);
            (stream.boxed(), lag)
        });
        tokio::pin!(stream);
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => return Ok(()),
                // Client gone: drop the subscription with the stream.
                _ = &mut hangup => return Ok(()),
                next = stream.next() => match next {
                    Some(env) => {
                        from = Some(env.cursor());
                        let suppressed = match catchup.as_mut() {
                            Some(c) => c.observe(&env),
                            None => false,
                        };
                        if suppressed {
                            // Inside catch-up window — count and skip
                            // (buffering the tail_cap most recent),
                            // the seam flush happens when we cross the
                            // live edge.
                        } else {
                            // Flush the pending seam BEFORE the first
                            // live event so the client sees the
                            // catch-up boundary. Order: (a) buffered
                            // tail envelopes as normal Event frames,
                            // oldest first, (b) the summary carrying
                            // the watermark, (c) the live event below.
                            //
                            // INVARIANT (continuum #261 discipline):
                            // the summary's `advanced_to` is the LAST
                            // suppressed cursor, which is ≥ every tail
                            // cursor, and the tail is written BEFORE
                            // the summary — so a consumer that
                            // persists the watermark from the summary
                            // never persists past an event it was not
                            // delivered.
                            //
                            // Known limitation (same as the summary
                            // since day one, not fixed here): the seam
                            // only flushes when the first LIVE event
                            // arrives — on an idle room the tail and
                            // summary wait for live traffic.
                            if let Some(seam) =
                                catchup.as_mut().and_then(BacklogCatchup::take_summary)
                            {
                                for buffered in &seam.tail {
                                    tokio::select! {
                                        _ = &mut shutdown => return Ok(()),
                                        _ = &mut hangup => return Ok(()),
                                        result = write_event_response(&mut writer, buffered, &state.shared_frames) => result?,
                                    }
                                }
                                write_response(&mut writer, &seam.summary.into_response())
                                    .await?;
                            }
                            tokio::select! {
                                _ = &mut shutdown => return Ok(()),
                                _ = &mut hangup => return Ok(()),
                                result = write_event_response(&mut writer, &env, &state.shared_frames) => result?,
                            }
                            // Cursor heartbeat: tell the consumer this event
                            // is now safely delivered on this stream so its
                            // persisted watermark can advance past it.
                            // Only a consumer that asked (it persists its cursor)
                            // gets the frame; every other stream would only have
                            // to ignore it (airc #1416).
                            if parts.cursor_heartbeat && last_advance.elapsed() >= ADVANCE_EVERY {
                                last_advance = std::time::Instant::now();
                                let c = env.cursor();
                                write_response(
                                    &mut writer,
                                    &Response::AttachCursorAdvanced {
                                        skipped: 0,
                                        advanced_to: airc_ipc::request::IpcCursor {
                                            epoch: c.seq.epoch,
                                            counter: c.seq.counter,
                                            event_id: c.event_id,
                                        },
                                    },
                                )
                                .await?;
                            }
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

/// Serve an `Attach` that names a CHANNEL SET on one stream
/// (`AttachRequest::channel_set`). Why: the router subscribes per
/// channel, so a subscriber of N rooms used to hold N sockets, N daemon
/// tasks and N reader tasks — measured 2026-10-04 on a 15-subscriber
/// continuum core: ~960 attach sockets to one daemon, growing with every
/// room and citizen. Here the N router subscriptions (in-process mpsc
/// receivers, cheap) are merged and written down ONE socket; the client
/// routes each frame by the envelope's own `channel`.
///
/// Per room, independently: the start (`Live`, or resume after its own
/// cursor), the resume point, and the router's lag flag. A lagged room
/// is re-subscribed from ITS resume point and nothing else is touched:
/// the merge is a keyed `StreamMap`, so replacing one room's stream
/// leaves every sibling's queued events in place (Astra's review of
/// #1523: a rebuild of the whole set dropped a sibling's queued line and
/// re-opened a cursorless sibling at a newer edge). A room's resume
/// point before it has delivered anything is the ring head read BEFORE
/// its live registration, so a lag that drops its first events replays
/// exactly those; a room empty at attach resumes from the ring start,
/// which is everything since. Order within a room is the router's;
/// across rooms none was ever promised (they were separate sockets).
///
/// Not served on a set, refused before the ack: `coalesce_backlog` and
/// `cursor_heartbeat` (their `AttachCursorAdvanced` frame names no
/// channel yet), and an empty set. Loud, so a client never waits on a
/// stream the daemon silently narrowed.
async fn stream_attach_set<R, W>(
    reader: R,
    mut writer: W,
    state: Arc<DaemonState>,
    parts: airc_ipc::AttachParts,
) -> Result<(), DaemonError>
where
    R: AsyncReadExt + Unpin,
    W: AsyncWriteExt + Unpin,
{
    let refuse = |message: &str| Response::Error {
        message: message.to_string(),
    };
    let Some(entries) = parts.channels else {
        return write_response(&mut writer, &refuse("attach: no channel set")).await;
    };
    if entries.is_empty() {
        return write_response(
            &mut writer,
            &refuse("attach: an empty channel set subscribes to nothing"),
        )
        .await;
    }
    if parts.coalesce_backlog || parts.cursor_heartbeat {
        return write_response(
            &mut writer,
            &refuse("attach: coalesce_backlog and cursor_heartbeat are single-channel only (the cursor frame names no channel)"),
        )
        .await;
    }
    let kinds: Option<Vec<Kind>> = parts
        .kinds
        .map(|k| k.into_iter().map(map_ipc_kind).collect());
    let delivery: Option<Vec<DeliveryClass>> = parts
        .delivery
        .map(|d| d.into_iter().map(map_ipc_delivery).collect());
    let headers = parts.headers;
    let filter_for = |room: airc_core::RoomId| {
        let mut filter = Filter::channel(room);
        if let Some(kinds) = kinds.clone() {
            filter = filter.with_kinds(kinds);
        }
        if let Some(delivery) = delivery.clone() {
            filter = filter.with_delivery(delivery);
        }
        filter.with_headers(headers.clone())
    };

    /// One room of the set: where a re-subscription resumes, and the
    /// router's lag flag for its current stream.
    struct RoomSub {
        resume: Option<Cursor>,
        lag: airc_bus::LagFlag,
    }
    // Register every room BEFORE the ack — the same subscribe-before-ack
    // contract as the single channel: once the client sees `Ok`, no
    // room has a gap between ack and registration. A room named twice
    // gets one subscription, the first entry's start.
    let mut subs: std::collections::HashMap<airc_core::RoomId, RoomSub> = Default::default();
    let mut merged: tokio_stream::StreamMap<
        airc_core::RoomId,
        futures::stream::BoxStream<'static, Arc<Envelope>>,
    > = tokio_stream::StreamMap::new();
    for entry in entries {
        if subs.contains_key(&entry.channel) {
            continue;
        }
        let room = entry.channel;
        let (stream, lag, resume) = match entry.start() {
            AttachStart::After(c) => {
                let cursor = Cursor::new(Seq::new(c.epoch, c.counter), c.event_id);
                let (stream, lag) = state
                    .router
                    .subscribe_with_lag(filter_for(room), Some(cursor));
                (stream.boxed(), lag, Some(cursor))
            }
            // No cursor is the live edge, never a ring replay. The resume
            // point is the head read BEFORE registering, so a lag before
            // the first delivery replays exactly what was dropped.
            AttachStart::Live | AttachStart::FromTranscriptStart => {
                let edge = state.router.head_cursor(room);
                let (stream, lag) = state.router.subscribe_live_with_lag(filter_for(room));
                (stream.boxed(), lag, edge)
            }
        };
        merged.insert(room, stream);
        subs.insert(room, RoomSub { resume, lag });
    }
    write_response(&mut writer, &Response::Ok).await?;

    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);
    let hangup = client_hung_up(reader);
    tokio::pin!(hangup);

    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return Ok(()),
            _ = &mut hangup => return Ok(()),
            next = merged.next() => match next {
                Some((room, env)) => {
                    if let Some(sub) = subs.get_mut(&room) {
                        sub.resume = Some(env.cursor());
                    }
                    tokio::select! {
                        _ = &mut shutdown => return Ok(()),
                        _ = &mut hangup => return Ok(()),
                        result = write_event_response(&mut writer, &env, &state.shared_frames) => result?,
                    }
                    // Only a room that dropped a live push is re-subscribed,
                    // from its own resume point; its siblings keep their
                    // streams and everything queued on them.
                    for (room, sub) in subs.iter_mut() {
                        if !sub.lag.is_lagged() {
                            continue;
                        }
                        let (stream, lag) = state
                            .router
                            .subscribe_with_lag(filter_for(*room), sub.resume);
                        merged.insert(*room, stream.boxed());
                        sub.lag = lag;
                    }
                }
                // Every router stream ended: the router is shutting down.
                None => return Ok(()),
            },
        }
    }
}

/// Resolves when the attach client's side of the socket closes: EOF
/// (`Ok(0)`) or a transport error (a Windows named pipe reports the
/// disconnect as an error). Bytes a client writes on an attach stream
/// carry no protocol meaning and are drained — the arm must stay armed
/// for the stream's whole life, not trip on chatter.
async fn client_hung_up<R: AsyncReadExt + Unpin>(mut reader: R) {
    let mut scratch = [0u8; 64];
    loop {
        match reader.read(&mut scratch).await {
            Ok(0) | Err(_) => return,
            Ok(_) => continue,
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
    /// "One page back" (card 7d5b6a65 extension): how many of the
    /// most-recent suppressed envelopes to deliver as real Event
    /// frames at the seam. 0 = classic all-or-nothing coalesce.
    tail_cap: usize,
    /// The `tail_cap` most-recent suppressed envelopes, oldest first.
    /// `skipped` keeps counting ALL suppressed envelopes; the summary
    /// subtracts what the tail actually delivers.
    tail: VecDeque<Arc<Envelope>>,
}

impl BacklogCatchup {
    fn new(live_edge: Option<Cursor>, tail_cap: usize) -> Self {
        Self {
            live_edge,
            skipped: 0,
            last_skipped_cursor: None,
            crossed: live_edge.is_some(),
            tail_cap,
            tail: VecDeque::new(),
        }
    }

    /// Observe one envelope. Returns `true` when the envelope is
    /// inside the catch-up window (caller should suppress it; the
    /// `tail_cap` most recent are buffered for the seam flush) and
    /// `false` once we've crossed the live edge.
    fn observe(&mut self, env: &Arc<Envelope>) -> bool {
        if !self.crossed {
            // No live_edge means the channel was empty at subscribe,
            // so EVERYTHING that arrives is by definition live (no
            // backlog phase).
            return false;
        }
        if let Some(edge) = self.live_edge {
            let cursor = env.cursor();
            if cursor.is_after(&edge) {
                self.crossed = false; // we've moved past catchup
                return false;
            }
            self.skipped = self.skipped.saturating_add(1);
            self.last_skipped_cursor = Some(cursor);
            if self.tail_cap > 0 {
                if self.tail.len() == self.tail_cap {
                    self.tail.pop_front();
                }
                self.tail.push_back(Arc::clone(env));
            }
            return true;
        }
        false
    }

    /// Pull the seam flush once we've crossed the live edge: the
    /// buffered tail plus the coalesce summary. Returns `None` if
    /// there's nothing pending (already taken or catch-up never had
    /// backlog). The gate is TOTAL suppressed (> 0), not the
    /// post-tail remainder: when the whole backlog fit in the tail
    /// the summary's `skipped` is 0 but the client still needs the
    /// watermark frame to persist its cursor.
    fn take_summary(&mut self) -> Option<BacklogSeam> {
        if self.crossed {
            return None;
        }
        // crossed=false at this point means either (a) we observed
        // something past the edge — pull the seam OR (b) we never
        // had a live_edge to begin with. Mark crossed so we don't
        // re-emit.
        let delivered = self.tail.len() as u64;
        let total_suppressed = self.skipped;
        let advanced_to = self.last_skipped_cursor;
        let tail = std::mem::take(&mut self.tail);
        self.skipped = 0;
        self.last_skipped_cursor = None;
        self.crossed = true;
        advanced_to.map(|cursor| BacklogSeam {
            tail,
            summary: BacklogSummary {
                // Only events NOT delivered in the tail count as
                // skipped in the summary the client renders.
                skipped: total_suppressed.saturating_sub(delivered),
                cursor,
            },
        })
    }
}

/// Everything the daemon writes at the catch-up→live seam: the "one
/// page back" tail (possibly empty) followed by the summary watermark.
struct BacklogSeam {
    tail: VecDeque<Arc<Envelope>>,
    summary: BacklogSummary,
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

/// Share the canonical event frame for this envelope allocation across attach
/// writers. The receiving API and its owned Response::Event remain unchanged.
async fn write_event_response<W>(
    writer: &mut W,
    envelope: &Arc<airc_bus::Envelope>,
    frames: &crate::shared_frames::SharedFrames,
) -> Result<(), DaemonError>
where
    W: AsyncWriteExt + Unpin,
{
    let frame = frames.get(envelope).await?;
    write_encoded_frame(writer, &frame.payload)
        .await
        .map_err(DaemonError::Io)
}

#[cfg(test)]
mod shared_frame_cancellation_tests {
    use super::*;
    use airc_core::{ClientId, PeerId, RoomId};
    use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
    use airc_store::InMemoryEventStore;

    #[tokio::test]
    async fn saturated_attach_admission_observes_client_hangup_and_shutdown() {
        let home = tempfile::tempdir().unwrap();
        let mut state = DaemonState::build(
            PeerId::new(),
            PeerKeypair::generate(),
            Arc::new(PeerKeyRegistry::new()),
            VerificationPolicy::Strict,
            home.path().to_owned(),
            &home.path().join("events.sqlite"),
            Arc::new(InMemoryEventStore::new()),
            crate::DaemonRuntimeInfo::unknown(),
        )
        .await
        .unwrap();
        state.shared_frames = crate::shared_frames::SharedFrames::new(
            1,
            2 * airc_ipc::codec::MAX_FRAME_BYTES as usize,
        );
        let state = Arc::new(state);
        let held_envelope = Arc::new(Envelope::new(
            RoomId::new(),
            (PeerId::new(), ClientId::new()),
            Kind::StreamChunk,
            DeliveryClass::StreamChunk,
            bytes::Bytes::from_static(b"held by another writer"),
        ));
        let held = state.shared_frames.get(&held_envelope).await.unwrap();
        for shutdown in [false, true] {
            let channel = RoomId::new();
            let (mut client, daemon) = tokio::io::duplex(1024);
            let (reader, writer) = tokio::io::split(daemon);
            let task = tokio::spawn(stream_attach(
                reader,
                writer,
                state.clone(),
                AttachRequest::new(channel, AttachStart::Live),
            ));
            assert!(matches!(
                read_frame::<_, Response>(&mut client).await.unwrap(),
                Some(Response::Ok)
            ));
            let waiting = state.shared_frames.admission_waiting.notified();
            tokio::pin!(waiting);
            waiting.as_mut().enable();
            state
                .router
                .publish(Envelope::new(
                    channel,
                    (PeerId::new(), ClientId::new()),
                    Kind::StreamChunk,
                    DeliveryClass::StreamChunk,
                    bytes::Bytes::from_static(b"must await entry budget"),
                ))
                .await
                .unwrap();
            tokio::time::timeout(Duration::from_secs(2), waiting)
                .await
                .expect("actual cache admission must be saturated");
            let mut client = Some(client);
            if shutdown {
                state.shutdown.notify_waiters();
            } else {
                drop(client.take());
            }
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .expect("existing cancellation must interrupt admission")
                .unwrap()
                .unwrap();
            drop(client);
        }
        drop(held);
        // No cancelled waiter can retain the admission slot after its writer is
        // gone. This uses a fresh allocation and the real cache path.
        tokio::time::timeout(
            Duration::from_secs(2),
            state.shared_frames.get(&Arc::new((*held_envelope).clone())),
        )
        .await
        .expect("cancelled attach must release admission state")
        .unwrap();
    }

    mod channel_set {
        use super::*;
        use airc_ipc::ChannelAttach;

        async fn state() -> Arc<DaemonState> {
            let home = tempfile::tempdir().unwrap();
            let state = DaemonState::build(
                PeerId::new(),
                PeerKeypair::generate(),
                Arc::new(PeerKeyRegistry::new()),
                VerificationPolicy::Strict,
                home.path().to_owned(),
                &home.path().join("events.sqlite"),
                Arc::new(InMemoryEventStore::new()),
                crate::DaemonRuntimeInfo::unknown(),
            )
            .await
            .unwrap();
            // Keep the tempdir alive for the test by leaking it into the state's lifetime.
            std::mem::forget(home);
            Arc::new(state)
        }

        fn chat(channel: RoomId, text: &'static str) -> Envelope {
            Envelope::new(
                channel,
                (PeerId::new(), ClientId::new()),
                Kind::Message,
                DeliveryClass::Durable,
                bytes::Bytes::from_static(text.as_bytes()),
            )
        }

        async fn next_event<C: AsyncReadExt + Unpin>(client: &mut C) -> Envelope {
            match tokio::time::timeout(Duration::from_secs(2), read_frame::<_, Response>(client))
                .await
                .expect("an event frame within 2 s")
                .unwrap()
            {
                Some(Response::Event { envelope }) => {
                    airc_wire::decode(bytes::Bytes::from(envelope)).unwrap()
                }
                other => panic!("expected an Event frame, got {other:?}"),
            }
        }

        // what this catches (2026-10-04, ~960 sockets on one daemon): a set attach
        // serves N rooms on ONE stream, each frame carrying its own room so the
        // client can route it, with the order inside a room preserved — the
        // contract that lets airc-lib hold one socket per subscriber instead of
        // one per room. Also: a resume cursor is honoured per room (B replays
        // only what came after its cursor while A is live), and the client's
        // hang-up ends the stream exactly as it does for a single channel.
        #[tokio::test]
        async fn a_channel_set_streams_every_room_on_one_socket_routed_by_room() {
            let state = state().await;
            let a = RoomId::new();
            let b = RoomId::new();
            // B has history: the attach resumes after its first line. The cursor
            // is read the way a client gets it — off the delivered event — through
            // a throwaway single-channel attach from the transcript start.
            state.router.publish(chat(b, "b-old")).await.unwrap();
            state
                .router
                .publish(chat(b, "b-after-cursor"))
                .await
                .unwrap();
            let b_cursor = {
                let (mut probe, daemon) = tokio::io::duplex(64 * 1024);
                let (reader, writer) = tokio::io::split(daemon);
                let task = tokio::spawn(stream_attach(
                    reader,
                    writer,
                    state.clone(),
                    AttachRequest::new(b, AttachStart::FromTranscriptStart),
                ));
                assert!(matches!(
                    read_frame::<_, Response>(&mut probe).await.unwrap(),
                    Some(Response::Ok)
                ));
                let first = next_event(&mut probe).await;
                assert_eq!(&first.payload[..], b"b-old");
                let c = first.cursor();
                drop(probe);
                task.await.unwrap().unwrap();
                airc_ipc::IpcCursor {
                    epoch: c.seq.epoch,
                    counter: c.seq.counter,
                    event_id: c.event_id,
                }
            };
            let (mut client, daemon) = tokio::io::duplex(64 * 1024);
            let (reader, writer) = tokio::io::split(daemon);
            let request = AttachRequest::channel_set(vec![
                ChannelAttach {
                    channel: a,
                    from: None,
                },
                ChannelAttach {
                    channel: b,
                    from: Some(b_cursor),
                },
            ]);
            let task = tokio::spawn(stream_attach(reader, writer, state.clone(), request));
            assert!(matches!(
                read_frame::<_, Response>(&mut client).await.unwrap(),
                Some(Response::Ok)
            ));

            // B's resume replays the one line after its cursor, nothing older.
            let replayed = next_event(&mut client).await;
            assert_eq!(replayed.channel, b);
            assert_eq!(&replayed.payload[..], b"b-after-cursor");

            for (room, text) in [(a, "a-1"), (b, "b-1"), (a, "a-2")] {
                state.router.publish(chat(room, text)).await.unwrap();
            }
            let mut per_room: std::collections::HashMap<RoomId, Vec<Vec<u8>>> = Default::default();
            for _ in 0..3 {
                let env = next_event(&mut client).await;
                per_room
                    .entry(env.channel)
                    .or_default()
                    .push(env.payload.to_vec());
            }
            assert_eq!(
                per_room[&a],
                vec![b"a-1".to_vec(), b"a-2".to_vec()],
                "A's order is kept"
            );
            assert_eq!(per_room[&b], vec![b"b-1".to_vec()]);

            drop(client);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .expect("client hang-up ends the set stream")
                .unwrap()
                .unwrap();
        }

        // what this catches (Astra's review of #1523): a lag on ONE room must not
        // touch its siblings. The writer is held (the client does not read), A's
        // router buffer overflows so A lags, and a B line is queued meanwhile.
        // Rebuilding the whole set re-subscribed B live and dropped that queued
        // line. Now only A is re-subscribed from its own resume point: B's line
        // arrives exactly once, A's lines stay in order with no duplicate, and
        // A keeps delivering after the lag.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_lag_on_one_room_leaves_its_siblings_queued_events_untouched() {
            let state = state().await;
            let a = RoomId::new();
            let b = RoomId::new();
            let (mut client, daemon) = tokio::io::duplex(1024);
            let (reader, writer) = tokio::io::split(daemon);
            let request = AttachRequest::channel_set(vec![
                ChannelAttach {
                    channel: a,
                    from: None,
                },
                ChannelAttach {
                    channel: b,
                    from: None,
                },
            ]);
            let task = tokio::spawn(stream_attach(reader, writer, state.clone(), request));
            assert!(matches!(
                read_frame::<_, Response>(&mut client).await.unwrap(),
                Some(Response::Ok)
            ));
            // Nobody reads the client: the daemon's writer blocks on the duplex,
            // A's 1024-slot subscriber buffer fills, and the rest is dropped (lag).
            // Durable lines, so the resume can replay what the lag dropped. The
            // write-behind sink is bounded too: when it reports saturation the
            // publish yields and retries, which is backpressure, not a failure.
            async fn publish_durable(state: &DaemonState, env: Envelope) {
                loop {
                    match state.router.publish(env.clone()).await {
                        Ok(_) => return,
                        Err(airc_bus::BusError::WriteBehindSaturated) => {
                            tokio::task::yield_now().await
                        }
                        Err(e) => panic!("publish failed: {e:?}"),
                    }
                }
            }
            let flood = 1600usize;
            for i in 0..flood {
                let text: &'static str = Box::leak(format!("a-{i:04}").into_boxed_str());
                publish_durable(&state, chat(a, text)).await;
            }
            publish_durable(&state, chat(b, "b-queued-during-lag")).await;
            publish_durable(&state, chat(a, "a-final")).await;

            let mut a_seen: Vec<String> = Vec::new();
            let mut b_seen = 0usize;
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            while std::time::Instant::now() < deadline {
                let env = next_event(&mut client).await;
                let text = String::from_utf8(env.payload.to_vec()).unwrap();
                if env.channel == b {
                    assert_eq!(text, "b-queued-during-lag");
                    b_seen += 1;
                } else {
                    a_seen.push(text);
                }
                if b_seen == 1 && a_seen.last().is_some_and(|t| t == "a-final") {
                    break;
                }
            }
            assert_eq!(b_seen, 1, "the sibling's queued line arrives exactly once");
            assert_eq!(
                a_seen.last().map(String::as_str),
                Some("a-final"),
                "A resumed after its lag"
            );
            let mut sorted = a_seen.clone();
            sorted.sort();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                a_seen.len(),
                "A never repeats a line across its resume"
            );
            let numbered: Vec<&String> = a_seen.iter().filter(|t| t.starts_with("a-")).collect();
            assert!(
                numbered.windows(2).all(|w| w[0] <= w[1]),
                "A's order is kept across the resume"
            );
            // The lag is certain by construction: the daemon is blocked writing
            // into a 1 KiB duplex while A's 1024-slot buffer takes 1601 pushes.
            // Gap-free delivery is the resume replaying the dropped lines from
            // the durable sink, which is the single-channel contract kept per room.
            assert_eq!(
                a_seen.len(),
                flood + 1,
                "A's resume replays every dropped line"
            );

            drop(client);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }

        // what this catches: the shapes a set does not serve are refused BEFORE
        // the ack, loudly, so a client never waits on a silently narrowed
        // stream: an empty set, and the single-channel-only backlog/heartbeat
        // frames (their cursor frame names no channel yet).
        #[tokio::test]
        async fn a_set_refuses_before_the_ack_what_it_does_not_serve() {
            let state = state().await;
            let room = RoomId::new();
            let shapes = [
                AttachRequest::channel_set(vec![]),
                AttachRequest::channel_set(vec![ChannelAttach {
                    channel: room,
                    from: None,
                }])
                .with_coalesced_backlog(),
                AttachRequest::channel_set(vec![ChannelAttach {
                    channel: room,
                    from: None,
                }])
                .with_cursor_heartbeat(),
            ];
            for request in shapes {
                let (mut client, daemon) = tokio::io::duplex(4096);
                let (reader, writer) = tokio::io::split(daemon);
                let task = tokio::spawn(stream_attach(
                    reader,
                    writer,
                    state.clone(),
                    request.clone(),
                ));
                match read_frame::<_, Response>(&mut client).await.unwrap() {
                    Some(Response::Error { message }) => {
                        assert!(message.starts_with("attach:"), "{message}")
                    }
                    other => panic!("{request:?} must be refused, got {other:?}"),
                }
                tokio::time::timeout(Duration::from_secs(2), task)
                    .await
                    .unwrap()
                    .unwrap()
                    .unwrap();
            }
        }
    }
}
