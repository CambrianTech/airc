//! The pure clearing: reservations first, then the next unit of highest value per unit, until
//! capacity is spent or no further unit is worth anything.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::amount::{Amount, Unit};
use crate::claim::{Claim, ClaimId, Floor, Increment};
use crate::value::{NonFinite, Price, Value};

/// Each claim's target (its reservation plus what it was granted past it), in the order the claims
/// were given, the price, and what no claim wanted.
pub(crate) struct Solution<U: Unit> {
    pub targets: Vec<Amount<U>>,
    pub price: Price<U>,
    pub unallocated: Amount<U>,
}

/// One candidate: a claim's next block, ordered by value per unit, then by the LOWER claim id
/// (ties are deterministic whatever order the claims arrived in).
struct Candidate<U: Unit> {
    per_unit: Value,
    claim: ClaimId,
    slot: usize,
    index: usize,
    block: Increment<U>,
}

impl<U: Unit> PartialEq for Candidate<U> {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == Ordering::Equal
    }
}

impl<U: Unit> Eq for Candidate<U> {}

impl<U: Unit> PartialOrd for Candidate<U> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl<U: Unit> Ord for Candidate<U> {
    fn cmp(&self, other: &Self) -> Ordering {
        self.per_unit
            .total_cmp(&other.per_unit)
            .then_with(|| other.claim.cmp(&self.claim))
    }
}

/// A claim's blocks past its reservation: an optional floor joins its leading increments into one
/// indivisible block (below the floor the claim takes nothing), priced at their average, which is
/// never below the next increment's value per unit because the increments are concave.
fn blocks<U: Unit>(claim: &Claim<U>) -> Result<Vec<Increment<U>>, NonFinite> {
    let Floor::Optional(floor) = claim.floor else {
        return Ok(claim.increments.clone());
    };
    if floor == Amount::ZERO {
        return Ok(claim.increments.clone());
    }
    let mut out = Vec::with_capacity(claim.increments.len());
    let mut joined = Amount::ZERO;
    let mut rest = Vec::new();
    for increment in &claim.increments {
        if joined < floor {
            let take = increment.width.min(floor.saturating_sub(joined));
            joined = joined.saturating_add(take);
            if take < increment.width {
                rest.push(Increment { width: increment.width.saturating_sub(take), ..*increment });
            }
        } else {
            rest.push(*increment);
        }
    }
    if joined < floor {
        // The claim's increments never reach its floor: it takes nothing.
        return Ok(Vec::new());
    }
    let value = claim.value_of(joined)?;
    let per_unit = Value::new(value.raw() / joined.raw() as f64)?;
    out.push(Increment { width: joined, per_unit, divisible: false });
    out.extend(rest);
    Ok(out)
}

/// Clear `claims` against `capacity`, the reservations already known to fit.
pub(crate) fn solve<U: Unit>(capacity: Amount<U>, claims: &[&Claim<U>]) -> Result<Solution<U>, NonFinite> {
    let reserved: Amount<U> = claims.iter().map(|c| c.reservation()).sum();
    let mut free = capacity.saturating_sub(reserved);
    let mut targets: Vec<Amount<U>> = claims.iter().map(|c| c.reservation()).collect();
    let all_blocks = claims.iter().map(|&c| blocks(c)).collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::new();
    for (slot, (claim, blocks)) in claims.iter().zip(&all_blocks).enumerate() {
        if let Some(first) = blocks.first() {
            heap.push(Candidate { per_unit: first.per_unit, claim: claim.id, slot, index: 0, block: *first });
        }
    }
    let mut last_price = None;
    while let Some(next) = heap.pop() {
        if free == Amount::ZERO || next.per_unit.raw() <= 0.0 {
            break;
        }
        let take = if next.block.width <= free {
            next.block.width
        } else if next.block.divisible {
            free
        } else {
            // An indivisible block that does not fit ends this claim (its blocks are a prefix).
            continue;
        };
        free = free.saturating_sub(take);
        last_price = Some(next.per_unit);
        if let Some(target) = targets.get_mut(next.slot) {
            *target = target.saturating_add(take);
        }
        if take == next.block.width {
            if let Some(block) = all_blocks.get(next.slot).and_then(|b| b.get(next.index + 1)) {
                heap.push(Candidate { index: next.index + 1, per_unit: block.per_unit, block: *block, ..next });
            }
        }
    }
    // Slack capacity is worth nothing at the margin: the price is zero unless capacity ran out.
    let price = match (free == Amount::ZERO, last_price) {
        (true, Some(per_unit)) => Price::per_unit(per_unit),
        _ => Price::per_unit(Value::ZERO),
    };
    Ok(Solution { targets, price, unallocated: free })
}
