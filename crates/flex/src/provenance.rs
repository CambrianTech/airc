//! Where a measured input came from, so stale and unmeasured inputs are explicit.

use serde::{Deserialize, Serialize};

/// The measurement that produced an input (a probe, a model, a replay), as an opaque id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct SourceId(pub u128);

/// How an input is known.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Provenance {
    /// Measured by `source` at `at_ms` (the caller's clock), as revision `version` of its method.
    Measured {
        source: SourceId,
        at_ms: u64,
        version: u64,
    },
    /// Measured once, at `last_ms`, and `age_ms` old when this clearing ran.
    Stale {
        source: SourceId,
        last_ms: u64,
        age_ms: u64,
    },
    /// Never measured.
    Unmeasured,
}

impl Provenance {
    /// Whether the input may be traded on: an unmeasured value claims only its floor.
    pub fn is_measured(self) -> bool {
        !matches!(self, Self::Unmeasured)
    }
}
