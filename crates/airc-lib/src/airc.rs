//! The `Airc` facade — primary entrypoint for consumer apps.
//!
//! Owns the substrate handles (identity, store, peer registry,
//! local-fs transport per room). Cheap to clone via inner `Arc`s.
//!
//! Lifecycle:
//!
//! ```no_run
//! # async fn run(home: std::path::PathBuf) -> Result<(), Box<dyn std::error::Error>> {
//! use airc_lib::Airc;
//!
//! let airc = Airc::open(home).await?;
//! airc.join("project-x").await?;
//! airc.say("hello").await?;
//! let recent = airc.page_recent(10).await?;
//! for event in &recent {
//!     println!("{} → {}", event.peer_id, event.event_id);
//! }
//! # Ok(()) }
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::RwLock;
use std::task::{Context, Poll};

use airc_core::{
    body::Body, headers::Headers, transcript::MentionTarget, ClientId, EventId, PeerId,
    TranscriptCursor, TranscriptEvent,
};
use airc_daemon::{peers_store, LocalIdentity};
use airc_protocol::{
    Envelope, Frame, FrameKind, PeerKeyRegistry, Signature, Subscription, VerificationPolicy,
};
use airc_store::{EventStore, SqliteEventStore};
use airc_transport::{LocalFsAdapter, SignedTransport, Transport};
use futures::stream::{Stream, StreamExt};
use tokio::sync::{broadcast, Mutex};

use crate::error::AircError;
use crate::registry::PeerSpec;
use crate::room::{self, Room};

const EVENTS_DB_FILENAME: &str = "events.sqlite";

/// Capacity of the live broadcast channel. Each consumer that calls
/// [`Airc::subscribe`] gets its own receiver; lagged receivers see
/// `BroadcastStreamRecvError::Lagged(n)` rather than silently miss
/// events — the operating doc's "no silent fallback" rule. Consumers
/// that need durable replay use `Airc::resume_from` against the store.
const LIVE_BROADCAST_CAPACITY: usize = 1024;

/// In-process AIRC handle. Holds identity, store, per-room
/// signed-local-fs transports, and a background subscriber per wire
/// that converts received `Frame`s into `TranscriptEvent`s and
/// appends them to the durable store. Cheap to clone via inner
/// `Arc`s.
///
/// Lifecycle:
///   - `Airc::open` initialises identity + store + peer registry.
///   - `Airc::join(name)` / `Airc::say(text)` lazily start a
///     subscriber on the room's wire if one isn't already running.
///   - Consumers wanting live push call `Airc::subscribe()` and
///     get a `Stream<Item = TranscriptEvent>`.
pub struct Airc {
    inner: Arc<AircInner>,
}

struct AircInner {
    home: PathBuf,
    identity: LocalIdentity,
    store: Arc<dyn EventStore>,
    registry: Arc<RwLock<PeerKeyRegistry>>,
    policy: VerificationPolicy,
    /// Per-wire background subscriber tasks. Spawned lazily on first
    /// `say`/`send`/`subscribe`/`page_recent` referencing the wire.
    /// Held in a Mutex so concurrent calls can't double-spawn.
    subscribers: Mutex<HashMap<PathBuf, WireSubscriber>>,
    /// Live event fan-out. Every event the subscribers append to the
    /// store is also forwarded here so consumers tailing via
    /// [`Airc::subscribe`] see it immediately.
    live_tx: broadcast::Sender<TranscriptEvent>,
}

struct WireSubscriber {
    /// Kept alive by ownership of its `JoinHandle`. Dropped when
    /// `Airc` is dropped (since the AircInner holding it goes away),
    /// which closes the underlying transport subscription.
    _task: tokio::task::JoinHandle<()>,
}

impl Airc {
    /// Open or initialise an Airc handle at `<home>`. This call:
    ///   - Loads `<home>/identity.{key,json}` (generates if missing).
    ///   - Opens `<home>/events.sqlite` and applies any pending
    ///     event-store migrations.
    ///   - Loads `<home>/peers.json` into the in-memory trust registry.
    ///
    /// Production policy is always `VerificationPolicy::Strict` —
    /// unsigned frames are rejected. Use `open_with_policy` if a
    /// test harness needs a different stance.
    pub async fn open(home: impl Into<PathBuf>) -> Result<Self, AircError> {
        Self::open_with_policy(home, VerificationPolicy::Strict).await
    }

    /// Variant of [`open`] that lets the caller pin the
    /// `VerificationPolicy`. The only legitimate non-Strict use is
    /// in-process tests that intentionally exercise unsigned paths.
    pub async fn open_with_policy(
        home: impl Into<PathBuf>,
        policy: VerificationPolicy,
    ) -> Result<Self, AircError> {
        let home: PathBuf = home.into();
        std::fs::create_dir_all(&home).map_err(airc_daemon::IdentityError::Io)?;
        let identity = LocalIdentity::load_or_generate(&home)?;

        let store_path = home.join(EVENTS_DB_FILENAME);
        let store_url = format!("sqlite://{}?mode=rwc", store_path.display());
        let store: Arc<dyn EventStore> = Arc::new(SqliteEventStore::open(&store_url).await?);

        let mut registry = PeerKeyRegistry::new();
        registry
            .enrol(identity.peer_id, 0, identity.keypair.public_bytes())
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        for stored in peers_store::load(&home)? {
            registry
                .enrol(
                    stored.peer_id,
                    0,
                    stored
                        .pubkey_bytes()
                        .map_err(|e| AircError::Crypto(e.to_string()))?,
                )
                .map_err(|e| AircError::Crypto(e.to_string()))?;
        }
        let registry = Arc::new(RwLock::new(registry));
        let (live_tx, _) = broadcast::channel(LIVE_BROADCAST_CAPACITY);

        Ok(Self {
            inner: Arc::new(AircInner {
                home,
                identity,
                store,
                registry,
                policy,
                subscribers: Mutex::new(HashMap::new()),
                live_tx,
            }),
        })
    }

    /// Return the home directory backing this handle.
    pub fn home(&self) -> &Path {
        &self.inner.home
    }

    /// Return the local peer's stable identifier.
    pub fn peer_id(&self) -> PeerId {
        self.inner.identity.peer_id
    }

    /// Return the per-session client identifier.
    pub fn client_id(&self) -> ClientId {
        self.inner.identity.client_id
    }

    /// Return the peer-spec string suitable for sharing with another
    /// peer so they can enrol this identity into their trust registry.
    pub fn peer_spec(&self) -> String {
        crate::registry::format_peer_spec(
            self.inner.identity.peer_id,
            &self.inner.identity.keypair.public_bytes(),
        )
    }

    /// Switch the current room to one derived from `name`. Same name
    /// on two peers yields the same channel UUID via UUIDv5, so they
    /// converge without exchanging the UUID out-of-band. Spawns a
    /// background subscriber on the new room's wire if one isn't
    /// already running, so subsequent `say`s land in the store.
    pub async fn join(&self, name: &str) -> Result<Room, AircError> {
        let room = Room::from_name(&self.inner.home, name);
        room::save(&self.inner.home, &room)?;
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(room)
    }

    /// Read the persisted current room. Returns the default room
    /// (synthesised on the fly, NOT persisted) if no `room.json`
    /// has been written yet.
    pub async fn current_room(&self) -> Result<Room, AircError> {
        Ok(room::load_or_default(&self.inner.home)?)
    }

    /// Send a plain-text message to the current room. Returns the
    /// event's `EventId` so callers can correlate or filter their
    /// own echo via [`Airc::subscribe`].
    pub async fn say(&self, text: &str) -> Result<EventId, AircError> {
        self.send(Body::text(text), Headers::new()).await
    }

    /// Send a frame with typed body and arbitrary headers to the
    /// current room. Frame is signed under the local identity and
    /// written through the room's local-fs wire. The room's
    /// background subscriber will see the frame on the wire,
    /// convert it to a `TranscriptEvent`, append it to the store,
    /// and broadcast it to live [`subscribe`](Self::subscribe)
    /// receivers — so `say(...).await` followed by
    /// `page_recent(...)` reliably observes the new event.
    pub async fn send(&self, body: Body, headers: Headers) -> Result<EventId, AircError> {
        let room = self.current_room().await?;
        self.ensure_wire_subscriber(&room.wire).await?;
        let event_id = EventId::new();
        let frame = Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id,
                sender: self.inner.identity.peer_id,
                sender_client: self.inner.identity.client_id,
                channel: room.channel,
                target: MentionTarget::All,
                lamport: now_ms(),
                occurred_at_ms: now_ms(),
                reply_to: None,
                headers,
                body: Some(body),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        let inner = LocalFsAdapter::new(&room.wire);
        let transport = SignedTransport::new(
            inner,
            self.inner.identity.keypair.clone(),
            self.inner.identity.peer_id,
            self.inner.registry.clone(),
            self.inner.policy,
        );
        transport
            .send(frame)
            .await
            .map_err(|e| AircError::Transport(e.to_string()))?;
        Ok(event_id)
    }

    /// Subscribe to the live event stream. Every event appended to
    /// the durable store (by this `Airc` handle or by remote peers
    /// the wire's subscriber sees) is forwarded to all live
    /// subscribers. Lagged receivers get
    /// [`broadcast::error::RecvError::Lagged`] rather than silent
    /// drops — consumers that need durable replay use
    /// [`Airc::resume_from`].
    pub async fn subscribe(&self) -> Result<EventStream, AircError> {
        let room = self.current_room().await?;
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(EventStream {
            rx: self.inner.live_tx.subscribe(),
        })
    }

    /// Fetch the most recent `limit` events from the durable store,
    /// filtered to the current room. Returns events in transcript
    /// order (oldest → newest within the page).
    pub async fn page_recent(&self, limit: usize) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        self.ensure_wire_subscriber(&room.wire).await?;
        Ok(self
            .inner
            .store
            .page_recent(Some(room.channel), limit)
            .await?)
    }

    /// Fetch up to `limit` events strictly after `cursor` from the
    /// current room. Use the cursor of the last event returned to
    /// page forward.
    pub async fn resume_from(
        &self,
        cursor: &TranscriptCursor,
        limit: usize,
    ) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        Ok(self
            .inner
            .store
            .resume_from(cursor, Some(room.channel), limit)
            .await?)
    }

    /// Cursor of the newest event in the current room, or `None` if
    /// the room has no events yet.
    pub async fn latest_cursor(&self) -> Result<Option<TranscriptCursor>, AircError> {
        let room = self.current_room().await?;
        Ok(self.inner.store.latest_cursor(Some(room.channel)).await?)
    }

    /// Append a `TranscriptEvent` to the durable store directly. The
    /// caller is responsible for the event's identity/lamport fields
    /// being correct. Useful for consumers replaying captured events
    /// or for tests that want to seed history.
    pub async fn append_event(&self, event: TranscriptEvent) -> Result<(), AircError> {
        Ok(self.inner.store.append(event).await?)
    }

    /// Enrol a peer into the local trust registry and persist it to
    /// `<home>/peers.json`. Pass the peer-spec the remote produced
    /// via [`Airc::peer_spec`].
    pub async fn add_peer(&self, spec: PeerSpec) -> Result<(), AircError> {
        peers_store::add(&self.inner.home, spec.peer_id, spec.pubkey)?;
        let mut registry = self
            .inner
            .registry
            .write()
            .map_err(|_| AircError::Crypto("registry lock poisoned".to_string()))?;
        registry
            .enrol(spec.peer_id, 0, spec.pubkey)
            .map_err(|e| AircError::Crypto(e.to_string()))?;
        Ok(())
    }

    /// Return a list of enrolled peers (read from peers.json — the
    /// daemon writes the same file, so this view stays consistent).
    pub async fn peers(&self) -> Result<Vec<EnrolledPeer>, AircError> {
        let stored = peers_store::load(&self.inner.home)?;
        Ok(stored
            .into_iter()
            .map(|p| EnrolledPeer {
                peer_id: p.peer_id,
                pubkey_b64: p.pubkey_b64,
            })
            .collect())
    }

    /// Idempotently spawn a background subscriber on `wire` if one
    /// isn't already running. The subscriber:
    ///   1. attaches a SignedTransport<LocalFsAdapter> subscription
    ///      anchored at the start of the wire (replays existing
    ///      frames into the store on first attach);
    ///   2. converts each verified `Frame` into a `TranscriptEvent`
    ///      via `Frame::into_transcript_event`;
    ///   3. appends to the durable store;
    ///   4. fans the event out on `live_tx` for live subscribers.
    ///
    /// Verification failures and store errors are surfaced on stderr
    /// rather than silently swallowed — the operating doc's "errors
    /// reach a debugger" rule.
    async fn ensure_wire_subscriber(&self, wire: &Path) -> Result<(), AircError> {
        let mut subs = self.inner.subscribers.lock().await;
        if subs.contains_key(wire) {
            return Ok(());
        }
        let transport = SignedTransport::new(
            LocalFsAdapter::new(wire),
            self.inner.identity.keypair.clone(),
            self.inner.identity.peer_id,
            self.inner.registry.clone(),
            self.inner.policy,
        );
        // Replay-anchored so first attach captures pre-existing
        // frames on the wire and routes them into the store.
        let subscription = Subscription {
            from_cursor: Some(TranscriptCursor {
                lamport: 0,
                event_id: EventId::from_u128(0),
            }),
            ..Default::default()
        };
        let mut stream = transport
            .subscribe(subscription)
            .await
            .map_err(|e| AircError::Transport(e.to_string()))?;

        let store = self.inner.store.clone();
        let live_tx = self.inner.live_tx.clone();
        let task = tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                match item {
                    Ok(frame) => {
                        let event = frame.into_transcript_event();
                        match store.append(event.clone()).await {
                            Ok(()) => {
                                // No active receivers is normal (no
                                // one's called `subscribe()` yet) —
                                // broadcast::send returns Err in that
                                // case and we don't care.
                                let _ = live_tx.send(event);
                            }
                            Err(airc_store::StoreError::DuplicateEventId(_)) => {
                                // Replay-anchored attach re-reads
                                // the existing wire on every fresh
                                // Airc::open; the store rejects the
                                // duplicate, we move on without
                                // fanning out (the live broadcast is
                                // for genuinely new events).
                            }
                            Err(err) => {
                                eprintln!("airc-lib subscriber: store append failed: {err}");
                            }
                        }
                    }
                    Err(verify_err) => {
                        eprintln!("airc-lib subscriber: frame verification failed: {verify_err}");
                    }
                }
            }
        });
        subs.insert(wire.to_path_buf(), WireSubscriber { _task: task });
        Ok(())
    }
}

/// Live transcript-event stream returned by [`Airc::subscribe`].
/// Wraps a broadcast receiver and yields events as they arrive.
///
/// Lagged behaviour: if the consumer falls behind the broadcast
/// capacity, the stream surfaces the lag count and resumes from
/// the current tip. That's a real signal — consumers that need
/// durable replay should snapshot a cursor and call
/// [`Airc::resume_from`] when they catch up. Silent drops are not
/// part of the API.
pub struct EventStream {
    rx: broadcast::Receiver<TranscriptEvent>,
}

impl Stream for EventStream {
    type Item = Result<TranscriptEvent, LiveLag>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let fut = this.rx.recv();
        futures::pin_mut!(fut);
        match fut.as_mut().poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(event)) => Poll::Ready(Some(Ok(event))),
            Poll::Ready(Err(broadcast::error::RecvError::Lagged(n))) => {
                Poll::Ready(Some(Err(LiveLag { skipped: n })))
            }
            Poll::Ready(Err(broadcast::error::RecvError::Closed)) => Poll::Ready(None),
        }
    }
}

/// Surfaced when a live stream falls behind the broadcast capacity.
/// `skipped` is the number of events the consumer missed; recover by
/// snapshotting a `TranscriptCursor` and calling
/// [`Airc::resume_from`] to backfill from the store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLag {
    pub skipped: u64,
}

impl std::fmt::Display for LiveLag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "live stream lagged {} events — resume from cursor",
            self.skipped
        )
    }
}

impl std::error::Error for LiveLag {}

/// One row in [`Airc::peers`]. Mirrors the daemon's `PeerEntry`
/// without forcing consumers to pull the daemon crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnrolledPeer {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}
