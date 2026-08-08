//! Active-agent heartbeat events.
//!
//! Closes GRID-SUBSTRATE-AUDIT flaw #6 from #964: "the active-agent
//! loop is not yet self-managing." Before this module, observers had
//! to read inbox prose ("claude-tab-1 back online") to know which
//! agents were alive. After this module, agents emit typed
//! heartbeats on an interval and any observer can query
//! [`Airc::active_agents(within)`] to get a current liveness view.
//!
//! Headers (filterable without body decode):
//! - `airc.heartbeat.kind` = `"alive"` (reserved space for future
//!   kinds: `"leaving"`, `"degraded"`).
//! - `airc.heartbeat.runtime` = caller-supplied runtime label
//!   (e.g. `"claude"`, `"codex"`, `"interactive"`).
//! - `airc.heartbeat.client` = optional runtime client id
//!   (`"codex:..."`, `"claude:..."`) when known.
//!
//! Frame kind is `Event` (durable — same trade-off the audit's flaw
//! #4 flags). 60s default emit interval keeps noise bounded. The
//! ephemeral-vs-durable substrate split is the right long-term fix
//! and is its own follow-up.
//!
//! Scope cut: this module ships the typed event + emit task + query.
//! It does **not** ship:
//! - Automatic stop when the process exits (caller owns the handle's
//!   lifecycle; dropping aborts the task).
//! - Cross-room scoping on the EMIT side. The READ side is now
//!   room-carrying ([`Airc::active_agents_in`], [`Airc::room_roster_in`],
//!   [`Airc::room_roster_cards_in`]), but beats still go to the current
//!   default room only (`send_frame_to` → `current_room()`).
//!
//!   The consequence is worth stating plainly, because it decides what a
//!   room-scoped roster MEANS today: a peer subscribed to five rooms beats
//!   into one, so asking for the roster of any OTHER room returns empty.
//!   That is honest-and-empty, not wrong — and it is strictly better than
//!   what it replaces, which answered a question about room B with room A's
//!   roster and looked confident doing it. It composes the moment emission
//!   becomes per-room.
//!
//!   Per-room emission is a real design call, not an oversight: beating into
//!   N rooms multiplies presence traffic by N, against a cadence chosen
//!   specifically to keep that traffic bounded. There is likely a better
//!   shape available — the coordinator beacon already carries this scope's
//!   FULL channel list (`publish_presence`), so cross-room presence may be
//!   derivable without multiplying beats at all.
//! - A `"leaving"` event on graceful shutdown (caller can emit one
//!   manually via [`Airc::emit_agent_heartbeat`] with a custom kind
//!   if needed; the typed shape is reserved for it).

use std::sync::Arc;
use std::time::Duration;

use airc_core::headers::Headers;
use airc_core::identity::Identity;
use airc_core::transcript::MentionTarget;
use airc_core::{Body, PeerId, TranscriptEvent};
use airc_protocol::FrameKind;
use airc_work::{AgentAvailabilityState, WorkCardId};
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

use crate::error::AircError;
use crate::time::now_ms;
use crate::Airc;

pub const HEADER_HEARTBEAT_KIND: &str = "airc.heartbeat.kind";
pub const HEADER_HEARTBEAT_RUNTIME: &str = "airc.heartbeat.runtime";
pub const HEADER_HEARTBEAT_CLIENT: &str = "airc.heartbeat.client";

/// Default 60s emit cadence. Tuned so a peer that hasn't beat in
/// 3× the default is unambiguously stale, while keeping heartbeat
/// noise low on a busy inbox.
pub const DEFAULT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(60);

/// What the heartbeat is asserting. Reserved space for future kinds
/// (`Leaving`, `Degraded`) so observers can switch on this string
/// rather than parsing free-text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeartbeatKind {
    /// Periodic "I'm still here" beat.
    Alive,
    /// Caller is shutting down gracefully — observers should mark
    /// this peer offline immediately, not wait for staleness.
    Leaving,
}

impl HeartbeatKind {
    pub fn header_value(self) -> &'static str {
        match self {
            HeartbeatKind::Alive => "alive",
            HeartbeatKind::Leaving => "leaving",
        }
    }
}

/// One typed heartbeat record. Body of the durable event; substrate
/// headers `airc.heartbeat.kind` / `runtime` carry the same values
/// so observers can filter without decoding the body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentHeartbeat {
    pub kind: HeartbeatKind,
    pub peer: PeerId,
    /// Caller-supplied runtime label. Convention: lowercase identifier
    /// (`"claude"`, `"codex"`, `"interactive"`, `"automation"`).
    pub runtime: String,
    /// Optional runtime-client id. This distinguishes multiple tabs
    /// sharing the same durable peer identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    /// Optional caller-supplied scope label. Useful when one agent
    /// runs from multiple project worktrees and observers want to
    /// distinguish them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// Optional build identifier for operators checking whether idle
    /// or broken agents are on stale code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build: Option<String>,
    pub emitted_at_ms: u64,
    /// Optional coordination signal: what this agent is currently
    /// claiming, whether it can accept new work, and which doctrine
    /// it's following. Lets observers answer "what is each agent
    /// doing right now?" from a single beat instead of projecting
    /// the work board. Card aacf2162.
    ///
    /// Defaults to an empty signal for back-compat: peers on
    /// pre-aacf2162 code emit beats without this field, and decoders
    /// see an empty `CoordinationSignal`. New peers populate the
    /// fields they have data for; consumers must treat each sub-field
    /// as optional.
    #[serde(default, skip_serializing_if = "CoordinationSignal::is_empty")]
    pub coordination: CoordinationSignal,
}

/// Coordination payload carried on `AgentHeartbeat`. Every field is
/// optional (defaults to empty/`None`) so older emitters round-trip
/// cleanly and observers can pick up the fields they understand.
///
/// Card aacf2162. The shape captures the three signals AGENTS.md §6
/// names ("current claims, availability, doctrine version") — enough
/// to drive cross-peer "what is everyone doing right now?" displays,
/// scheduling heuristics ("don't propose work to a Busy agent"), and
/// staleness alerts ("this agent is on the wrong doctrine version").
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CoordinationSignal {
    /// Work cards this agent currently holds active claims on at
    /// emit time. Empty when idle. The list is a snapshot — observers
    /// MUST NOT treat it as authoritative once newer events have
    /// landed (the work-board projection remains the source of truth
    /// for claim state, by the store-as-arbiter contract). What this
    /// gives them is a `O(beats)` view of cross-peer activity that
    /// doesn't require replaying the whole board.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub active_claims: Vec<WorkCardId>,
    /// Self-reported availability: Ready / Busy / Away. Distinct from
    /// the substrate's `AgentAvailabilityReported` event (which is
    /// per-repo and explicit); this is a per-beat, per-agent quick
    /// signal that observers can use to route DMs or task proposals.
    /// `None` means "didn't report" (e.g. non-agent runtimes that
    /// don't model availability).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<AgentAvailabilityState>,
    /// Stable identifier of the operating doctrine this agent is
    /// following — typically the commit hash of the `AGENTS.md` it
    /// loaded on attach. Lets observers detect agents on stale
    /// doctrine (`doctrine_version` mismatch is reason to suggest
    /// `airc update` or re-pull). Free-form string; convention is a
    /// short hex sha or a semver-like label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub doctrine_version: Option<String>,
}

impl CoordinationSignal {
    /// True when every field is empty. Used by serde
    /// `skip_serializing_if` to keep beats from pre-aacf2162 callers
    /// (and idle agents with nothing to report) byte-identical to
    /// the original heartbeat shape on the wire.
    pub fn is_empty(&self) -> bool {
        self.active_claims.is_empty()
            && self.availability.is_none()
            && self.doctrine_version.is_none()
    }
}

/// Summary of one currently-alive agent. Returned by
/// [`Airc::active_agents`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLiveness {
    pub peer: PeerId,
    pub runtime: String,
    pub client_id: Option<String>,
    pub scope: Option<String>,
    pub build: Option<String>,
    pub last_seen_ms: u64,
    /// Coordination snapshot from the latest beat (card aacf2162).
    /// Empty when the emitter didn't report any coordination signal
    /// — observers should treat absent fields as "unknown", not
    /// "absent": an idle agent and a pre-aacf2162 agent both surface
    /// here the same way.
    pub coordination: CoordinationSignal,
}

/// One present member of a room: liveness (presence) joined with the
/// peer's published display name. Returned by [`Airc::room_roster`].
///
/// This is the SINGLE airc-side call a roster consumer (continuum's
/// `RoomRosterSource`, the persona-groundedness "[Present in this room]"
/// injection) needs: `active_agents` presence + the `IdentityPublished`
/// name-join done HERE, so the consumer makes ONE call and never parses
/// identity events or runs a second scan itself. airc owns presence +
/// the identity join; the consumer owns what it does with the roster.
///
/// `display_name` is `None` when the peer is present (heartbeating) but
/// has not published an identity card in the scanned window — an honest
/// "present but unnamed", not an omission. `self` is INCLUDED (a roster
/// lists everyone present); a caller rendering "who else is here" drops
/// its own `peer_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMember {
    pub peer_id: PeerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    pub runtime: String,
    /// Self-reported availability from the latest heartbeat's
    /// coordination signal. `None` = the runtime didn't report it
    /// (treat as unknown, not "unavailable").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<AgentAvailabilityState>,
    pub last_seen_ms: u64,
}

/// One present member of a room joined with the peer's FULL identity
/// card. Returned by [`Airc::room_roster_cards`].
///
/// This is [`RoomMember`]'s richer sibling: same liveness fields
/// (presence), but instead of just `display_name` it carries the whole
/// [`Identity`] — `name`, `pronouns`, `role`, `bio`, `status`,
/// `fingerprint`, and the `integrations` badge map. The motivating
/// consumer is continuum's positron desktop roster, which renders an
/// identity card per member (name + pronouns + role + bio + integration
/// badges) and would otherwise pay `room_roster` + N
/// [`Airc::peer_identity_card`] round-trips; this folds the join
/// airc-side so the consumer makes ONE call.
///
/// There is deliberately NO separate `display_name` field: the name
/// lives at `identity.name`, read from the SAME durable per-peer
/// identity index [`Airc::peer_alias`] reads, so there is one name
/// source and no drift (the compression principle — one fact, one
/// place). `identity` is `None` when the peer is present (heartbeating)
/// but has never published a card — the honest "present but uncarded"
/// (mirror of `RoomMember::display_name: None`); a consumer renders a
/// `peer_id` label for those, never an invented name. `self` is
/// INCLUDED (a roster lists everyone present); a caller rendering "who
/// else is here" drops its own `peer_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomMemberCard {
    pub peer_id: PeerId,
    pub runtime: String,
    /// Self-reported availability from the latest heartbeat's
    /// coordination signal. `None` = the runtime didn't report it
    /// (treat as unknown, not "unavailable"). Same field as
    /// [`RoomMember::availability`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub availability: Option<AgentAvailabilityState>,
    pub last_seen_ms: u64,
    /// Full published identity card, or `None` for a present-but-uncarded
    /// peer. `name`/`pronouns`/`role`/`bio`/`integrations` live here —
    /// there is no separate `display_name` to drift against.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<Identity>,
}

/// Handle to a running heartbeat emit task. Dropping or calling
/// [`HeartbeatTask::stop`] aborts the task.
pub struct HeartbeatTask {
    inner: Arc<Mutex<Option<JoinHandle<()>>>>,
}

impl HeartbeatTask {
    fn new(handle: JoinHandle<()>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(Some(handle))),
        }
    }

    /// Stop the heartbeat task. Aborts the spawned tokio task; no
    /// `Leaving` event is emitted. Callers that want a graceful
    /// "I'm leaving" signal should call
    /// [`Airc::emit_agent_heartbeat`] with [`HeartbeatKind::Leaving`]
    /// before dropping the handle.
    pub async fn stop(self) {
        if let Some(handle) = self.inner.lock().await.take() {
            handle.abort();
            let _ = handle.await;
        }
    }
}

impl Drop for HeartbeatTask {
    fn drop(&mut self) {
        let inner = self.inner.clone();
        // Best-effort abort on drop. We can't await from Drop, so
        // spawn a tiny task that takes the handle and aborts.
        // Skipped if a runtime isn't available (unusual outside of
        // tests).
        if tokio::runtime::Handle::try_current().is_ok() {
            tokio::spawn(async move {
                if let Some(handle) = inner.lock().await.take() {
                    handle.abort();
                }
            });
        }
    }
}

impl Airc {
    /// Emit a single heartbeat. Useful for ad-hoc beats (e.g.
    /// `Leaving` on graceful shutdown) outside the periodic task.
    pub async fn emit_agent_heartbeat(
        &self,
        kind: HeartbeatKind,
        runtime: impl Into<String>,
        scope: Option<String>,
    ) -> Result<(), AircError> {
        self.emit_agent_heartbeat_with_metadata(kind, runtime, None, scope, None)
            .await
    }

    /// Emit a heartbeat with runtime metadata. CLI `join` uses this
    /// form so managers can distinguish multiple tabs sharing one
    /// peer identity and spot stale builds.
    pub async fn emit_agent_heartbeat_with_metadata(
        &self,
        kind: HeartbeatKind,
        runtime: impl Into<String>,
        client_id: Option<String>,
        scope: Option<String>,
        build: Option<String>,
    ) -> Result<(), AircError> {
        self.emit_agent_heartbeat_with_coordination(
            kind,
            runtime,
            client_id,
            scope,
            build,
            CoordinationSignal::default(),
        )
        .await
    }

    /// Emit a heartbeat with the full enriched payload — runtime
    /// metadata plus the coordination signal (active claims,
    /// availability, doctrine version) from card aacf2162. Older
    /// `emit_*` methods compose onto this with an empty
    /// `CoordinationSignal`; the wire bytes are byte-identical to
    /// the pre-aacf2162 shape when `coordination.is_empty()`.
    pub async fn emit_agent_heartbeat_with_coordination(
        &self,
        kind: HeartbeatKind,
        runtime: impl Into<String>,
        client_id: Option<String>,
        scope: Option<String>,
        build: Option<String>,
        coordination: CoordinationSignal,
    ) -> Result<(), AircError> {
        let runtime = runtime.into();
        let heartbeat = AgentHeartbeat {
            kind,
            peer: self.peer_id(),
            runtime: runtime.clone(),
            client_id: client_id.clone(),
            scope,
            build,
            emitted_at_ms: now_ms()?,
            coordination,
        };
        let body = serde_json::to_value(&heartbeat)
            .map_err(|error| AircError::Crypto(format!("agent heartbeat encode: {error}")))?;
        let mut headers = Headers::new();
        headers.insert(
            HEADER_HEARTBEAT_KIND.into(),
            kind.header_value().to_string(),
        );
        headers.insert(HEADER_HEARTBEAT_RUNTIME.into(), runtime);
        if let Some(client_id) = client_id {
            headers.insert(HEADER_HEARTBEAT_CLIENT.into(), client_id);
        }
        self.send_frame_to(
            FrameKind::Event,
            MentionTarget::All,
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }

    /// Spawn a background task that emits `Alive` heartbeats every
    /// `interval`. Returns a handle that aborts the task when
    /// dropped or [`HeartbeatTask::stop`]ed.
    ///
    /// Pass [`DEFAULT_HEARTBEAT_INTERVAL`] for the standard 60s
    /// cadence. Shorter intervals are useful in tests; longer
    /// intervals are useful for low-traffic agents that don't need
    /// fine-grained liveness.
    pub async fn start_agent_heartbeat(
        &self,
        runtime: impl Into<String>,
        scope: Option<String>,
        interval: Duration,
    ) -> Result<HeartbeatTask, AircError> {
        self.start_agent_heartbeat_with_metadata(runtime, None, scope, None, interval)
            .await
    }

    /// Spawn a heartbeat task with runtime metadata.
    pub async fn start_agent_heartbeat_with_metadata(
        &self,
        runtime: impl Into<String>,
        client_id: Option<String>,
        scope: Option<String>,
        build: Option<String>,
        interval: Duration,
    ) -> Result<HeartbeatTask, AircError> {
        self.start_agent_heartbeat_with_coordination(
            runtime,
            client_id,
            scope,
            build,
            interval,
            CoordinationSignal::default(),
        )
        .await
    }

    /// Spawn a heartbeat task that emits the coordination signal on
    /// every tick. Card 0bf262eb: closes the populator gap left by
    /// aacf2162 (which added the field but no caller passed one).
    ///
    /// The `coordination` snapshot is captured by the spawned task
    /// and re-emitted on every interval. For MVP scope this is a
    /// static-per-session snapshot — caller-supplied at session
    /// start, never mutated by the task. Future work refreshes
    /// fields like `active_claims` from the live work-board
    /// projection on each tick (tracked as a follow-up to this
    /// card); the wire shape and method signature are stable, so
    /// that refinement is back-compat.
    ///
    /// When `coordination.is_empty()`, the beats are byte-identical
    /// to `start_agent_heartbeat_with_metadata` — observers see no
    /// `coordination` key (skip_serializing_if).
    pub async fn start_agent_heartbeat_with_coordination(
        &self,
        runtime: impl Into<String>,
        client_id: Option<String>,
        scope: Option<String>,
        build: Option<String>,
        interval: Duration,
        coordination: CoordinationSignal,
    ) -> Result<HeartbeatTask, AircError> {
        let runtime = runtime.into();
        // Emit one beat synchronously so observers see the agent
        // alive immediately, before the first interval tick.
        self.emit_agent_heartbeat_with_coordination(
            HeartbeatKind::Alive,
            runtime.clone(),
            client_id.clone(),
            scope.clone(),
            build.clone(),
            coordination.clone(),
        )
        .await?;

        let airc = self.clone();
        let handle = tokio::spawn(async move {
            let mut ticker = tokio::time::interval(interval);
            // Skip the first tick since we already emitted above.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                // Card d4e3e350: refresh active_claims from the live
                // board projection on every tick. Preserves the
                // caller's doctrine_version + availability snapshot
                // (those don't change tick-to-tick in this scope);
                // only the claim list is dynamic. A board query that
                // fails (daemon transient, no current room) degrades
                // to the baseline snapshot — better to send a stale
                // claim list than skip the heartbeat entirely.
                let refreshed = refresh_coordination(&airc, &coordination)
                    .await
                    .unwrap_or_else(|_| coordination.clone());
                if let Err(error) = airc
                    .emit_agent_heartbeat_with_coordination(
                        HeartbeatKind::Alive,
                        runtime.clone(),
                        client_id.clone(),
                        scope.clone(),
                        build.clone(),
                        refreshed,
                    )
                    .await
                {
                    eprintln!("agent heartbeat emit failed: {error}");
                }
            }
        });
        Ok(HeartbeatTask::new(handle))
    }

    /// Return the agents whose most recent heartbeat falls within
    /// `within` of the current wall-clock time. Walks the recent
    /// transcript page (`window` events) and reduces to one entry
    /// per peer (latest-wins).
    ///
    /// Excludes peers whose latest heartbeat was [`HeartbeatKind::
    /// Leaving`] — that's a deliberate offline signal.
    pub async fn active_agents(
        &self,
        within: Duration,
        window: usize,
    ) -> Result<Vec<AgentLiveness>, AircError> {
        self.active_agents_in(None, within, window).await
    }

    /// Live agents in a NAMED room — the room-carrying half of
    /// [`active_agents`](Self::active_agents), which reads whatever this scope's
    /// default subscription happens to be.
    ///
    /// `None` keeps the default-room behaviour exactly (`page_recent_filtered`
    /// scopes an unset channel to the current room), so this is one
    /// implementation, not a forked copy.
    ///
    /// A consumer that carries its own room needs to name it: presence is
    /// per-room, so "who is live" answered from a citizen's default room while
    /// she is acting in another tells her about the wrong company entirely — and
    /// a roster is what she uses to decide who to address.
    pub async fn active_agents_in(
        &self,
        room: Option<airc_core::RoomId>,
        within: Duration,
        window: usize,
    ) -> Result<Vec<AgentLiveness>, AircError> {
        let now = now_ms()?;
        let cutoff = now.saturating_sub(within.as_millis() as u64);
        let filter = crate::EventFilter {
            channel: room,
            ..Default::default()
        };
        let recent = self.page_recent_filtered(filter, window).await?;
        let mut latest: std::collections::HashMap<AgentHeartbeatKey, AgentHeartbeat> =
            std::collections::HashMap::new();
        for event in &recent {
            let Some(beat) = parse_heartbeat(event) else {
                continue;
            };
            if beat.emitted_at_ms < cutoff {
                continue;
            }
            // Keep the highest emitted_at_ms per peer.
            latest
                .entry(AgentHeartbeatKey::from(&beat))
                .and_modify(|existing| {
                    if beat.emitted_at_ms > existing.emitted_at_ms {
                        *existing = beat.clone();
                    }
                })
                .or_insert(beat);
        }
        let mut alive: Vec<AgentLiveness> = latest
            .into_values()
            .filter(|beat| beat.kind == HeartbeatKind::Alive)
            .map(|beat| AgentLiveness {
                peer: beat.peer,
                runtime: beat.runtime,
                client_id: beat.client_id,
                scope: beat.scope,
                build: beat.build,
                last_seen_ms: beat.emitted_at_ms,
                coordination: beat.coordination,
            })
            .collect();
        alive.sort_by_key(|liveness| {
            format!(
                "{}:{}",
                liveness.peer,
                liveness.client_id.as_deref().unwrap_or("")
            )
        });
        Ok(alive)
    }

    /// The room's present membership: [`Self::active_agents`] presence
    /// JOINED with each peer's published display name, in ONE call.
    ///
    /// This is the airc-side roster the persona-groundedness consumer
    /// (continuum `RoomRosterSource`) needs so it does NOT parse
    /// `IdentityPublished` itself or run a second transcript scan: airc
    /// owns presence + the identity name-join; the consumer owns the
    /// prompt injection. `within`/`window` are forwarded to
    /// `active_agents` (liveness cutoff + page size).
    ///
    /// The name-join reads the DURABLE per-peer identity index via the
    /// canonical [`Self::peer_alias`] resolver (one indexed `scoped_state`
    /// read per present peer), NOT a recent-transcript rescan. This is the
    /// fix for roster-name decay: a peer's once-per-join `IdentityPublished`
    /// card scrolls out of any bounded transcript window in a busy room, so
    /// the old scan returned `None` for every name; the durable index
    /// retains the name for as long as the peer has ever announced one.
    ///
    /// Ordering mirrors `active_agents` (stable by peer + client_id).
    /// `self` is included — a roster lists everyone present; a caller
    /// rendering "who else is here" filters its own `peer_id`.
    pub async fn room_roster(
        &self,
        within: Duration,
        window: usize,
    ) -> Result<Vec<RoomMember>, AircError> {
        self.room_roster_in(None, within, window).await
    }

    /// The roster of a NAMED room. Room-carrying half of
    /// [`room_roster`](Self::room_roster); `None` is the default room, so this
    /// is one implementation rather than a copy.
    pub async fn room_roster_in(
        &self,
        room: Option<airc_core::RoomId>,
        within: Duration,
        window: usize,
    ) -> Result<Vec<RoomMember>, AircError> {
        let live = self.active_agents_in(room, within, window).await?;
        let mut members = Vec::with_capacity(live.len());
        for liveness in live {
            members.push(RoomMember {
                display_name: self.peer_alias(liveness.peer).await?,
                peer_id: liveness.peer,
                runtime: liveness.runtime,
                availability: liveness.coordination.availability,
                last_seen_ms: liveness.last_seen_ms,
            });
        }
        Ok(members)
    }

    /// Richer roster: like [`Self::room_roster`] but each member carries
    /// the FULL [`Identity`] card instead of just `display_name`. The
    /// motivating consumer is continuum's positron desktop roster, which
    /// renders name + pronouns + role + bio + the `integrations` badge
    /// map per member; without this it would pay `room_roster` + N
    /// [`Self::peer_identity_card`] round-trips. This folds the identity
    /// join airc-side so the consumer makes ONE call — the same "airc
    /// owns presence + the identity join" contract as `room_roster`.
    ///
    /// Each card's `identity` reads the SAME durable per-peer identity
    /// index as [`Self::peer_alias`] / [`Self::peer_identity_card`]
    /// (`scoped_state`, daemon-routed when attached), so it survives the
    /// `IdentityPublished` event scrolling out of the recent transcript
    /// window. A present peer that has never published a card surfaces as
    /// `identity: None` — the honest "present but uncarded", mirroring
    /// `room_roster`'s `display_name: None`.
    ///
    /// Ordering mirrors [`Self::active_agents`] (stable by peer +
    /// client_id). `self` is included — a roster lists everyone present;
    /// a caller rendering "who else is here" filters its own `peer_id`.
    ///
    /// Cost: one `peer_identity_card` read per present peer. At roster
    /// scale (tens of members) the sequential reads are fine; a batched
    /// `peer_identity_cards(&[PeerId])` daemon path is the documented
    /// future optimization if a busy room makes the round-trips bite.
    pub async fn room_roster_cards(
        &self,
        within: Duration,
        window: usize,
    ) -> Result<Vec<RoomMemberCard>, AircError> {
        self.room_roster_cards_in(None, within, window).await
    }

    /// The identity-joined roster of a NAMED room. Room-carrying half of
    /// [`room_roster_cards`](Self::room_roster_cards); `None` is the default
    /// room. This is the variant a room-scoped consumer wants — per #262 the
    /// bare roster renders `peer-xxxx` rows, so a caller that reaches for the
    /// room-correct read must not have to give up the identity join to get it.
    pub async fn room_roster_cards_in(
        &self,
        room: Option<airc_core::RoomId>,
        within: Duration,
        window: usize,
    ) -> Result<Vec<RoomMemberCard>, AircError> {
        let live = self.active_agents_in(room, within, window).await?;
        let mut members = Vec::with_capacity(live.len());
        for liveness in live {
            let identity = self
                .peer_identity_card(liveness.peer)
                .await?
                .map(|card| card.identity);
            members.push(RoomMemberCard {
                peer_id: liveness.peer,
                runtime: liveness.runtime,
                availability: liveness.coordination.availability,
                last_seen_ms: liveness.last_seen_ms,
                identity,
            });
        }
        Ok(members)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AgentHeartbeatKey {
    peer: PeerId,
    client_id: Option<String>,
}

impl AgentHeartbeatKey {
    fn from(beat: &AgentHeartbeat) -> Self {
        Self {
            peer: beat.peer,
            client_id: beat.client_id.clone(),
        }
    }
}

fn parse_heartbeat(event: &TranscriptEvent) -> Option<AgentHeartbeat> {
    let _ = event.headers.get(HEADER_HEARTBEAT_KIND)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

/// Card d4e3e350: re-compute the dynamic fields of `CoordinationSignal`
/// against the live work-board projection. Preserves `doctrine_version`
/// and `availability` from `baseline` (those are session-scoped and
/// don't change tick-to-tick); overrides `active_claims` with the
/// current room's cards owned by this peer. Caller passes the
/// baseline so a transient board-query failure can degrade to it
/// rather than emitting an empty signal.
///
/// Returns `Err` only on a genuine query failure. The heartbeat task
/// surfaces that as a falls-back to baseline and keeps beating —
/// "stale claim list" beats "no heartbeat at all" for liveness.
async fn refresh_coordination(
    airc: &crate::Airc,
    baseline: &CoordinationSignal,
) -> Result<CoordinationSignal, AircError> {
    let board = airc
        .work_board_complete(crate::WORK_BOARD_PROJECTION_PAGE_SIZE)
        .await?;
    let me = airc.peer_id();
    let active_claims: Vec<WorkCardId> = board
        .snapshot()
        .cards
        .iter()
        .filter(|card| card.owner == Some(me) && card.claim_id.is_some())
        .map(|card| card.card_id)
        .collect();
    Ok(CoordinationSignal {
        active_claims,
        doctrine_version: baseline.doctrine_version.clone(),
        availability: baseline.availability,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // what this catches: a question about room B being answered with room A's
    // roster. That is what every default-scoped presence read does today, and it
    // is the exact gate-says-A-read-says-B shape `project_room_work_board` was
    // made public to fix. A citizen who belongs to several rooms decides WHO TO
    // ADDRESS from this read, so answering from her default room hands her the
    // wrong company with full confidence.
    //
    // Also pins the honest half of the current limitation: heartbeats are still
    // EMITTED only into the current room, so a room nobody beats into reads
    // EMPTY. Empty is the correct answer to "who is live in a room with no
    // beats" — the failure this replaces was a populated, plausible, wrong one.
    // If per-room emission lands later, this test keeps holding.
    #[tokio::test]
    async fn a_rooms_roster_is_never_another_rooms_roster() {
        let dir = tempfile::tempdir().unwrap();
        let airc = crate::Airc::open_with_wire_root_for_test(
            dir.path().join("citizen/.airc"),
            dir.path().join("wire"),
        )
        .await
        .expect("open scope");

        let home = airc.join("academy").await.expect("join home room");
        let other = airc
            .subscribe_room("cambriantech")
            .await
            .expect("belong to a second room without moving focus");

        airc.emit_agent_heartbeat(HeartbeatKind::Alive, "test", None)
            .await
            .expect("beat");

        let within = Duration::from_secs(300);
        let here = airc
            .room_roster_in(Some(home.channel), within, 200)
            .await
            .expect("roster of the room she beats into");
        assert_eq!(here.len(), 1, "her own beat is visible in her home room");

        let there = airc
            .room_roster_in(Some(other.channel), within, 200)
            .await
            .expect("roster of the other room");
        assert!(
            there.is_empty(),
            "a room she has never beaten into must report NOBODY, never her home \
             room's roster — got {there:?}"
        );

        // `None` must stay exactly the old default-room behaviour, or this is a
        // breaking change dressed as an addition.
        let default_scoped = airc
            .room_roster_in(None, within, 200)
            .await
            .expect("default-scoped roster");
        assert_eq!(
            default_scoped.len(),
            here.len(),
            "None means the current room — unchanged for every existing caller"
        );
    }

    fn sample_alive() -> AgentHeartbeat {
        AgentHeartbeat {
            kind: HeartbeatKind::Alive,
            peer: PeerId::new(),
            runtime: "claude".to_string(),
            client_id: Some("claude:tab-1".to_string()),
            scope: Some("/work/airc".to_string()),
            build: Some("abc123".to_string()),
            emitted_at_ms: 1_700_000_000_000,
            coordination: CoordinationSignal::default(),
        }
    }

    fn sample_leaving() -> AgentHeartbeat {
        AgentHeartbeat {
            kind: HeartbeatKind::Leaving,
            peer: PeerId::new(),
            runtime: "codex".to_string(),
            client_id: None,
            scope: None,
            build: None,
            emitted_at_ms: 1_700_000_001_000,
            coordination: CoordinationSignal::default(),
        }
    }

    fn sample_coordination() -> CoordinationSignal {
        CoordinationSignal {
            active_claims: vec![
                WorkCardId::from_u128(0xAACF_2162),
                WorkCardId::from_u128(0x75B5_4D0A),
            ],
            availability: Some(AgentAvailabilityState::Busy),
            doctrine_version: Some("agents-md@08eb758".to_string()),
        }
    }

    #[test]
    fn alive_heartbeat_round_trips_through_json() {
        let beat = sample_alive();
        let json = serde_json::to_string(&beat).expect("encode");
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, beat);
    }

    #[test]
    fn leaving_heartbeat_round_trips_through_json() {
        let beat = sample_leaving();
        let json = serde_json::to_string(&beat).expect("encode");
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, beat);
    }

    #[test]
    fn header_values_stable() {
        assert_eq!(HeartbeatKind::Alive.header_value(), "alive");
        assert_eq!(HeartbeatKind::Leaving.header_value(), "leaving");
    }

    #[test]
    fn scope_is_optional_in_json() {
        let beat = AgentHeartbeat {
            kind: HeartbeatKind::Alive,
            peer: PeerId::new(),
            runtime: "interactive".to_string(),
            client_id: None,
            scope: None,
            build: None,
            emitted_at_ms: 0,
            coordination: CoordinationSignal::default(),
        };
        let json = serde_json::to_string(&beat).expect("encode");
        // `serde(skip_serializing_if = "Option::is_none")` should
        // omit the scope key entirely when None.
        assert!(!json.contains("\"scope\""));
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.scope, None);
    }

    // -----------------------------------------------------------------
    // Card aacf2162 — coordination signal on the heartbeat.
    // -----------------------------------------------------------------

    /// A new beat that populates the coordination signal must
    /// round-trip every field byte-identically. This is the wire
    /// contract: an observer that sees this beat must reconstruct
    /// "Busy, holding cards X and Y, doctrine 08eb758" without loss.
    #[test]
    fn coordination_signal_round_trips_through_json() {
        let mut beat = sample_alive();
        beat.coordination = sample_coordination();
        let json = serde_json::to_string(&beat).expect("encode");
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, beat);
        assert_eq!(decoded.coordination.active_claims.len(), 2);
        assert_eq!(
            decoded.coordination.availability,
            Some(AgentAvailabilityState::Busy)
        );
        assert_eq!(
            decoded.coordination.doctrine_version.as_deref(),
            Some("agents-md@08eb758"),
        );
    }

    /// An empty `CoordinationSignal` must not appear in the JSON at
    /// all. That keeps the wire bytes byte-identical to the
    /// pre-aacf2162 shape for idle agents and pre-aacf2162 callers,
    /// so the change is invisible to peers that don't care.
    #[test]
    fn empty_coordination_is_omitted_from_json() {
        let beat = sample_alive();
        assert!(beat.coordination.is_empty());
        let json = serde_json::to_string(&beat).expect("encode");
        assert!(
            !json.contains("\"coordination\""),
            "empty CoordinationSignal must not be serialised; got {json}"
        );
    }

    /// Decoding a pre-aacf2162 beat (no `coordination` key at all)
    /// must succeed and yield an empty signal. This is the back-compat
    /// half: a fresh client must not refuse heartbeats from older
    /// peers still on the wire.
    ///
    /// The "legacy" fixture is synthesized from a real `AgentHeartbeat`
    /// with an empty coordination signal — the same JSON a pre-aacf2162
    /// emitter would have produced, and the same JSON a current
    /// emitter produces when the coordination signal is empty (the
    /// `empty_coordination_is_omitted_from_json` test pins that
    /// invariant). Using `PeerId::new()` (v4) for the fixture avoids
    /// hand-rolled UUID strings, which can be malformed or accidentally
    /// collide with real identities in p2p.
    #[test]
    fn pre_aacf2162_beats_decode_with_empty_coordination() {
        let beat = sample_alive();
        assert!(beat.coordination.is_empty());
        let legacy_wire = serde_json::to_string(&beat).expect("encode legacy-shaped beat");
        assert!(
            !legacy_wire.contains("\"coordination\""),
            "fixture must lack a coordination key to exercise the back-compat path; \
             got {legacy_wire}"
        );
        let decoded: AgentHeartbeat =
            serde_json::from_str(&legacy_wire).expect("legacy-shape decode");
        assert_eq!(decoded.kind, HeartbeatKind::Alive);
        assert_eq!(decoded.runtime, beat.runtime);
        assert_eq!(decoded.peer, beat.peer);
        assert!(
            decoded.coordination.is_empty(),
            "missing coordination key must deserialize to the empty signal, \
             not produce an error"
        );
    }

    /// Each sub-field is independently optional: an emitter that
    /// only knows its own availability (but not active claims or
    /// doctrine) must still round-trip cleanly.
    #[test]
    fn partial_coordination_signals_round_trip_independently() {
        let only_availability = CoordinationSignal {
            availability: Some(AgentAvailabilityState::Away),
            ..Default::default()
        };
        let only_claims = CoordinationSignal {
            active_claims: vec![WorkCardId::from_u128(1)],
            ..Default::default()
        };
        let only_doctrine = CoordinationSignal {
            doctrine_version: Some("v1".to_string()),
            ..Default::default()
        };
        for signal in [only_availability, only_claims, only_doctrine] {
            let mut beat = sample_alive();
            beat.coordination = signal.clone();
            let json = serde_json::to_string(&beat).expect("encode");
            let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
            assert_eq!(decoded.coordination, signal);
        }
    }

    /// Card 0bf262eb: the `start_*_with_coordination` task captures
    /// the supplied snapshot and re-emits it on every tick. Today the
    /// snapshot is static for the session (refresh-from-board-each-tick
    /// is a follow-up); this test pins the bytes that go on the wire
    /// per beat so the static-snapshot contract is explicit.
    #[test]
    fn coordination_snapshot_emit_path_round_trips() {
        // The MVP populator in airc-cli/src/commands.rs supplies the
        // build SHA as `doctrine_version` and leaves the other fields
        // default — that's the wire shape an observer must currently
        // expect from `airc join` on this commit.
        let snapshot = CoordinationSignal {
            doctrine_version: Some("b56c735".to_string()),
            ..Default::default()
        };
        assert!(!snapshot.is_empty(), "MVP snapshot must serialize");

        let beat = AgentHeartbeat {
            kind: HeartbeatKind::Alive,
            peer: PeerId::new(),
            runtime: "claude".to_string(),
            client_id: None,
            scope: Some("/work/airc".to_string()),
            build: Some("b56c735".to_string()),
            emitted_at_ms: 1_700_000_000_000,
            coordination: snapshot.clone(),
        };
        let json = serde_json::to_string(&beat).expect("encode");
        // The MVP populator emits exactly one coordination field.
        assert!(
            json.contains("\"doctrine_version\":\"b56c735\""),
            "doctrine_version must appear on the wire; got {json}"
        );
        // The other two fields are still defaulted away by
        // `skip_serializing_if` — keeps beats small and the wire
        // shape forward-compatible for future populators.
        assert!(
            !json.contains("\"active_claims\""),
            "MVP must NOT emit active_claims yet (default empty Vec); got {json}"
        );
        assert!(
            !json.contains("\"availability\""),
            "MVP must NOT emit availability yet (default None); got {json}"
        );
        let decoded: AgentHeartbeat = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded.coordination, snapshot);
    }

    /// `is_empty` is the predicate `skip_serializing_if` and observer
    /// "this agent reported nothing" logic both rely on. Pin it.
    #[test]
    fn is_empty_only_true_when_every_field_unset() {
        assert!(CoordinationSignal::default().is_empty());
        assert!(!CoordinationSignal {
            active_claims: vec![WorkCardId::from_u128(1)],
            ..Default::default()
        }
        .is_empty());
        assert!(!CoordinationSignal {
            availability: Some(AgentAvailabilityState::Ready),
            ..Default::default()
        }
        .is_empty());
        assert!(!CoordinationSignal {
            doctrine_version: Some("x".into()),
            ..Default::default()
        }
        .is_empty());
    }
}
