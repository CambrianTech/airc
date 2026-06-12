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
pub mod adapter;
pub mod agent_heartbeat;
pub mod airc;
mod broadcast_deduper;
pub mod capability_registry;
pub mod command_bus;
pub mod coordinator;
mod daemon;
pub mod diagnostic_event_sink;
pub mod error;
pub mod external_identity;
pub mod gh_account_registry;
pub mod gh_client;
pub mod join_context;
mod lan;
pub mod lane_coordination;
pub mod lifecycle;
pub mod mesh_identity;
mod messaging;
mod peers;
pub mod publish;
pub mod registry;
pub mod registry_refresh;
mod relay;
pub mod room;
pub mod route;
mod stream;
pub mod subscriptions;
pub mod task_negotiation;
mod time;
mod transport;
mod udp;
mod webrtc;
pub mod webrtc_media;
pub mod webrtc_signaling;
pub mod work;
pub(crate) mod work_board_cache;
pub mod work_manager;
pub mod work_roster;
pub mod work_subscription;

pub use account_registry::{
    merge_registry_documents, scope_home_is_temp_rooted, AccountPeerBeacon,
    AccountRegistryDocument, AccountRegistryError, AccountRegistryStore, RegistryMergeOutcome,
    SqliteAccountRegistryStore, ACCOUNT_REGISTRY_SCHEMA_VERSION,
};
pub use agent_heartbeat::{
    AgentHeartbeat, AgentLiveness, CoordinationSignal, HeartbeatKind, HeartbeatTask,
    DEFAULT_HEARTBEAT_INTERVAL, HEADER_HEARTBEAT_KIND, HEADER_HEARTBEAT_RUNTIME,
};
pub use airc::{machine_account_home, Airc};
pub use airc_protocol::{
    AssertionError, IdentityAssertion, HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_DEADLINE,
    HEADER_AIRC_REPLY_TO,
};
pub use capability_registry::{
    CapabilityCandidate, CapabilityEntry, CapabilityQuery, CapabilityRegistry, DEFAULT_OFFER_TTL_MS,
};
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
pub use daemon::decode_wire_event;
pub use diagnostic_event_sink::{
    AircEventDiagnosticSink, HEADER_DIAG_CODE, HEADER_DIAG_COMPONENT, HEADER_DIAG_SEVERITY,
};
// Observability macros live in the substrate (airc-diagnostics) so
// every consumer reaches for them downward: `airc_lib::probe!` /
// `airc_lib::time_probe!`. The `probe` re-export carries both the
// `probe!`/`time_probe!` macros and the `probe::class` constants.
pub use airc_diagnostics::{probe, time_probe};
pub use error::AircError;
pub use external_identity::{
    BridgedMessage, BridgedMessageFilter, ExternalIdentity, ExternalIdentitySource,
    HEADER_BRIDGE_HANDLE, HEADER_BRIDGE_SOURCE,
};
pub use gh_account_registry::{
    account_registry_block, gh_auth_ready, writer_filename, writer_key, AccountRegistryBlock,
    GhAccountRegistryStore, AIRC_DISABLE_ACCOUNT_REGISTRY_ENV,
};
pub use gh_client::{
    parse_pr_url, parse_pr_view, GhCheck, GhClient, GhError, MergeReceipt, PrCreateArgs, PrCreated,
    PrEditBaseArgs, PrMergeArgs, PrView, PrViewArgs,
};
pub use join_context::{JoinContext, GENERAL_CHANNEL};
pub use lane_coordination::{
    LaneAction, LaneCoordinationEvent, LaneStatus, HEADER_COORD_KIND, HEADER_COORD_LANE_ID,
    HEADER_COORD_PR,
};
pub use mesh_identity::{
    load_cached as load_cached_mesh_identity, resolve as resolve_mesh_identity,
    resolve_with as resolve_mesh_identity_with, CachedIdentity, MeshIdentityError,
    Source as MeshIdentitySource, DEFAULT_TTL_MS as MESH_IDENTITY_TTL_MS,
};
pub use peers::EnrolledPeer;
pub use publish::{PublishReceipt, PublishTarget};
pub use registry::{format_peer_spec, PeerSpec, PeerSpecError};
pub use registry_refresh::{
    run_loop as run_registry_refresh_loop, sync_once as registry_sync_once, GateBlock,
    RegistryRefreshConfig, RegistryRefreshGate, SyncOutcome, TickReport as RegistrySyncReport,
};
pub use room::Room;
pub use route::{
    endpoints_from_json, endpoints_to_json, ImportedInvite, InviteBeacon, PeerDialFailure,
    RouteClass, RouteDecision, RouteDiscoverySnapshot, RouteEndpoint, RoutePolicy,
    TransportCandidate, TransportHealthSample, TransportHealthState, TransportHealthTable,
    TransportKind, TransportResolver, TransportRole, TransportRoute,
};
pub use stream::{EventFilter, EventStream, FilteredEventStream, LiveLag};
pub use subscriptions::{
    derive_room_id, ChannelName, ChannelNameError, MeshIdentity, Subscription, SubscriptionError,
    SubscriptionSet,
};
pub use task_negotiation::{
    HEADER_AIRC_TASK_ACCEPTED, HEADER_AIRC_TASK_OFFER, HEADER_AIRC_TASK_REQUEST,
};
pub use webrtc_media::{
    IncomingTrack, IncomingTrackHandler, OpenedWebRtcConnection, OutgoingAudioTrack,
    OutgoingSampleTrack, OutgoingVideoTrack, WebRtcConnectionBuilder, WebRtcMediaCodec,
};
pub use work::{
    AllocateWorkspace, ChangeWorkCardState, ChangeWorkLaneState, ClaimManagerHat, ClaimWorkCard,
    ClaimableWorkItem, ClaimableWorkQuery, CreateWorkCard, CreateWorkLane, HeartbeatWorkClaim,
    HeartbeatWorkspace, LinkCardPullRequest, MarkPullRequestMerged, ObserveLocalGitWorkspace,
    ObservePullRequests, ObservedLocalGitWorkspace, ObservedPullRequests, ReleaseManagerHat,
    ReleaseWorkClaim, ReleaseWorkspace, ReportAgentAvailability, RequestWorkspace, UpdateWorkCard,
    WorkQueueStatus, WorkQueueStatusQuery, WORK_BOARD_PROJECTION_PAGE_SIZE,
};
pub use work_manager::{
    SeededWorkCard, WorkBacklogSeedCandidate, WorkBacklogSeedOutcome, WorkBacklogSeedResult,
    WorkManagerAgent, WorkManagerQuery, WorkManagerReason, WorkManagerRecommendation,
    WorkManagerRecommendationKind, WorkManagerStatus,
};
pub use work_roster::{WorkRosterQuery, WorkRosterRow, WorkRosterStatus};
pub use work_subscription::{
    WorkEventFilter, HEADER_WORK_CARD_ID, HEADER_WORK_LANE_ID, HEADER_WORK_REPO,
};

// Convenience re-exports so consumers don't need to pull airc-core
// just to type the common return values.
pub use airc_core::{
    body::Body,
    headers::{HeaderFilter, Headers},
    transcript::MentionTarget,
    ClientId, EventId, PeerId, PersonaCapabilities, PersonaCapabilitiesError, RoomId,
    TranscriptCursor, TranscriptEvent, TranscriptKind, PERSONA_CAPABILITIES_KEY,
};
// Trust tiers are the capability-registry ranking axis (card a9580f9d);
// re-export so consumers ranking candidates don't pull airc-store.
pub use airc_store::peer_trust::TrustTier;
pub use airc_work::{
    AgentAvailabilityRecord, AgentAvailabilityReported, AgentAvailabilityState, BoardSnapshot,
    BranchName, CardState, CardUpdated, ClaimId, DirtyState, GitObjectId, LaneId, LaneState,
    LocalGitSnapshot, ManagerHat, Priority, ProjectionError, RepoId, WorkBoardProjection, WorkCard,
    WorkCardId, WorkEvent, WorkspaceId, WorkspaceStatus, BODY_HINT_FORGE_WORK_EVENT,
    HEADER_FORGE_WORK_EVENT_KIND, HEADER_FORGE_WORK_REPO,
};
