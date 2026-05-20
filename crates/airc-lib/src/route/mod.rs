//! Routing, discovery, and invite/rendezvous model.
//!
//! This subsystem owns route policy, health, endpoint discovery,
//! invite beacons, resolver selection, and execution of selected
//! routes. App and CLI layers consume this through `Airc`; they do
//! not construct transport adapters or route frames themselves.

pub mod health;
pub mod invite;
pub mod policy;
pub mod resolver;

pub(crate) mod execution;

pub use health::{TransportHealthSample, TransportHealthState, TransportHealthTable};
pub use invite::{InviteBeacon, RouteEndpoint};
pub use policy::{
    RouteClass, RouteDecision, RoutePolicy, TransportCandidate, TransportKind, TransportRole,
};
pub use resolver::{TransportResolver, TransportRoute};
