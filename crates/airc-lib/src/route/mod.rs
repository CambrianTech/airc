//! Routing, discovery, and invite/rendezvous model.
//!
//! This subsystem owns route policy, health, endpoint discovery,
//! invite beacons, resolver selection, and execution of selected
//! routes. App and CLI layers consume this through `Airc`; they do
//! not construct transport adapters or route frames themselves.

pub mod delivery_ledger;
pub mod dial_quarantine;
pub mod discovery;
pub mod health;
pub mod invite;
pub mod policy;
pub mod resolver;

pub(crate) mod execution;

pub use dial_quarantine::{
    DialGate, DialQuarantine, DEAD_AFTER_CONSECUTIVE_FAILURES, INITIAL_BACKOFF_MS, MAX_BACKOFF_MS,
};
pub use discovery::{PeerDialFailure, PeerDialSkip, RouteDiscoverySnapshot};
pub use health::{TransportHealthSample, TransportHealthState, TransportHealthTable};
pub use invite::{
    endpoints_from_json, endpoints_to_json, ImportedInvite, InviteBeacon, RouteEndpoint,
};
pub use policy::{
    crypto_mode, CryptoMode, RouteClass, RouteDecision, RoutePolicy, TransportCandidate,
    TransportKind, TransportRole,
};
pub use resolver::{TransportResolver, TransportRoute};
