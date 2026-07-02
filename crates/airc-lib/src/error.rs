//! Unified error type for the consumer API.
//!
//! Wraps the underlying crate errors (store, transport, identity,
//! room) so consumers see one `AircError` rather than juggling four.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AircError {
    #[error("identity: {0}")]
    Identity(#[from] airc_identity::IdentityError),

    #[error("event store: {0}")]
    Store(#[from] airc_store::StoreError),

    #[error("work store: {0}")]
    WorkStore(#[from] airc_work_store::WorkStoreError),

    #[error("work projection: {0}")]
    WorkProjection(#[from] airc_work::ProjectionError),

    #[error("work event codec: {0}")]
    WorkCodec(#[from] airc_work::WorkEventCodecError),

    #[error("local git observer: {0}")]
    LocalGit(#[from] airc_work::LocalGitError),

    #[error("pull request source: {0}")]
    PullRequestSource(#[from] airc_work::PullRequestSourceError),

    #[error("room state: {0}")]
    Room(#[from] crate::room::RoomError),

    #[error("subscription state: {0}")]
    Subscription(#[from] crate::subscriptions::SubscriptionError),

    #[error("channel name: {0}")]
    ChannelName(#[from] crate::subscriptions::ChannelNameError),

    #[error("mesh identity: {0}")]
    MeshIdentity(#[from] crate::mesh_identity::MeshIdentityError),

    #[error("account coordinator: {0}")]
    Coordinator(#[from] crate::coordinator::CoordinatorError),

    #[error("account registry: {0}")]
    AccountRegistry(#[from] crate::account_registry::AccountRegistryError),

    #[error("system clock before UNIX_EPOCH: {0}")]
    Clock(#[from] std::time::SystemTimeError),

    /// JSON (de)serialization of a value the consumer API owns — e.g. the
    /// `Identity` card serialized into the durable per-peer identity index
    /// (scoped_state). A programmer error in practice for fixed-shape
    /// structs, surfaced rather than swallowed.
    #[error("serde: {0}")]
    Serde(#[from] serde_json::Error),

    #[error("peer spec: {0}")]
    PeerSpec(#[from] crate::registry::PeerSpecError),

    #[error("peers store: {0}")]
    PeersStore(#[from] airc_trust::PeersStoreError),

    #[error("daemon client: {0}")]
    DaemonClient(#[from] airc_ipc::ClientError),

    /// Transport-side I/O. Stringified because LocalFsAdapter and
    /// LanTcpAdapter return different concrete error types and a
    /// blanket `#[from]` per backend is more weight than it's worth
    /// at this layer.
    #[error("transport: {0}")]
    Transport(String),

    /// Route resolver refused or selected a route the current sender
    /// cannot execute.
    #[error("route: {0}")]
    Route(String),

    /// Caller asked for an operation that needs an active room but
    /// the state has none yet. Construct one via `Airc::join`.
    #[error("no current room — call `join` to set one")]
    NoCurrentRoom,

    /// Caller asked to leave a channel that this scope is not
    /// currently subscribed to.
    #[error("not subscribed to channel: {0}")]
    NotSubscribed(String),

    /// Caller passed a uuid-shaped string to [`crate::Airc::join`].
    /// `join(name)` takes a channel NAME and hashes it into the
    /// channel UUID; a uuid-shaped string gets re-hashed and
    /// produces a brand-new channel whose UUID does NOT match the
    /// caller's intent. Card c409eaf5 makes that class of silent
    /// failure loud. Either pass the channel NAME (like "continuum"
    /// or "general"), or — if you already hold a channel UUID and
    /// want to attach to the existing channel — look it up via the
    /// subscription set and pass its actual name. UUIDs do not
    /// round-trip through `ChannelName::new`.
    #[error(
        "join refused: {string:?} looks like a UUID. \
         `Airc::join(name)` takes a channel NAME and derives the channel UUID by hashing — \
         passing a UUID string re-hashes it into a different channel. \
         If you want to attach to an existing channel by its UUID, look it up via the \
         subscription set and pass its name to join."
    )]
    JoinUuidString { string: String },

    /// Caller attempted to mutate a work card from a room whose work
    /// projection does not contain that card. Work cards are
    /// room-scoped coordination state; transitions from another room
    /// would create false projections.
    #[error(
        "work card {card_id} is not in current room {room_name} ({room_id}); switch to the card's room before mutating it"
    )]
    WorkCardNotInCurrentRoom {
        card_id: airc_work::WorkCardId,
        room_name: String,
        room_id: airc_core::RoomId,
    },

    /// Caller attempted to create a second active claim for a card
    /// that already has one. Claims are leases; duplicate active
    /// claims make manager/persona training data ambiguous.
    #[error("work card {card_id} already has active claim {claim_id:?} owned by {owner:?}")]
    WorkCardAlreadyClaimed {
        card_id: airc_work::WorkCardId,
        claim_id: Option<airc_work::ClaimId>,
        owner: Option<airc_core::PeerId>,
    },

    /// Card 09fddedd: `relink` supersedes an EXISTING link — a card
    /// with no linked PR has nothing to supersede. Use `airc work
    /// link` for the first link; refusing here keeps the audit
    /// semantics honest (`PullRequestRelinked.old_pull_request` must
    /// record a real prior link, never a fabricated placeholder).
    #[error(
        "work card {card_id} has no linked pull_request to supersede; use `airc work link` for the first link"
    )]
    WorkCardHasNoLinkedPullRequest { card_id: airc_work::WorkCardId },

    /// Card 09fddedd: relinking a card to the PR it already tracks is
    /// always an operator mistake (wrong card id or wrong PR number) —
    /// emitting a no-op supersede event would only pollute the audit
    /// trail, so the SDK refuses loudly instead.
    #[error(
        "work card {card_id} is already linked to PR #{number} ({repo}); relink requires a different successor PR"
    )]
    WorkCardRelinkSamePullRequest {
        card_id: airc_work::WorkCardId,
        repo: airc_work::RepoId,
        number: u64,
    },

    /// Card 09fddedd: a card in a terminal state (`Merged`/`Closed`)
    /// has finished its lifecycle; superseding its PR link would point
    /// the merger at a successor for work that already shipped. The
    /// projection drops such events defensively on replay; the SDK
    /// refuses up front so the operator hears about it.
    #[error(
        "work card {card_id} is in terminal state {state:?}; its pull_request link cannot be superseded"
    )]
    WorkCardRelinkTerminalState {
        card_id: airc_work::WorkCardId,
        state: airc_work::CardState,
    },

    /// Caller passed a peer registry operation referencing a peer
    /// not in the local registry.
    #[error("unknown peer: {0}")]
    UnknownPeer(airc_core::PeerId),

    /// Underlying signing key was unloadable / corrupted in a way
    /// that the identity layer didn't already classify.
    #[error("crypto: {0}")]
    Crypto(String),

    /// A `PendingCommand`'s deadline elapsed before any matching
    /// reply event arrived on the broadcast stream. The receiver
    /// may still be processing the request; consumers that need
    /// idempotent retry should send a new request with a fresh
    /// correlation id rather than reusing the timed-out one.
    #[error("command deadline elapsed (correlation_id={correlation_id})")]
    CommandDeadline { correlation_id: uuid::Uuid },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn join_uuid_string_error_names_the_input() {
        let err = AircError::JoinUuidString {
            string: "11c1a7ac-cb85-5ca0-a5b4-2847280ea3fa".to_string(),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("11c1a7ac-cb85-5ca0-a5b4-2847280ea3fa"),
            "{msg}"
        );
        assert!(msg.contains("looks like a UUID"), "{msg}");
        assert!(msg.contains("channel NAME"), "{msg}");
    }

    /// Card c409eaf5 — pin the canonical detection: every uuid shape
    /// the standard parser accepts MUST trip the guard, otherwise the
    /// trap re-opens for the next consumer that picks an alternate
    /// uuid format. Bare hex (no hyphens) and braced are both valid
    /// per uuid::Uuid::parse_str.
    #[test]
    fn uuid_detection_canonical_shapes_all_trip() {
        let shapes = [
            "11c1a7ac-cb85-5ca0-a5b4-2847280ea3fa",     // hyphenated
            "11c1a7accb855ca0a5b42847280ea3fa",         // bare hex
            "{11c1a7ac-cb85-5ca0-a5b4-2847280ea3fa}",   // braced
            "  11c1a7ac-cb85-5ca0-a5b4-2847280ea3fa  ", // padded
        ];
        for s in shapes {
            assert!(
                uuid::Uuid::parse_str(s.trim()).is_ok(),
                "uuid::parse_str must accept {s:?} — if this fails, \
                 either the guard needs to broaden or the test case is wrong"
            );
        }
    }

    /// Card c409eaf5 — false-positive guard. Real channel names must
    /// pass through; the predicate is uuid-specific, not "anything
    /// hex-looking."
    #[test]
    fn uuid_detection_rejects_real_channel_names() {
        let names = ["continuum", "general", "cambriantech", "ai-dev", "room-1"];
        for s in names {
            assert!(
                uuid::Uuid::parse_str(s.trim()).is_err(),
                "uuid::parse_str must REJECT real channel name {s:?}"
            );
        }
    }
}
