//! The one solver for every grant, below both repos (continuum's ONE-BUDGET slice 1, airc's
//! NETWORK-AUTHORITY): claims cleared by price against measured capacity.
//!
//! - **Targets:** reservations (required floors) first, then the next unit of highest measured value
//!   per unit, decided on the whole capacity. Required floors that cannot fit are `Infeasible`, named,
//!   with nothing moved.
//! - **Price:** the scarcity price, what one more unit of capacity would buy (the most valuable demand
//!   left unmet); zero when every claim's demand is met.
//! - **Ownership:** what a holder physically owns stays charged until it reports what it holds now
//!   (`report_held`) or is withdrawn as gone (`withdraw`). Growth comes only from capacity no one
//!   owns, in whole blocks; a shrink is a request. Capacity measured below what is owned is
//!   reported as overcommitted, and nothing grows.
//! - **Bounds:** no optimality is claimed where indivisible blocks make the problem non-convex; every
//!   clearing reports the achieved value and the relaxed upper bound.
//! - **Determinism:** pure (no I/O, clock or randomness); ties break by [`ClaimId`]; values are
//!   finite and totally ordered, so the same inputs give the same clearing on every machine.

mod amount;
mod claim;
mod interval;
mod joint;
mod pool;
mod provenance;
mod solve;
mod tier;
mod value;

pub use amount::{Amount, Unit};
pub use claim::{Claim, ClaimError, ClaimId, Floor, Increment, OwnerId};
pub use interval::{IntervalClock, IntervalError, LaneCapacity, LaneDemand, LaneInterval, Lanes};
pub use joint::{JointCapacity, JointDemand, JointError};
pub use pool::{
    PoolAmounts, PoolCapacity, PoolError, PoolFitError, PoolId, PoolTopology, PoolViewId,
    TopologyRevision,
};
pub use provenance::{Provenance, SourceId};
pub use tier::{
    Capacity, Clearing, Grant, GrantState, LeaseId, Outcome, Release, ReleaseError, Tier,
};
pub use value::{NonFinite, Price, Value};
