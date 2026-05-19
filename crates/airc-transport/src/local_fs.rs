//! `LocalFsAdapter` — same-machine multi-process wire backed by an
//! append-only JSONL log on the filesystem.
//!
//! Use case: AI peers (Claude Code, Codex, vHSM sessions, personas)
//! co-located on one Mac that need to talk without burning network
//! budget. A "wire" is a directory; every peer points its adapter at
//! the same directory; one process appends, all subscribed processes
//! see. This is the path that directly retires gh-polling for
//! multi-agent same-Mac chat.
//!
//! Layout: `<wire-dir>/frames.jsonl` — one JSON-encoded `Frame` per
//! line. Writers acquire an exclusive `flock(2)` on the file via
//! `fs2::FileExt::lock_exclusive` before each append, so multi-
//! process writers are serialized at the kernel level regardless of
//! frame size. The flock is released on file drop at the end of each
//! send. Readers poll the file by byte-offset and parse new lines.
//!
//! **Delivery semantics per `FrameKind`** (per the `Transport` trait
//! contract):
//! - `Message` / `Control` — durable. fsync'd before `send` returns.
//!   Receive side applies backpressure (slow consumer slows the
//!   wire); no frames are lost.
//! - `Event` — interrupt-style. Written to disk but NOT fsync'd; the
//!   bytes are visible to same-machine readers immediately, just not
//!   guaranteed to survive a kernel crash. Receive side is lossy
//!   past the per-subscriber buffer (slow consumer drops events,
//!   wire keeps moving). Codex turn/interrupt, turn/steer, presence
//!   transitions, and typing indicators ride on this kind.
//!
//! Constraints (called out so PR-3 can harden them):
//! - **Polling**: subscribers poll every 50 ms. Fine for the chat
//!   cadence; replace with `notify`/`inotify`/`kqueue` push-driven
//!   wake in a follow-up if latency matters.
//! - **Cursor replay**: `from_cursor: Some(...)` scans the file from
//!   start to find the cursor. Linear-scan; fine for the typical
//!   transcript size. PR-3+ can add an offset index.
//! - **Send concurrency on flock**: writes are serialized across the
//!   whole machine while the flock is held. For typical AI-agent
//!   chat traffic this is fine (sub-millisecond critical section).
//!   If high-frequency event traffic ever starves out senders, the
//!   right fix is a separate `events.jsonl` (lossier, no flock) or
//!   per-frame spool/rename. For PR-2 a single flocked log is
//!   correct and simple.

use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::Duration;

use async_trait::async_trait;
use fs2::FileExt;
use futures::stream::Stream;
use tokio::io::{AsyncBufReadExt, AsyncSeekExt, BufReader};
use tokio::sync::mpsc;
use tokio::time::sleep;

use airc_core::TranscriptCursor;
use airc_protocol::{Frame, FrameKind, Subscription};

use crate::error::LocalFsError;
use crate::transport::{FrameStream, Transport};

const FRAMES_FILENAME: &str = "frames.jsonl";
const POLL_INTERVAL: Duration = Duration::from_millis(50);
const RECEIVER_CHANNEL_DEPTH: usize = 64;

/// Same-machine wire backed by `<wire-dir>/frames.jsonl`.
///
/// Cross-process send safety comes from `fs2::FileExt::lock_exclusive`
/// (advisory flock) on the frames file — no in-memory mutex needed,
/// flock handles both intra- and inter-process serialization.
pub struct LocalFsAdapter {
    wire_dir: PathBuf,
}

impl LocalFsAdapter {
    /// Open the wire at `wire_dir`. The directory is created on first
    /// `send` / `subscribe` if it doesn't already exist — the caller
    /// doesn't need to pre-create it.
    pub fn new(wire_dir: impl Into<PathBuf>) -> Self {
        Self {
            wire_dir: wire_dir.into(),
        }
    }

    /// The directory backing this wire. Useful for tests + diagnostics.
    pub fn wire_dir(&self) -> &Path {
        &self.wire_dir
    }

    /// Compute the frames-log path.
    fn frames_path(&self) -> PathBuf {
        self.wire_dir.join(FRAMES_FILENAME)
    }

    /// Create the wire directory if absent. Called on every send +
    /// subscribe so the API doesn't have an "init" step.
    async fn ensure_wire_dir(&self) -> Result<(), LocalFsError> {
        tokio::fs::create_dir_all(&self.wire_dir).await?;
        Ok(())
    }
}

#[async_trait]
impl Transport for LocalFsAdapter {
    type Error = LocalFsError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        self.ensure_wire_dir().await?;

        // Serialize before crossing the spawn_blocking boundary so the
        // critical section (open + flock + write + maybe-fsync) is as
        // short as possible.
        let mut buffer = serde_json::to_vec(&frame)?;
        buffer.push(b'\n');
        let frame_kind = frame.kind;
        let path = self.frames_path();

        // std::fs + fs2 flock is sync; bracket the kernel calls in
        // spawn_blocking so we don't stall the async runtime. fs2
        // doesn't have a tokio-native equivalent and rolling our own
        // ioctl invocation would be more risk than this is worth.
        tokio::task::spawn_blocking(move || -> Result<(), LocalFsError> {
            let file = open_for_append_shared(&path)?;
            // Exclusive cross-process flock. Other writers (in this
            // process or any other) block here until we release. The
            // lock is released when `file` drops at end of block.
            file.lock_exclusive()?;
            let result = write_then_maybe_sync(&file, &buffer, frame_kind);
            // Explicit unlock so an Err from write/sync doesn't leak
            // the lock past the file's natural drop scope. (Drop
            // also unlocks on POSIX, but being explicit is clearer.)
            let _ = FileExt::unlock(&file);
            result
        })
        .await
        .map_err(|join_error| LocalFsError::Io(std::io::Error::other(join_error.to_string())))??;

        Ok(())
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        self.ensure_wire_dir().await?;
        let path = self.frames_path();
        let (tx, rx) = mpsc::channel(RECEIVER_CHANNEL_DEPTH);

        // Spawn the tail task. Owned by the returned stream's
        // lifetime: when the stream is dropped, `rx` is dropped,
        // `tx.send(...)` returns Err on the next iteration, and the
        // task exits.
        tokio::spawn(async move {
            if let Err(error) = tail_loop(path, subscription, tx.clone()).await {
                // Surface transport errors to the subscriber so they
                // can react, then close. We deliberately don't swallow.
                let _ = tx.send(Err(error)).await;
            }
        });

        // Wrap the mpsc receiver as a Stream without pulling
        // `tokio-stream` as a dep — futures::stream::unfold suffices.
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

/// Inside the spawn_blocking + flock critical section: append the
/// serialized frame, then fsync IFF the kind is durable. `Event`
/// frames skip the fsync so they don't pay the disk-flush latency
/// (interrupt-style delivery semantics, per the `Transport` trait
/// contract).
fn write_then_maybe_sync(
    mut file: &std::fs::File,
    buffer: &[u8],
    frame_kind: FrameKind,
) -> Result<(), LocalFsError> {
    use std::io::Write;

    file.write_all(buffer)?;
    match frame_kind {
        FrameKind::Message | FrameKind::Control => {
            // Durable kinds: fsync so a reader attaching right after
            // this returns observes the frame even past a kernel
            // crash. Substrate contract: Ok means durable.
            file.sync_data()?;
        }
        FrameKind::Event => {
            // Interrupt-style: skip fsync. The bytes are in the OS
            // page cache and visible to same-machine readers
            // immediately; survival across a kernel crash is not
            // required for event semantics.
        }
    }
    Ok(())
}

/// Open the frames file for append with permissive share modes so
/// concurrent readers (subscribers tailing the file) and other writers
/// can coexist. Unix opens are permissive by default; Windows defaults
/// to `FILE_SHARE_NONE` (exclusive), which would lock subscribers out
/// the moment a sender holds the handle for the flock + write critical
/// section — surfacing as ERROR_ACCESS_DENIED on the reader side and
/// breaking every multi-process test. Explicitly granting share rights
/// here restores cross-platform parity with the Unix open(2) defaults.
fn open_for_append_shared(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    // `read(true)` is required for fs2's `lock_exclusive` on Windows:
    // LockFileEx wants the handle to grant at least GENERIC_READ. A
    // pure-append handle (`FILE_APPEND_DATA` only) succeeds on Unix
    // flock but Windows surfaces ERROR_ACCESS_DENIED when we try to
    // take the cross-process lock. The handle still appends because
    // `append(true)` flips the seek-to-end behaviour; read access is
    // never used — it's a Windows ACL formality.
    options.read(true).create(true).append(true);
    apply_windows_share_mode(&mut options);
    options.open(path)
}

/// Async counterpart for the subscriber tail loop. Same share-mode
/// rationale as `open_for_append_shared`.
async fn open_for_read_shared(path: &Path) -> std::io::Result<tokio::fs::File> {
    let mut options = tokio::fs::OpenOptions::new();
    options.read(true);
    apply_windows_share_mode_async(&mut options);
    options.open(path).await
}

#[cfg(windows)]
fn apply_windows_share_mode(options: &mut std::fs::OpenOptions) {
    use std::os::windows::fs::OpenOptionsExt;
    // FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE. Matches
    // the permissive sharing Unix opens give by default. Without this
    // a concurrent reader or writer hits ERROR_ACCESS_DENIED.
    options.share_mode(0x1 | 0x2 | 0x4);
}

#[cfg(not(windows))]
fn apply_windows_share_mode(_options: &mut std::fs::OpenOptions) {
    // Unix opens are permissive by default; nothing to do.
}

#[cfg(windows)]
fn apply_windows_share_mode_async(options: &mut tokio::fs::OpenOptions) {
    // `share_mode` is an inherent method on tokio's `OpenOptions`
    // (not via the std extension trait), so no `use` needed here.
    options.share_mode(0x1 | 0x2 | 0x4);
}

#[cfg(not(windows))]
fn apply_windows_share_mode_async(_options: &mut tokio::fs::OpenOptions) {}

/// Background tail loop: poll the frames file, parse new lines, filter
/// against the subscription, and ship matches through `tx`.
async fn tail_loop(
    path: PathBuf,
    subscription: Subscription,
    tx: mpsc::Sender<Result<Frame, LocalFsError>>,
) -> Result<(), LocalFsError> {
    let mut offset: u64 = if subscription.from_cursor.is_some() {
        // Cursor-anchored: scan from start so we can replay.
        0
    } else {
        // Live-only: skip to end. Existing frames are not replayed.
        match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error.into()),
        }
    };

    loop {
        // Stop if the subscriber dropped the stream. Avoids waking
        // for the next poll just to discover the channel is closed.
        if tx.is_closed() {
            return Ok(());
        }

        let len = match tokio::fs::metadata(&path).await {
            Ok(metadata) => metadata.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // File deleted under us (logrotate, manual cleanup,
                // teardown). Forget our position so when a writer
                // recreates the file we read from the start.
                offset = 0;
                sleep(POLL_INTERVAL).await;
                continue;
            }
            Err(error) => return Err(error.into()),
        };

        if len < offset {
            // Truncation or recreation detected — file is shorter
            // than our last recorded position. Without this branch
            // we'd wait forever for the file to grow past the stale
            // offset, silently dropping every new frame.
            // (Codex's #668 review finding.)
            offset = 0;
        }

        if len > offset {
            offset = drain_new_lines(&path, offset, &subscription, &tx).await?;
        }

        sleep(POLL_INTERVAL).await;
    }
}

/// Read from `offset` to current EOF, dispatch matching frames, return
/// the new offset (positioned at the end of the last complete line —
/// partial trailing lines are NOT consumed so a concurrent appender's
/// in-flight write is re-read once it lands).
async fn drain_new_lines(
    path: &Path,
    mut offset: u64,
    subscription: &Subscription,
    tx: &mpsc::Sender<Result<Frame, LocalFsError>>,
) -> Result<u64, LocalFsError> {
    let mut file = open_for_read_shared(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    loop {
        line.clear();
        let read = reader.read_line(&mut line).await?;
        if read == 0 {
            // True EOF.
            break;
        }
        if !line.ends_with('\n') {
            // Partial line — a writer is mid-append. Don't advance
            // offset; we'll re-read on the next poll once the
            // newline arrives.
            break;
        }
        offset += read as u64;

        let trimmed = line.trim_end_matches('\n');
        if trimmed.is_empty() {
            continue;
        }

        let frame: Frame = serde_json::from_str(trimmed)?;

        if !subscription.matches(&frame) {
            continue;
        }
        if let Some(cursor) = &subscription.from_cursor {
            if !frame_after_cursor(&frame, cursor) {
                continue;
            }
        }

        // Delivery dispatch per FrameKind (per Codex's lag-policy
        // feedback — events must not stall behind transcript catch-
        // up; messages must not be silently dropped).
        let dispatch = match frame.kind {
            FrameKind::Message | FrameKind::Control => {
                // Durable kinds: apply backpressure. If the
                // subscriber is slow, this await blocks the tail
                // loop until they catch up. No frames lost.
                tx.send(Ok(frame)).await.map_err(|_| DispatchExit::Closed)
            }
            FrameKind::Event => {
                // Interrupt-style: lossy. Drop on full buffer;
                // surface closed-subscriber as the exit signal.
                match tx.try_send(Ok(frame)) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        // Subscriber backlog full — event dropped.
                        // The wire keeps moving, which is the
                        // documented semantic. Could log a counter
                        // here in a future patch.
                        Ok(())
                    }
                    Err(mpsc::error::TrySendError::Closed(_)) => Err(DispatchExit::Closed),
                }
            }
        };

        if dispatch.is_err() {
            // Subscriber dropped. Exit the spawned task cleanly.
            // We've already advanced `offset` past this line, which
            // is fine — there's no resumability after drop.
            return Ok(offset);
        }
    }
    Ok(offset)
}

/// Internal: signal that the subscriber's stream is closed, so the
/// tail-loop should exit. Not exposed — just an internal control flow
/// type so the dispatch match is exhaustive without conflating with
/// `LocalFsError`.
enum DispatchExit {
    Closed,
}

/// Is `frame` strictly after `cursor` in transcript order?
/// Lamport first, event_id as tiebreaker — matches airc-core's
/// transcript ordering rule.
fn frame_after_cursor(frame: &Frame, cursor: &TranscriptCursor) -> bool {
    let envelope = &frame.envelope;
    envelope.lamport > cursor.lamport
        || (envelope.lamport == cursor.lamport
            && envelope.event_id.as_uuid() > cursor.event_id.as_uuid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{
        headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId, RoomId,
    };
    use airc_protocol::{ChannelId, Envelope, Frame, FrameKind, Signature, Subscription};
    use futures::stream::StreamExt;
    use tempfile::TempDir;

    fn frame_at(lamport: u64, channel: ChannelId, body: &str) -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id: EventId::from_u128(lamport as u128),
                sender: PeerId::from_u128(0xa1),
                sender_client: ClientId::from_u128(0xc1),
                channel,
                target: MentionTarget::All,
                lamport,
                occurred_at_ms: 1_700_000_000_000 + lamport,
                reply_to: None,
                headers: Headers::new(),
                body: Some(Body::text(body)),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    #[tokio::test]
    async fn send_then_subscribe_with_cursor_replays_frame() {
        // The "two AI agents share a Mac" base case. Agent A sends,
        // Agent B subscribes with from_cursor=None... but None means
        // live-only, so we use a from_cursor anchor to force replay.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        adapter
            .send(frame_at(1, channel, "hello from A"))
            .await
            .unwrap();

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();
        let received = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("must yield within 2s")
            .expect("stream must yield Some")
            .expect("frame must parse");

        assert_eq!(received.envelope.lamport, 1);
        let body_text = received
            .envelope
            .body
            .as_ref()
            .and_then(Body::as_text)
            .expect("body must be text");
        assert_eq!(body_text, "hello from A");
    }

    #[tokio::test]
    async fn live_subscription_skips_past_frames() {
        // from_cursor=None means "live only" — frames already on disk
        // when subscribe() is called are NOT replayed. Critical for
        // agents who reconnect and don't want to re-process history.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        adapter.send(frame_at(1, channel, "old")).await.unwrap();

        let sub = Subscription {
            channel: Some(channel),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();

        // Give the tail loop a poll cycle to seek to EOF.
        sleep(Duration::from_millis(100)).await;

        adapter.send(frame_at(2, channel, "new")).await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("must yield within 2s")
            .expect("stream must yield Some")
            .expect("frame must parse");
        assert_eq!(received.envelope.lamport, 2);
        assert_eq!(
            received
                .envelope
                .body
                .as_ref()
                .and_then(Body::as_text)
                .unwrap(),
            "new"
        );
    }

    #[tokio::test]
    async fn subscription_channel_filter_routes_correctly() {
        // Fan-out hot path: one wire carries multiple channels; each
        // subscriber gets only its channel's frames.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let chan_a = RoomId::from_u128(0xaaaa);
        let chan_b = RoomId::from_u128(0xbbbb);

        let sub = Subscription {
            channel: Some(chan_a),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();

        // Let the tail task start.
        sleep(Duration::from_millis(50)).await;

        adapter.send(frame_at(1, chan_a, "a-frame")).await.unwrap();
        adapter.send(frame_at(2, chan_b, "b-frame")).await.unwrap();
        adapter
            .send(frame_at(3, chan_a, "a-frame-2"))
            .await
            .unwrap();

        // Should receive only the chan_a frames.
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("first")
            .expect("some")
            .expect("ok");
        let second = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("second")
            .expect("some")
            .expect("ok");
        assert_eq!(first.envelope.lamport, 1);
        assert_eq!(second.envelope.lamport, 3);

        // No third frame on the chan_a stream within 200ms.
        let third = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
        assert!(third.is_err(), "no further frames expected");
    }

    #[tokio::test]
    async fn two_subscribers_both_receive() {
        // Fan-out parity: two independent subscribers on the same
        // channel both get every frame. The "Codex AND Claude" case.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        let make_sub = || Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };

        let mut stream_a = adapter.subscribe(make_sub()).await.unwrap();
        let mut stream_b = adapter.subscribe(make_sub()).await.unwrap();

        sleep(Duration::from_millis(50)).await;
        adapter
            .send(frame_at(1, channel, "broadcast"))
            .await
            .unwrap();

        let recv_a = tokio::time::timeout(Duration::from_secs(2), stream_a.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let recv_b = tokio::time::timeout(Duration::from_secs(2), stream_b.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(recv_a.envelope.lamport, 1);
        assert_eq!(recv_b.envelope.lamport, 1);
    }

    #[tokio::test]
    async fn events_are_lossy_when_subscriber_lags() {
        // Codex's lag-policy contract: Event frames must drop on a
        // full subscriber buffer rather than back up the wire. If
        // every event sent here arrives, the lossy semantic is
        // broken. Send WAY more events than the channel depth (64)
        // while NOT consuming, then drain — expect strictly fewer
        // than sent.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();

        // Let the tail loop arm. Then send a flood of Events
        // without reading from the stream.
        sleep(Duration::from_millis(50)).await;
        let total_sent: u64 = 500;
        for lamport in 1..=total_sent {
            let mut frame = frame_at(lamport, channel, &format!("evt-{lamport}"));
            frame.kind = FrameKind::Event;
            adapter.send(frame).await.unwrap();
        }

        // Give the tail loop time to drain the file into the bounded
        // subscriber channel — events will be dropped at try_send
        // when the channel is full.
        sleep(Duration::from_millis(300)).await;

        // Drain whatever made it through.
        let mut count: u64 = 0;
        while let Ok(Some(Ok(_))) =
            tokio::time::timeout(Duration::from_millis(50), stream.next()).await
        {
            count += 1;
        }
        assert!(
            count < total_sent,
            "events must be lossy when subscriber lags; received {count} of {total_sent}"
        );
        assert!(count > 0, "at least some events should arrive; got {count}");
    }

    #[tokio::test]
    async fn messages_are_not_dropped_under_subscriber_lag() {
        // The flip side: Message frames apply backpressure and MUST
        // be delivered in full. If a slow subscriber drops messages
        // we've broken the durability contract — the Codex bridge
        // (and every other consumer) needs to rely on this.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();

        sleep(Duration::from_millis(50)).await;
        let total_sent: u64 = 200;
        for lamport in 1..=total_sent {
            adapter
                .send(frame_at(lamport, channel, &format!("msg-{lamport}")))
                .await
                .unwrap();
        }

        // Drain — all must arrive (backpressure can slow it, but
        // can't lose). Generous timeout for the full drain since
        // backpressure may stall periodically.
        let mut received: u64 = 0;
        while received < total_sent {
            let item = tokio::time::timeout(Duration::from_secs(10), stream.next())
                .await
                .expect("must yield within 10s under backpressure")
                .expect("stream must yield Some")
                .expect("frame must parse");
            received += 1;
            // Sanity that order is preserved.
            assert_eq!(item.envelope.lamport, received);
        }
        assert_eq!(received, total_sent);
    }

    #[tokio::test]
    async fn truncation_recovers_offset_to_start() {
        // Codex's #668 review finding: if the log is truncated/
        // recreated under a live subscriber whose offset has
        // advanced past the new EOF, the subscriber must NOT stall.
        // The tail loop has to detect `len < offset`, reset to 0,
        // and continue reading.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        // Send a big-ish frame so the file has meaningful length.
        // Then attach live and read it via cursor replay; offset
        // advances past frame_1.
        let frame_1 = frame_at(1, channel, "before-truncation");
        adapter.send(frame_1.clone()).await.unwrap();

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();

        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.envelope.lamport, 1);

        // Now truncate the file. Subscriber's tail loop has its
        // offset positioned at the end of frame_1. Without the
        // truncation-detection branch, the subscriber would stall.
        {
            let mut options = tokio::fs::OpenOptions::new();
            options.write(true).truncate(true);
            apply_windows_share_mode_async(&mut options);
            options.open(adapter.frames_path()).await.unwrap();
        }

        // Give the tail loop one poll cycle to observe the shrunk
        // file and reset its offset.
        sleep(Duration::from_millis(100)).await;

        // Send a fresh frame post-truncation. Subscriber MUST see
        // it (the bug would have it stall here, waiting for the
        // file to grow past the old offset).
        let frame_2 = frame_at(2, channel, "after-truncation");
        adapter.send(frame_2.clone()).await.unwrap();

        let second = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("tail must recover from truncation within 2s")
            .expect("stream must yield Some")
            .expect("frame must parse");
        assert_eq!(second.envelope.lamport, 2);
        assert_eq!(
            second
                .envelope
                .body
                .as_ref()
                .and_then(Body::as_text)
                .unwrap(),
            "after-truncation"
        );
    }

    #[tokio::test]
    async fn deletion_then_recreation_recovers_offset() {
        // Companion to the truncation test: outright DELETE the
        // file and recreate via a fresh send. The tail loop's
        // NotFound branch must also reset offset so the recreated
        // file is read from the start.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        adapter
            .send(frame_at(1, channel, "before-delete"))
            .await
            .unwrap();

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(first.envelope.lamport, 1);

        // Delete the file entirely.
        tokio::fs::remove_file(adapter.frames_path()).await.unwrap();
        sleep(Duration::from_millis(100)).await;

        // Send recreates the file via OpenOptions::create(true).
        adapter
            .send(frame_at(2, channel, "after-delete"))
            .await
            .unwrap();

        let second = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .expect("tail must recover from deletion within 2s")
            .expect("stream must yield Some")
            .expect("frame must parse");
        assert_eq!(second.envelope.lamport, 2);
    }

    #[tokio::test]
    async fn flock_serializes_concurrent_in_process_senders() {
        // The flock contract: even with many concurrent senders, the
        // resulting frames.jsonl is well-formed (one frame per line,
        // no torn writes, all frames present). Tests within-process
        // concurrency — cross-process flock relies on the same
        // kernel mechanism so the in-process test is a proxy. (A
        // real cross-process test would spawn child processes,
        // which is out of scope for unit tests.)
        let dir = TempDir::new().unwrap();
        let adapter = std::sync::Arc::new(LocalFsAdapter::new(dir.path()));
        let channel = RoomId::from_u128(0xc0ffee);

        let mut handles = Vec::new();
        let frames_per_sender: u64 = 30;
        let sender_count: u64 = 8;
        for sender_idx in 0..sender_count {
            let adapter = adapter.clone();
            let handle = tokio::spawn(async move {
                for i in 0..frames_per_sender {
                    let lamport = sender_idx * frames_per_sender + i + 1;
                    adapter
                        .send(frame_at(
                            lamport,
                            channel,
                            &format!("sender-{sender_idx}-{i}"),
                        ))
                        .await
                        .unwrap();
                }
            });
            handles.push(handle);
        }
        for h in handles {
            h.await.unwrap();
        }

        // Read the resulting file: every line MUST parse, and the
        // total count MUST equal sender_count * frames_per_sender.
        let contents = tokio::fs::read_to_string(adapter.frames_path())
            .await
            .unwrap();
        let lines: Vec<_> = contents.lines().collect();
        let expected = (sender_count * frames_per_sender) as usize;
        assert_eq!(
            lines.len(),
            expected,
            "every concurrent send must land as exactly one line"
        );
        for line in lines {
            let _: Frame = serde_json::from_str(line).expect("every line must parse cleanly");
        }
    }

    #[tokio::test]
    async fn drop_stream_stops_tail_task() {
        // Resource discipline: dropping the returned stream must
        // shut down its background tail. Tested indirectly: the tail
        // task observes the closed channel and exits on its next
        // poll. We can't observe the task directly but we can assert
        // the second send doesn't panic / leak (smoke test).
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let stream = adapter.subscribe(sub).await.unwrap();
        drop(stream);

        // Sends after stream drop should still succeed against the
        // wire — the closed subscriber doesn't affect the writer.
        sleep(Duration::from_millis(100)).await;
        adapter
            .send(frame_at(1, channel, "no one home"))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_is_durable_when_returning_ok() {
        // The contract: send returning Ok means the frame is on disk
        // and readable. Pin that by reading the file directly after
        // a single send.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        adapter.send(frame_at(1, channel, "durable")).await.unwrap();

        let contents = tokio::fs::read_to_string(adapter.frames_path())
            .await
            .unwrap();
        // One frame, one line, ends with newline.
        assert_eq!(contents.lines().count(), 1);
        assert!(contents.ends_with('\n'));
        let parsed: Frame = serde_json::from_str(contents.trim_end()).unwrap();
        assert_eq!(parsed.envelope.lamport, 1);
    }

    #[tokio::test]
    async fn from_cursor_drops_frames_at_or_before_anchor() {
        // Replay semantics: from_cursor returns frames STRICTLY after
        // the anchor (not including the anchor frame itself), so a
        // reconnecting peer doesn't re-process the last-acked event.
        let dir = TempDir::new().unwrap();
        let adapter = LocalFsAdapter::new(dir.path());
        let channel = RoomId::from_u128(0xc0ffee);

        let frame1 = frame_at(1, channel, "first");
        let frame2 = frame_at(2, channel, "second");
        let frame3 = frame_at(3, channel, "third");

        adapter.send(frame1.clone()).await.unwrap();
        adapter.send(frame2.clone()).await.unwrap();
        adapter.send(frame3.clone()).await.unwrap();

        let sub = Subscription {
            channel: Some(channel),
            from_cursor: Some(TranscriptCursor {
                lamport: 1,
                event_id: frame1.envelope.event_id,
            }),
            ..Default::default()
        };
        let mut stream = adapter.subscribe(sub).await.unwrap();

        let recv_a = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let recv_b = tokio::time::timeout(Duration::from_secs(2), stream.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert_eq!(recv_a.envelope.lamport, 2);
        assert_eq!(recv_b.envelope.lamport, 3);

        // No further frames.
        let none = tokio::time::timeout(Duration::from_millis(200), stream.next()).await;
        assert!(none.is_err(), "should be no further frames");
    }
}
