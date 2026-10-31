//! The one solver for every grant, below both repos (continuum's ONE-BUDGET slice 1, airc's
//! NETWORK-AUTHORITY): claims cleared by price against measured capacity.
//!
//! A claim grows while its measured marginal value exceeds its tier's price; the price is the
//! value per unit of the last increment admitted. Required floors are reservations granted before
//! anything is traded. A shrink is a request: the freed amount is granted to no one until its
//! holder acknowledges the release. Every grant is fenced by its tier's generation.
//!
//! Pure: no I/O, no clock, no randomness. The same inputs give the same clearing on every
//! machine (ties break by [`ClaimId`]; values are finite and totally ordered).

mod amount;
mod claim;
mod provenance;
mod solve;
mod tier;
mod value;

pub use amount::{Amount, Unit};
pub use claim::{Claim, ClaimError, ClaimId, Floor, Increment, OwnerId};
pub use provenance::{Provenance, SourceId};
pub use tier::{Capacity, Clearing, Grant, GrantState, Outcome, ReleaseError, Tier};
pub use value::{NonFinite, Price, Value};
