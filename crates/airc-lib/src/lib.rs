//! `airc-lib` — consumer-facing AIRC API.
//!
//! This crate composes the lower-level substrate crates
//! (`airc-core`, `airc-protocol`, `airc-store`, `airc-transport`,
//! `airc-daemon`) into one `Airc` facade so consumers (Continuum,
//! OpenClaw, Hermes, agent runtimes, the CLI itself) don't have to
//! reconstruct the wiring on every embedding.
//!
//! Two ways to use it:
//!
//! 1. **In-process embedding** via [`Airc::open(home)`]. The handle
//!    owns its identity + store + transports directly; no daemon
//!    involved. Consumers that want their own substrate instance in
//!    the same process.
//!
//! 2. **Daemon-attached** via [`Airc::attach(home, socket)`]. The
//!    handle talks to a long-running daemon process over the IPC
//!    socket; suitable for CLI subcommands and short-lived consumers
//!    that want to share state with persistent runtime processes.
//!
//! Closes grievance §5 (CLI/Daemon Accumulating Policy) and Gate 4
//! (Consumer Embedding) of the operating doc:
//!
//! > Pass when a small consumer app can link `airc-lib` and:
//! > create/load identity; join a channel; send typed body with
//! > headers; subscribe by header/channel/kind; fetch replay; use
//! > blobs; never shell out.
//!
//! Both shapes are now shipped. The doctrine framework is in
//! [`docs/architecture/AIRC-RUST-SUBSTRATE.md`](../../docs/architecture/AIRC-RUST-SUBSTRATE.md)
//! and the consumer roster + table schemas are in
//! [`docs/architecture/CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md`](../../docs/architecture/CAMBRIAN-CONSUMER-INTEGRATION-MATRIX.md)
//! and [`docs/DATA-MODEL-REFERENCE.md`](../../docs/DATA-MODEL-REFERENCE.md).

#![deny(unsafe_code)]

pub mod account_registry;
pub mod airc;
pub mod command_bus;
pub mod coordinator;
mod coordinator_lock;
mod daemon;
pub mod error;
mod fs_permissions;
pub mod gh_account_registry;
pub mod join_context;
mod lan;
pub mod lifecycle;
pub mod mesh_identity;
mod messaging;
mod peers;
pub mod registry;
pub mod room;
pub mod route;
mod stream;
pub mod subscriptions;
mod time;
mod transport;
mod wire_replay;
pub mod work;

pub use account_registry::{
    AccountPeerBeacon, AccountRegistryDocument, AccountRegistryError, AccountRegistryStore,
    SqliteAccountRegistryStore, ACCOUNT_REGISTRY_SCHEMA_VERSION,
};
pub use airc::Airc;
pub use command_bus::PendingCommand;
pub use coordinator::{
    account_root as coordinator_account_root, beacon_now,
    drain_stale_store as coordinator_drain_stale_store,
    load_own_beacon_store as coordinator_load_own_beacon_store,
    publish_store as coordinator_publish_store,
    release_refresh_lock as coordinator_release_refresh_lock,
    snapshot_store as coordinator_snapshot_store, try_acquire_refresh_lock, CoordinatorConfig,
    CoordinatorError, CoordinatorSnapshot, PresenceBeacon, RefreshLockOutcome,
    DEFAULT_HEARTBEAT_TTL_MS as COORDINATOR_HEARTBEAT_TTL_MS,
    DEFAULT_REFRESH_INTERVAL_MS as COORDINATOR_REFRESH_INTERVAL_MS,
};
pub use error::AircError;
pub use gh_account_registry::{gh_auth_ready, GhAccountRegistryStore};
pub use join_context::{JoinContext, GENERAL_CHANNEL};
pub use mesh_identity::{
    load_cached as load_cached_mesh_identity, resolve as resolve_mesh_identity,
    resolve_with as resolve_mesh_identity_with, CachedIdentity, MeshIdentityError,
    Source as MeshIdentitySource, DEFAULT_TTL_MS as MESH_IDENTITY_TTL_MS,
};
pub use peers::EnrolledPeer;
pub use registry::{format_peer_spec, PeerSpec, PeerSpecError};
pub use room::Room;
pub use route::{
    ImportedInvite, InviteBeacon, RouteClass, RouteDecision, RouteDiscoverySnapshot, RouteEndpoint,
    RoutePolicy, TransportCandidate, TransportHealthSample, TransportHealthState,
    TransportHealthTable, TransportKind, TransportResolver, TransportRole, TransportRoute,
};
pub use stream::{EventFilter, EventStream, FilteredEventStream, LiveLag};
pub use subscriptions::{
    derive_room_id, ChannelName, ChannelNameError, MeshIdentity, Subscription, SubscriptionError,
    SubscriptionSet,
};
pub use work::{
    AllocateWorkspace, ChangeWorkLaneState, ClaimManagerHat, ClaimWorkCard, CreateWorkCard,
    CreateWorkLane, HeartbeatWorkspace, ReleaseManagerHat, ReleaseWorkClaim, ReleaseWorkspace,
    RequestWorkspace,
};

// Convenience re-exports so consumers don't need to pull airc-core
// just to type the common return values.
pub use airc_core::{
    body::Body,
    headers::{HeaderFilter, Headers},
    transcript::MentionTarget,
    ClientId, EventId, PeerId, RoomId, TranscriptCursor, TranscriptEvent, TranscriptKind,
};
pub use airc_work::{
    BoardSnapshot, BranchName, CardState, ClaimId, LaneId, LaneState, ManagerHat, Priority,
    ProjectionError, RepoId, WorkBoardProjection, WorkCardId, WorkEvent, WorkspaceId,
    WorkspaceStatus,
};
