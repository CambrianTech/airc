//! Unified error type for the consumer API.
//!
//! Wraps the underlying crate errors (store, transport, identity,
//! room) so consumers see one `AircError` rather than juggling four.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum AircError {
    #[error("identity: {0}")]
    Identity(#[from] airc_daemon::IdentityError),

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

    #[error("peer spec: {0}")]
    PeerSpec(#[from] crate::registry::PeerSpecError),

    #[error("peers store: {0}")]
    PeersStore(#[from] airc_daemon::peers_store::PeersStoreError),

    #[error("daemon client: {0}")]
    DaemonClient(#[from] airc_daemon::ClientError),

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
