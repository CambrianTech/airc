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

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::RwLock;

use airc_core::{
    body::Body, headers::Headers, transcript::MentionTarget, ClientId, EventId, PeerId,
    TranscriptCursor, TranscriptEvent,
};
use airc_daemon::{peers_store, LocalIdentity};
use airc_protocol::{Envelope, Frame, FrameKind, PeerKeyRegistry, Signature, VerificationPolicy};
use airc_store::{EventStore, SqliteEventStore};
use airc_transport::{LocalFsAdapter, SignedTransport, Transport};

use crate::error::AircError;
use crate::registry::PeerSpec;
use crate::room::{self, Room};

const EVENTS_DB_FILENAME: &str = "events.sqlite";

/// In-process AIRC handle. Holds identity, store, and per-room
/// signed-local-fs transports. Wrap in `Arc` if you need to share
/// across tasks.
pub struct Airc {
    home: PathBuf,
    identity: LocalIdentity,
    store: Arc<dyn EventStore>,
    registry: Arc<RwLock<PeerKeyRegistry>>,
    policy: VerificationPolicy,
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

        Ok(Self {
            home,
            identity,
            store,
            registry,
            policy,
        })
    }

    /// Return the home directory backing this handle.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Return the local peer's stable identifier.
    pub fn peer_id(&self) -> PeerId {
        self.identity.peer_id
    }

    /// Return the per-session client identifier.
    pub fn client_id(&self) -> ClientId {
        self.identity.client_id
    }

    /// Return the peer-spec string suitable for sharing with another
    /// peer so they can enrol this identity into their trust registry.
    pub fn peer_spec(&self) -> String {
        crate::registry::format_peer_spec(
            self.identity.peer_id,
            &self.identity.keypair.public_bytes(),
        )
    }

    /// Switch the current room to one derived from `name`. Same name
    /// on two peers yields the same channel UUID via UUIDv5, so they
    /// converge without exchanging the UUID out-of-band.
    pub async fn join(&self, name: &str) -> Result<Room, AircError> {
        let room = Room::from_name(&self.home, name);
        room::save(&self.home, &room)?;
        Ok(room)
    }

    /// Read the persisted current room. Returns the default room
    /// (synthesised on the fly, NOT persisted) if no `room.json`
    /// has been written yet.
    pub async fn current_room(&self) -> Result<Room, AircError> {
        Ok(room::load_or_default(&self.home)?)
    }

    /// Send a plain-text message to the current room. Returns the
    /// event's `EventId` so callers can correlate or filter their
    /// own echo.
    pub async fn say(&self, text: &str) -> Result<EventId, AircError> {
        self.send(Body::text(text), Headers::new()).await
    }

    /// Send a frame with typed body and arbitrary headers to the
    /// current room. Frame is signed under the local identity and
    /// written through the room's local-fs wire.
    pub async fn send(&self, body: Body, headers: Headers) -> Result<EventId, AircError> {
        let room = self.current_room().await?;
        let event_id = EventId::new();
        let frame = Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id,
                sender: self.identity.peer_id,
                sender_client: self.identity.client_id,
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
            self.identity.keypair.clone(),
            self.identity.peer_id,
            self.registry.clone(),
            self.policy,
        );
        transport
            .send(frame)
            .await
            .map_err(|e| AircError::Transport(e.to_string()))?;
        Ok(event_id)
    }

    /// Fetch the most recent `limit` events from the durable store,
    /// filtered to the current room. Returns events in transcript
    /// order (oldest → newest within the page).
    pub async fn page_recent(&self, limit: usize) -> Result<Vec<TranscriptEvent>, AircError> {
        let room = self.current_room().await?;
        Ok(self.store.page_recent(Some(room.channel), limit).await?)
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
            .store
            .resume_from(cursor, Some(room.channel), limit)
            .await?)
    }

    /// Cursor of the newest event in the current room, or `None` if
    /// the room has no events yet.
    pub async fn latest_cursor(&self) -> Result<Option<TranscriptCursor>, AircError> {
        let room = self.current_room().await?;
        Ok(self.store.latest_cursor(Some(room.channel)).await?)
    }

    /// Append a `TranscriptEvent` to the durable store directly. The
    /// caller is responsible for the event's identity/lamport fields
    /// being correct. Useful for consumers replaying captured events
    /// or for tests that want to seed history.
    pub async fn append_event(&self, event: TranscriptEvent) -> Result<(), AircError> {
        Ok(self.store.append(event).await?)
    }

    /// Enrol a peer into the local trust registry and persist it to
    /// `<home>/peers.json`. Pass the peer-spec the remote produced
    /// via [`Airc::peer_spec`].
    pub async fn add_peer(&self, spec: PeerSpec) -> Result<(), AircError> {
        peers_store::add(&self.home, spec.peer_id, spec.pubkey)?;
        let mut registry = self
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
        let stored = peers_store::load(&self.home)?;
        Ok(stored
            .into_iter()
            .map(|p| EnrolledPeer {
                peer_id: p.peer_id,
                pubkey_b64: p.pubkey_b64,
            })
            .collect())
    }
}

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
