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
    // NOW, THEN BACKWARD (Joel, 2026-10-06: "it should be impossible" to
    // replay history down a stream; "it's backwards entirely"). Every
    // attach registers at the live edge. A start in the past (a bookmark,
    // or the transcript start) earns ONE page: the newest <= ATTACH_PAGE
    // events, read backward from the tip after registration, then a
    // summary counting what it left out. Older history is paged on demand
    // (`Inbox { before }`), never streamed. `coalesce_backlog` and
    // `backlog_tail` are no longer read: every past start is a page.
    let past_start = past_start_of(parts.start);

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
    // The lag-resume point before anything is delivered is the head read
    // BEFORE registering (the set path's rule): a lag then re-reads only what
    // the slow client dropped, never the ring or the durable transcript.
    let mut from = match state.router.head_cursor(channel) {
        Some(c) => Some(c),
        None => state.router.sink_head_cursor(channel).await,
    };
    let (stream, lag) = state.router.subscribe_live_with_lag(filter.clone());
    let mut pending = Some((stream.boxed(), lag));
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

    // ONE PAGE, then live. Written before the first live event so the
    // consumer sees the page, then the summary, then the stream. Live events
    // the page already carried (durable ones at or before its tip) are not
    // written twice.
    let mut seen_through: Option<Cursor> = None;
    if let Some(bookmark) = past_start {
        let page = attach_page(&state, channel, &filter, bookmark)
            .await
            .map_err(|error| DaemonError::Io(std::io::Error::other(error)))?;
        for env in &page.events {
            write_event_response(&mut writer, env, &state.shared_frames).await?;
        }
        if let Some(summary) = page.summary(parts.cursor_heartbeat, None) {
            write_response(&mut writer, &summary).await?;
        }
        if page.through.is_some() {
            from = page.through;
            seen_through = page.through;
        }
    }

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
                        if env.delivery.is_durable()
                            && seen_through.is_some_and(|through| !env.cursor().is_after(&through))
                        {
                            continue; // the page already carried it
                        }
                        from = Some(env.cursor());
                        {
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
                                        channel: None,
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

/// A room of a channel-set attach that lags again within this window of its
/// last lag re-subscription is a slow client, not a transient: its resume
/// replay (up to a sink page of 1024 events) overflowed the same 1024-slot
/// buffer again. Re-subscribing in a tight loop is a deep replay per lag.
const LAG_RESUBSCRIBE_QUIET: Duration = Duration::from_secs(2);
/// Pacing for such a room: starts here, doubles per quick re-lag, caps below.
const LAG_RESUBSCRIBE_BACKOFF_START: Duration = Duration::from_millis(250);
const LAG_RESUBSCRIBE_BACKOFF_MAX: Duration = Duration::from_secs(8);

/// PURE: how long a lagged room waits before its next re-subscription. Zero
/// unless it lagged again within [`LAG_RESUBSCRIBE_QUIET`] of its last one;
/// then a backoff that doubles with each quick re-lag, capped.
fn lag_resubscribe_delay(lagged: u32, since_last: Option<Duration>) -> Duration {
    match since_last {
        Some(since) if since < LAG_RESUBSCRIBE_QUIET => {
            let step = LAG_RESUBSCRIBE_BACKOFF_START * 2u32.saturating_pow(lagged.min(6));
            step.min(LAG_RESUBSCRIBE_BACKOFF_MAX)
        }
        _ => Duration::ZERO,
    }
}

/// One room of the set: where a re-subscription resumes, the router's
/// lag flag for its current stream, and the pacing state for lag
/// re-subscriptions.
struct RoomSub {
    resume: Option<Cursor>,
    lag: airc_bus::LagFlag,
    /// How many times this stream re-subscribed the room for lag.
    lagged: u32,
    /// When the room was last re-subscribed for lag.
    last_resubscribe: Option<Instant>,
    /// Not before this instant: a room that lags again right after a
    /// re-subscription (its resume replay overflowed the same slow
    /// client) waits, with capped backoff, instead of replaying in a
    /// tight loop. Siblings and the client's own reads are unaffected.
    not_before: Option<Instant>,
    /// The tip of this room's attach page: a live durable at or before it was
    /// already written in the page and is not written again.
    seen_through: Option<Cursor>,
}
/// Re-subscribe every room that dropped a live push, from its own resume
/// point, pacing a room that lags again right after its last resume.
fn resubscribe_lagged(
    state: &DaemonState,
    filter_for: &dyn Fn(airc_core::RoomId) -> Filter,
    subs: &mut std::collections::HashMap<airc_core::RoomId, RoomSub>,
    merged: &mut tokio_stream::StreamMap<
        airc_core::RoomId,
        futures::stream::BoxStream<'static, Arc<Envelope>>,
    >,
) {
    // Only a room that dropped a live push is re-subscribed,
    // from its own resume point; its siblings keep their
    // streams and everything queued on them. A room that lags
    // again within LAG_RESUBSCRIBE_QUIET of its last
    // re-subscription is paced: the resume replay would just
    // overflow the same slow client again, and an unpaced loop
    // is a deep replay from the sink per lag.
    let now = Instant::now();
    for (room, sub) in subs.iter_mut() {
        if !sub.lag.is_lagged() {
            continue;
        }
        if sub.not_before.is_some_and(|t| now < t) {
            continue;
        }
        let delay = lag_resubscribe_delay(
            sub.lagged,
            sub.last_resubscribe.map(|t| now.duration_since(t)),
        );
        if !delay.is_zero() && sub.not_before.is_none() {
            // First sight of a quick re-lag: arm the pause and say so;
            // the re-subscription happens when the pause has passed.
            // The stale stream is REMOVED now, not left polled: the
            // router flags a lagged subscriber but does not fence it, so
            // a push after the dropped one can still land, and a resume
            // point advanced past the gap would omit the dropped event
            // forever (Astra's review of #1524). Dropping the stream
            // unsubscribes it; the room is silent until the paced
            // re-subscription replays from the frozen resume point.
            merged.remove(room);
            sub.not_before = Some(now + delay);
            StderrJsonDiagnosticSink.emit(
                DiagnosticEvent::warn(
                    DiagnosticComponent::Daemon,
                    DiagnosticCode::AttachSetRoomLagged,
                    "channel-set room lagged again right after its resume; pacing the next re-subscription",
                )
                .with_field("room", room.to_string())
                .with_field("lagged", sub.lagged)
                .with_field("delay_ms", delay.as_millis() as u64),
            );
            continue;
        }
        let (stream, lag) = state
            .router
            .subscribe_with_lag(filter_for(*room), sub.resume);
        merged.insert(*room, stream.boxed());
        sub.lag = lag;
        sub.lagged = sub.lagged.saturating_add(1);
        sub.last_resubscribe = Some(now);
        sub.not_before = None;
        StderrJsonDiagnosticSink.emit(
            DiagnosticEvent::warn(
                DiagnosticComponent::Daemon,
                DiagnosticCode::AttachSetRoomLagged,
                "channel-set room dropped a live push; re-subscribed from its resume point",
            )
            .with_field("room", room.to_string())
            .with_field("lagged", sub.lagged),
        );
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
    if parts.cursor_heartbeat {
        return write_response(
            &mut writer,
            &refuse("attach: cursor_heartbeat is single-channel only"),
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

    // Register every room BEFORE the ack — the same subscribe-before-ack
    // contract as the single channel: once the client sees `Ok`, no
    // room has a gap between ack and registration. A room named twice
    // gets one subscription, the first entry's start.
    let mut subs: std::collections::HashMap<airc_core::RoomId, RoomSub> = Default::default();
    // Rooms whose start is in the past: each gets its page after the ack.
    let mut pages: Vec<(airc_core::RoomId, Option<Cursor>)> = Vec::new();
    let mut merged: tokio_stream::StreamMap<
        airc_core::RoomId,
        futures::stream::BoxStream<'static, Arc<Envelope>>,
    > = tokio_stream::StreamMap::new();
    for entry in entries {
        if subs.contains_key(&entry.channel) {
            continue;
        }
        let room = entry.channel;
        // Every room registers at the live edge, whatever its start: a start
        // in the past earns one page after the ack, never a replay (the
        // single-channel rule). The resume point is the head read BEFORE
        // registering, so a lag before the first delivery replays exactly
        // what was dropped. The ring is RAM: after a daemon restart it is
        // empty while the durable transcript is not, and a `None` baseline
        // would replay that whole history on the first lag (Astra's
        // re-review of #1523); the router's durable fallback answers it.
        if let Some(bookmark) = past_start_of(entry.start()) {
            pages.push((room, bookmark));
        }
        let edge = match state.router.head_cursor(room) {
            Some(c) => Some(c),
            None => state.router.sink_head_cursor(room).await,
        };
        let (stream, lag) = state.router.subscribe_live_with_lag(filter_for(room));
        let (stream, lag, resume) = (stream.boxed(), lag, edge);
        merged.insert(room, stream);
        subs.insert(
            room,
            RoomSub {
                resume,
                lag,
                lagged: 0,
                last_resubscribe: None,
                not_before: None,
                seen_through: None,
            },
        );
    }
    write_response(&mut writer, &Response::Ok).await?;

    // One page per past-start room, each followed by a summary that names
    // its room. The room's resume point moves to its page's tip, so a later
    // lag re-reads from there, never from the old bookmark.
    for (room, bookmark) in pages {
        let page = attach_page(&state, room, &filter_for(room), bookmark)
            .await
            .map_err(|error| DaemonError::Io(std::io::Error::other(error)))?;
        for env in &page.events {
            write_event_response(&mut writer, env, &state.shared_frames).await?;
        }
        if let Some(summary) = page.summary(false, Some(room)) {
            write_response(&mut writer, &summary).await?;
        }
        if let (Some(through), Some(sub)) = (page.through, subs.get_mut(&room)) {
            sub.resume = Some(through);
            sub.seen_through = Some(through);
        }
    }

    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);
    let hangup = client_hung_up(reader);
    tokio::pin!(hangup);

    loop {
        // A paced room's pause ends on a timer, not only on the next event:
        // a quiet set must not wait for unrelated traffic to catch a room up.
        let pause_ends = subs.values().filter_map(|s| s.not_before).min();
        let pause = async {
            match pause_ends {
                Some(t) => tokio::time::sleep_until(tokio::time::Instant::from_std(t)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            biased;
            _ = &mut shutdown => return Ok(()),
            _ = &mut hangup => return Ok(()),
            _ = pause => {
                resubscribe_lagged(&state, &filter_for, &mut subs, &mut merged);
            }
            // An EMPTY map (every room paused) yields `None` at once; that is
            // "nothing to poll until the pause ends", not the router ending.
            next = async {
                if merged.is_empty() {
                    std::future::pending::<()>().await;
                }
                merged.next().await
            } => match next {
                Some((room, env)) => {
                    let already_paged = subs.get(&room).and_then(|sub| sub.seen_through).is_some_and(
                        |through| env.delivery.is_durable() && !env.cursor().is_after(&through),
                    );
                    if already_paged {
                        continue; // the room's page already carried it
                    }
                    if let Some(sub) = subs.get_mut(&room) {
                        sub.resume = Some(env.cursor());
                    }
                    tokio::select! {
                        _ = &mut shutdown => return Ok(()),
                        _ = &mut hangup => return Ok(()),
                        result = write_event_response(&mut writer, &env, &state.shared_frames) => result?,
                    }
                    resubscribe_lagged(&state, &filter_for, &mut subs, &mut merged);
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

/// The most history one attach streams: its newest page. Every start in the
/// past, a bookmark or the transcript start, gets at most this many events,
/// whatever the request asks (Joel, 2026-10-06: replaying more down a stream
/// "should be impossible"). Older history is paged backward on demand with
/// `Inbox { before }`.
pub(crate) const ATTACH_PAGE: usize = 10;

/// How many unread events beyond its page an attach counts before it reports
/// "at least this many". Counting is a bounded store read, never a stream.
const UNREAD_COUNT_CAP: u64 = 10_000;

/// Durable rows read per backward step of the page-and-count walk.
const PAGE_SCAN_STEP: usize = 256;

/// `None` = the live edge only; `Some(None)` = the transcript start;
/// `Some(Some(c))` = a bookmark. The last two are both "a start in the past"
/// and are served the same way: one page, then live.
fn past_start_of(start: AttachStart) -> Option<Option<Cursor>> {
    match start {
        AttachStart::Live => None,
        AttachStart::FromTranscriptStart => Some(None),
        AttachStart::After(c) => Some(Some(Cursor::new(Seq::new(c.epoch, c.counter), c.event_id))),
    }
}

/// One attach's page: the newest events the consumer's filter admits, after
/// its bookmark, oldest first; how many more it left out; and the tip the
/// page was read at, so the live stream never writes an event twice.
struct AttachPage {
    events: Vec<Arc<Envelope>>,
    skipped: u64,
    through: Option<Cursor>,
}

impl AttachPage {
    /// The summary frame: written whenever the page left something out (the
    /// consumer must know there is more to page back), or when the consumer
    /// persists cursors and the page delivered anything. `channel` names the
    /// room on a set attach, where one stream carries several.
    fn summary(&self, heartbeat: bool, channel: Option<airc_core::RoomId>) -> Option<Response> {
        let through = self.through?;
        (self.skipped > 0 || (heartbeat && !self.events.is_empty())).then_some(
            Response::AttachCursorAdvanced {
                skipped: self.skipped,
                advanced_to: airc_ipc::request::IpcCursor {
                    epoch: through.seq.epoch,
                    counter: through.seq.counter,
                    event_id: through.event_id,
                },
                channel,
            },
        )
    }
}

/// Read one attach's page BACKWARD from the tip: the newest `ATTACH_PAGE`
/// durable events after `bookmark` that `filter` admits, and a count of the
/// older ones it left out (capped at `UNREAD_COUNT_CAP`). Work is bounded by
/// the count cap, never by how deep the room is or how old the bookmark.
async fn attach_page(
    state: &DaemonState,
    channel: airc_core::RoomId,
    filter: &Filter,
    bookmark: Option<Cursor>,
) -> Result<AttachPage, String> {
    let mut newest_first: Vec<Arc<Envelope>> = Vec::new();
    let mut skipped: u64 = 0;
    let mut through: Option<Cursor> = None;
    let mut before: Option<Cursor> = None;
    'walk: loop {
        let rows = state
            .router
            .durable_tail_before(channel, before, PAGE_SCAN_STEP)
            .await
            .map_err(|error| format!("attach page: {error}"))?;
        let exhausted = rows.len() < PAGE_SCAN_STEP;
        for env in rows.iter().rev() {
            let cursor = env.cursor();
            through.get_or_insert(cursor);
            if bookmark.is_some_and(|b| !cursor.is_after(&b)) {
                break 'walk; // reached what the consumer had already read
            }
            if !filter.matches(env) {
                continue;
            }
            if newest_first.len() < ATTACH_PAGE {
                newest_first.push(env.clone());
            } else {
                skipped += 1;
                if skipped >= UNREAD_COUNT_CAP {
                    break 'walk;
                }
            }
        }
        match rows.first() {
            Some(oldest) if !exhausted => before = Some(oldest.cursor()),
            _ => break,
        }
    }
    newest_first.reverse();
    Ok(AttachPage {
        events: newest_first,
        skipped,
        through,
    })
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

        // Durable lines, so a resume can replay what a lag dropped. The
        // write-behind sink is bounded too: when it reports saturation the
        // publish yields and retries, which is backpressure, not a failure.
        async fn publish_durable(state: &DaemonState, env: Envelope) {
            loop {
                match state.router.publish(env.clone()).await {
                    Ok(_) => return,
                    Err(airc_bus::BusError::WriteBehindSaturated) => tokio::task::yield_now().await,
                    Err(e) => panic!("publish failed: {e:?}"),
                }
            }
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

        // what this catches (2026-10-04, Kimi on the 5090): a persona's channel-set
        // attach (40 rooms) received durable events only when she authored them;
        // other nodes' durable events, which enter through the inbound bridge's
        // publish_if_new_from, never reached her live stream. A live set subscriber
        // must receive a durable event from another peer arriving by that path, in
        // a set as large as hers.
        #[tokio::test]
        async fn a_live_channel_set_receives_another_nodes_durable_event_via_the_bridge_path() {
            let state = state().await;
            let rooms: Vec<RoomId> = (0..40).map(|_| RoomId::new()).collect();
            let target = rooms[37];
            let (mut client, daemon) = tokio::io::duplex(64 * 1024);
            let (reader, writer) = tokio::io::split(daemon);
            let request = AttachRequest::channel_set(
                rooms
                    .iter()
                    .map(|room| ChannelAttach {
                        channel: *room,
                        from: None,
                    })
                    .collect(),
            );
            let task = tokio::spawn(stream_attach(reader, writer, state.clone(), request));
            assert!(matches!(
                read_frame::<_, Response>(&mut client).await.unwrap(),
                Some(Response::Ok)
            ));
            let remote_link = PeerId::new();
            let outcome = state
                .router
                .publish_if_new_from(chat(target, "from-another-node"), Some(remote_link))
                .await
                .unwrap();
            assert!(
                matches!(outcome, airc_bus::PublishIfNew::Published(_)),
                "{outcome:?}"
            );
            let got = next_event(&mut client).await;
            assert_eq!(got.channel, target);
            assert_eq!(&got.payload[..], b"from-another-node");
            drop(client);
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
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

        // what this catches (Astra's re-review of #1523): the ring is RAM, so a
        // daemon that restarted over a durable transcript has an EMPTY ring and a
        // full store. A room's pre-first-delivery resume point read from the ring
        // alone is None, and a lag on that room before its first delivery then
        // replays the room's whole history into a live subscriber. The shape that
        // exercises the baseline is two rooms: A delivers and drives the loop, B
        // overflows before delivering anything, so B is re-subscribed from its
        // BASELINE, not from a delivered cursor. (A one-room version cannot tell
        // the baselines apart: a room's resume point is set from its first event
        // before any lag is judged. Mutation-checked: the ring-only baseline
        // fails this test.)
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_cold_ring_over_a_durable_transcript_never_replays_history_on_a_first_lag() {
            let home = tempfile::tempdir().unwrap();
            let store = Arc::new(InMemoryEventStore::new());
            let build = |home: std::path::PathBuf, store: Arc<InMemoryEventStore>| async move {
                DaemonState::build(
                    PeerId::new(),
                    PeerKeypair::generate(),
                    Arc::new(PeerKeyRegistry::new()),
                    VerificationPolicy::Strict,
                    home.clone(),
                    &home.join("events.sqlite"),
                    store,
                    crate::DaemonRuntimeInfo::unknown(),
                )
                .await
                .unwrap()
            };
            let a = RoomId::new();
            let b = RoomId::new();
            // The first daemon generation writes B's durable history through its router.
            let warm = Arc::new(build(home.path().to_owned(), store.clone()).await);
            for i in 0..20 {
                let text: &'static str = Box::leak(format!("b-old-{i:02}").into_boxed_str());
                publish_durable(&warm, chat(b, text)).await;
            }
            // The write-behind must have landed in the store before the "restart",
            // or the second generation has no durable history and the test proves nothing.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            while warm.router.sink_head_cursor(b).await.is_none() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "the durable sink never received the history"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            drop(warm);
            // The second generation: same store, cold ring, durable history present.
            let state = Arc::new(build(home.path().to_owned(), store).await);
            assert!(
                state.router.head_cursor(b).is_none(),
                "the ring is cold after a restart"
            );
            assert!(
                state.router.sink_head_cursor(b).await.is_some(),
                "the durable transcript survives the restart"
            );

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
            // The writer is held (nobody reads). A gets a few lines; B overflows its
            // 1024-slot buffer before any of its lines could be written. When the
            // client drains, A's lines drive the loop, B is seen lagged with nothing
            // delivered, and B is re-subscribed from its baseline.
            // A's lines are bigger than the 1 KiB pipe, so the writer blocks on A's
            // FIRST frame and no B frame is written before B overflows; otherwise B's
            // resume point would come from a delivered line and the baseline would be
            // unused (the one-room version of this test passed its own mutation).
            for i in 0..3 {
                let text: &'static str =
                    Box::leak(format!("a-{i}-{}", "x".repeat(700)).into_boxed_str());
                publish_durable(&state, chat(a, text)).await;
            }
            let b_new = 1300usize;
            for i in 0..b_new {
                let text: &'static str = Box::leak(format!("b-new-{i:04}").into_boxed_str());
                publish_durable(&state, chat(b, text)).await;
            }
            publish_durable(&state, chat(b, "b-final")).await;

            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let mut b_seen: Vec<String> = Vec::new();
            loop {
                assert!(
                    std::time::Instant::now() < deadline,
                    "b-final never arrived"
                );
                let env = next_event(&mut client).await;
                if env.channel != b {
                    continue;
                }
                let text = String::from_utf8(env.payload.to_vec()).unwrap();
                assert!(
                    !text.starts_with("b-old-"),
                    "history from before the attach was replayed into a live subscriber: {text}"
                );
                b_seen.push(text);
                if b_seen.last().is_some_and(|t| t == "b-final") {
                    break;
                }
            }
            let mut dedup = b_seen.clone();
            dedup.sort();
            dedup.dedup();
            assert_eq!(
                dedup.len(),
                b_seen.len(),
                "B never repeats a line across its resume"
            );
            assert_eq!(
                b_seen.len(),
                b_new + 1,
                "every post-attach B line, gap-free across the resume"
            );
            drop(client);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }

        // what this catches (IntelMac 2026-10-04, mixed-state daemon at ~2 cores):
        // a slow set consumer that lags again right after its resume replay must
        // not be re-subscribed in a tight loop (each re-subscription is a deep
        // replay from the sink). The first lag, and any lag after a quiet
        // window, resumes at once; a quick re-lag waits, doubling per re-lag,
        // capped.
        #[test]
        fn a_quick_re_lag_is_paced_and_a_quiet_one_is_not() {
            assert_eq!(
                lag_resubscribe_delay(0, None),
                Duration::ZERO,
                "first lag: resume now"
            );
            assert_eq!(
                lag_resubscribe_delay(3, Some(Duration::from_secs(30))),
                Duration::ZERO,
                "a lag after a quiet window resumes now"
            );
            assert_eq!(
                lag_resubscribe_delay(0, Some(Duration::from_millis(100))),
                Duration::from_millis(250)
            );
            assert_eq!(
                lag_resubscribe_delay(1, Some(Duration::from_millis(100))),
                Duration::from_millis(500)
            );
            assert_eq!(
                lag_resubscribe_delay(3, Some(Duration::from_millis(100))),
                Duration::from_secs(2)
            );
            assert_eq!(
                lag_resubscribe_delay(40, Some(Duration::from_millis(100))),
                LAG_RESUBSCRIBE_BACKOFF_MAX,
                "capped, and no overflow at a high count"
            );
        }

        // what this catches (Astra's review of #1524): the router flags a lagged
        // subscriber but does not fence it, so while a paced room waits, its
        // stale stream can still deliver a push from AFTER the dropped one; a
        // resume point advanced past the gap then omits the dropped line for
        // good. The paced room's stream is removed when the pause is armed and
        // its resume point frozen. Here: the writer is held, A overflows, its
        // immediate resume replay overflows again (a quick re-lag, so a pause),
        // more lines land during the pause, then the client drains everything:
        // every line published arrives exactly once.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn a_paced_room_replays_the_gap_it_dropped_and_never_skips_it() {
            let state = state().await;
            let a = RoomId::new();
            let (mut client, daemon) = tokio::io::duplex(1024);
            let (reader, writer) = tokio::io::split(daemon);
            let request = AttachRequest::channel_set(vec![ChannelAttach {
                channel: a,
                from: None,
            }]);
            let task = tokio::spawn(stream_attach(reader, writer, state.clone(), request));
            assert!(matches!(
                read_frame::<_, Response>(&mut client).await.unwrap(),
                Some(Response::Ok)
            ));
            // Three buffers' worth while the writer is held: the first lag's
            // resume replay overflows again, which is the quick re-lag.
            let total = 3200usize;
            for i in 0..total {
                let text: &'static str = Box::leak(format!("a-{i:05}").into_boxed_str());
                publish_durable(&state, chat(a, text)).await;
            }
            // Let the write-behind land every line so a deep replay can find them.
            let deadline = std::time::Instant::now() + Duration::from_secs(5);
            loop {
                let tip = state.router.sink_head_cursor(a).await;
                let ring = state.router.head_cursor(a);
                if tip.is_some() && tip == ring {
                    break;
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "the sink never caught up"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            // The client drains; the daemon lags, resumes, re-lags, pauses, resumes.
            let mut seen: Vec<String> = Vec::new();
            let deadline = std::time::Instant::now() + Duration::from_secs(20);
            while seen.len() < total && std::time::Instant::now() < deadline {
                match tokio::time::timeout(
                    Duration::from_secs(3),
                    read_frame::<_, Response>(&mut client),
                )
                .await
                {
                    Ok(Ok(Some(Response::Event { envelope }))) => {
                        let env = airc_wire::decode(bytes::Bytes::from(envelope)).unwrap();
                        seen.push(String::from_utf8(env.payload.to_vec()).unwrap());
                    }
                    Ok(Ok(Some(_))) => {}
                    other => panic!("stream ended early after {} lines: {other:?}", seen.len()),
                }
            }
            let mut dedup = seen.clone();
            dedup.sort();
            dedup.dedup();
            assert_eq!(dedup.len(), seen.len(), "a line was delivered twice");
            assert_eq!(
                seen.len(),
                total,
                "a dropped line was never replayed (a gap)"
            );
            drop(client);
            tokio::time::timeout(Duration::from_secs(2), task)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
        }

        // what this catches (Astra's review of #1524, the fence itself): when a
        // pause is armed for a quick re-lag, the room's stale stream must be OUT
        // of the merge and its resume point frozen. The router flags a lagged
        // subscriber but does not fence it, so a stream left in the map could
        // deliver a push from after the gap and advance the resume point past
        // the dropped line. Pinned at the function, because the timing of a
        // live reproduction is a race no test should depend on.
        #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
        async fn arming_a_pause_removes_the_rooms_stream_and_freezes_its_resume_point() {
            let state = state().await;
            let room = RoomId::new();
            let filter_for = |room: RoomId| Filter::channel(room);
            // A subscription nobody polls: 1100 pushes overflow its 1024 slots,
            // which sets its lag flag the way a slow client does.
            let (stream, lag) = state.router.subscribe_live_with_lag(filter_for(room));
            let mut merged: tokio_stream::StreamMap<
                RoomId,
                futures::stream::BoxStream<'static, Arc<Envelope>>,
            > = tokio_stream::StreamMap::new();
            merged.insert(room, stream.boxed());
            for i in 0..1100 {
                let text: &'static str = Box::leak(format!("p-{i:04}").into_boxed_str());
                publish_durable(&state, chat(room, text)).await;
            }
            assert!(
                lag.is_lagged(),
                "the unpolled subscription must have lagged"
            );
            let frozen = state.router.head_cursor(room);
            let mut subs = std::collections::HashMap::new();
            subs.insert(
                room,
                RoomSub {
                    resume: frozen,
                    lag,
                    lagged: 1,
                    last_resubscribe: Some(Instant::now()), // a quick re-lag
                    not_before: None,
                    seen_through: None,
                },
            );
            resubscribe_lagged(&state, &filter_for, &mut subs, &mut merged);
            let sub = &subs[&room];
            assert!(sub.not_before.is_some(), "a quick re-lag arms a pause");
            assert!(
                !merged.contains_key(&room),
                "the stale stream is fenced out of the merge"
            );
            assert_eq!(
                sub.resume, frozen,
                "the resume point is frozen through the pause"
            );
            assert_eq!(sub.lagged, 1, "no re-subscription happened yet");

            // A lag after a quiet window is not paced: it re-subscribes at once.
            let (stream, lag) = state.router.subscribe_live_with_lag(filter_for(room));
            merged.insert(room, stream.boxed());
            for i in 0..1100 {
                let text: &'static str = Box::leak(format!("q-{i:04}").into_boxed_str());
                publish_durable(&state, chat(room, text)).await;
            }
            assert!(lag.is_lagged());
            subs.insert(
                room,
                RoomSub {
                    resume: frozen,
                    lag,
                    lagged: 1,
                    last_resubscribe: Some(Instant::now() - Duration::from_secs(30)),
                    not_before: None,
                    seen_through: None,
                },
            );
            resubscribe_lagged(&state, &filter_for, &mut subs, &mut merged);
            let sub = &subs[&room];
            assert!(sub.not_before.is_none());
            assert!(merged.contains_key(&room), "re-subscribed in place");
            assert_eq!(sub.lagged, 2);
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
                // `coalesce_backlog` on a set is no longer refused: every
                // past start is served as one page, so the flag is implied.
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
