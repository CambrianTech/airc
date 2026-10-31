//! A claim: what one holder asks of one tier, and what each further unit is worth to it.

use serde::{Deserialize, Serialize};

use crate::amount::{Amount, Unit};
use crate::provenance::Provenance;
use crate::value::{NonFinite, Value};

/// One claim, stable across clearings.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ClaimId(pub u128);

/// Who holds a claim (a mind, a lane, a link), for the receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct OwnerId(pub u128);

/// The least a claim takes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Floor<U: Unit> {
    /// A reservation: granted before anything is traded, never traded. If the required floors
    /// together exceed capacity, the clearing is infeasible and names them.
    Required(Amount<U>),
    /// The least worth having: below it the claim takes nothing. Traded like any increment.
    Optional(Amount<U>),
    /// No floor.
    None,
}

/// The next `width` units of a claim, each worth `per_unit`. A claim's increments are taken in
/// order, and `per_unit` never rises from one to the next (concave: each unit is worth no more than
/// the one before it), which is what makes the greedy order optimal.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Increment<U: Unit> {
    pub width: Amount<U>,
    pub per_unit: Value,
    /// Whether part of it may be granted (output room, a cache) or only all of it (an item's next
    /// level: its summary or its whole).
    pub divisible: bool,
}

/// What one holder asks of one tier.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Claim<U: Unit> {
    pub id: ClaimId,
    pub owner: OwnerId,
    pub floor: Floor<U>,
    /// What each unit past the floor is worth, in order.
    pub increments: Vec<Increment<U>>,
    /// How the values were obtained. An unmeasured claim is granted only its floor.
    pub provenance: Provenance,
    /// The measured cost of changing this claim's grant (a re-prefill, a page move): a change worth
    /// less than this is not made.
    pub switch_cost: Value,
}

/// Why a claim was refused before clearing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum ClaimError {
    /// An increment's value per unit rose above the one before it.
    NotConcave { at: usize },
    /// An increment had no width.
    EmptyIncrement { at: usize },
    /// A value per unit or the switch cost was negative.
    Negative,
    /// The claim id appears more than once in one clearing: none of them is cleared.
    DuplicateId,
    /// The claim id is held by another owner. Custody never changes hands inside a clearing: the
    /// holder is withdrawn first (Codex on airc#1630).
    OwnerChanged { holder: OwnerId },
}

impl<U: Unit> Claim<U> {
    /// The checks the solver's optimality and determinism rest on.
    pub fn validate(&self) -> Result<(), ClaimError> {
        if self.switch_cost.raw() < 0.0 {
            return Err(ClaimError::Negative);
        }
        let mut last: Option<Value> = None;
        for (at, increment) in self.increments.iter().enumerate() {
            if increment.width == Amount::ZERO {
                return Err(ClaimError::EmptyIncrement { at });
            }
            if increment.per_unit.raw() < 0.0 {
                return Err(ClaimError::Negative);
            }
            if last.is_some_and(|prev| increment.per_unit.total_cmp(&prev).is_gt()) {
                return Err(ClaimError::NotConcave { at });
            }
            last = Some(increment.per_unit);
        }
        Ok(())
    }

    /// The reserved amount: a required floor, or nothing.
    pub fn reservation(&self) -> Amount<U> {
        match self.floor {
            Floor::Required(amount) => amount,
            Floor::Optional(_) | Floor::None => Amount::ZERO,
        }
    }

    /// The value of holding `total` (reservation included): what its units past the reservation
    /// are worth.
    pub fn value_at(&self, total: Amount<U>) -> Result<Value, NonFinite> {
        self.value_of(total.saturating_sub(self.reservation()))
    }

    /// The value of holding `amount` past the reservation: the sum over the increments it covers.
    /// `NonFinite` only if that sum overflows, which the caller refuses rather than rounds.
    pub fn value_of(&self, amount: Amount<U>) -> Result<Value, NonFinite> {
        let mut left = amount.raw();
        let mut total = Value::ZERO;
        for increment in &self.increments {
            if left == 0 {
                break;
            }
            let take = left.min(increment.width.raw());
            total = Value::new(total.raw() + increment.per_unit.times(take)?.raw())?;
            left -= take;
        }
        Ok(total)
    }
}
