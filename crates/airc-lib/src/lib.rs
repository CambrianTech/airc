//! `airc-lib` — consumer-facing AIRC API.
//!
//! Closes grievance §5 (CLI/Daemon Accumulating Policy — the needed
//! crate split lists `airc-lib` as the consumer-facing surface) and
//! advances Gate 4 (Consumer Embedding) of the operating doc:
//!
//! > Pass when a small consumer app can link `airc-lib` and:
//! > create/load identity; join a channel; send typed body with
//! > headers; subscribe by header/channel/kind; fetch replay; use
//! > blobs; never shell out.
//!
//! This crate composes the lower-level substrate crates
//! (`airc-core`, `airc-protocol`, `airc-store`, `airc-transport`,
//! `airc-daemon`) into one `Airc` facade so consumers don't
//! reconstruct the wiring on every embedding.
//!
//! Scope of this slice (slice 6): in-process embedding. The handle
//! owns its identity + store + transports directly; no daemon IPC
//! involved. Daemon-attached mode (`Airc::attach(socket)`) is queued
//! for slice 6b along with the subscribe-stream surface.

#![deny(unsafe_code)]

pub mod airc;
pub mod error;
mod lan;
mod messaging;
mod peers;
pub mod registry;
pub mod room;
pub mod route;
mod stream;
mod time;
mod transport;
pub mod work;

pub use airc::Airc;
pub use error::AircError;
pub use peers::EnrolledPeer;
pub use registry::{format_peer_spec, PeerSpec, PeerSpecError};
pub use room::Room;
pub use route::{
    InviteBeacon, RouteClass, RouteDecision, RouteEndpoint, RoutePolicy, TransportCandidate,
    TransportHealthSample, TransportHealthState, TransportHealthTable, TransportKind,
    TransportResolver, TransportRole, TransportRoute,
};
pub use stream::{EventFilter, EventStream, FilteredEventStream, LiveLag};
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
