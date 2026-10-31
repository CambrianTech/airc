//! The pure clearing: reservations first, then the next unit of highest value per unit, until
//! capacity is spent or no further unit is worth anything.

use std::cmp::Ordering;
use std::collections::BinaryHeap;

use crate::amount::{Amount, Unit};
use crate::claim::{Claim, ClaimId, Floor, Increment};
use crate::value::{NonFinite, Price, Value};

/// Each claim's target (its reservation plus what it was granted past it), in the order the claims
/// were given; the scarcity price; what no claim wanted; and the value achieved past the
/// reservations.
pub(crate) struct Solution<U: Unit> {
    pub targets: Vec<Amount<U>>,
    pub price: Price<U>,
    pub unallocated: Amount<U>,
    pub achieved: Value,
}

/// Whether indivisible blocks keep their shape (the clearing) or are relaxed to divisible (the upper
/// bound on the optimum that the clearing's achieved value is reported against).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Shape {
    AsClaimed,
    Relaxed,
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
pub(crate) fn blocks<U: Unit>(claim: &Claim<U>) -> Result<Vec<Increment<U>>, NonFinite> {
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
            // A divisible increment gives only what the floor still needs; an indivisible one is
            // taken whole, so the joined block may pass the floor (Codex on airc#1630: an optional
            // floor of 3 over an indivisible 10 must never split it).
            let take = if increment.divisible {
                increment.width.min(floor.saturating_sub(joined))
            } else {
                increment.width
            };
            joined = joined.saturating_add(take);
            if take < increment.width {
                rest.push(Increment {
                    width: increment.width.saturating_sub(take),
                    ..*increment
                });
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
    out.push(Increment {
        width: joined,
        per_unit,
        divisible: false,
    });
    out.extend(rest);
    Ok(out)
}

/// Clear `claims` against `capacity`, the reservations already known to fit.
pub(crate) fn solve<U: Unit>(
    capacity: Amount<U>,
    claims: &[&Claim<U>],
    shape: Shape,
) -> Result<Solution<U>, NonFinite> {
    let reserved: Amount<U> = claims.iter().map(|c| c.reservation()).sum();
    let mut free = capacity.saturating_sub(reserved);
    let mut targets: Vec<Amount<U>> = claims.iter().map(|c| c.reservation()).collect();
    let all_blocks = claims
        .iter()
        .map(|&c| {
            blocks(c).map(|blocks| match shape {
                Shape::AsClaimed => blocks,
                Shape::Relaxed => blocks
                    .into_iter()
                    .map(|b| Increment {
                        divisible: true,
                        ..b
                    })
                    .collect(),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut heap = BinaryHeap::new();
    for (slot, (claim, blocks)) in claims.iter().zip(&all_blocks).enumerate() {
        if let Some(first) = blocks.first() {
            heap.push(Candidate {
                per_unit: first.per_unit,
                claim: claim.id,
                slot,
                index: 0,
                block: *first,
            });
        }
    }
    let mut achieved = Value::ZERO;
    // The most valuable demand left unmet: what one more unit of capacity would buy. Zero when
    // every claim's demand is met.
    let mut unmet = Value::ZERO;
    let note_unmet = |unmet: &mut Value, per_unit: Value| {
        if per_unit.total_cmp(unmet).is_gt() {
            *unmet = per_unit;
        }
    };
    while let Some(next) = heap.pop() {
        if next.per_unit.raw() <= 0.0 {
            break;
        }
        if free == Amount::ZERO {
            note_unmet(&mut unmet, next.per_unit);
            break;
        }
        let take = if next.block.width <= free {
            next.block.width
        } else if next.block.divisible {
            free
        } else {
            // An indivisible block that does not fit ends this claim (its blocks are a prefix), and
            // is demand left unmet.
            note_unmet(&mut unmet, next.per_unit);
            continue;
        };
        free = free.saturating_sub(take);
        achieved = Value::new(achieved.raw() + next.per_unit.times(take.raw())?.raw())?;
        if let Some(target) = targets.get_mut(next.slot) {
            *target = target.saturating_add(take);
        }
        if take < next.block.width {
            note_unmet(&mut unmet, next.per_unit);
        } else if let Some(block) = all_blocks
            .get(next.slot)
            .and_then(|b| b.get(next.index + 1))
        {
            heap.push(Candidate {
                index: next.index + 1,
                per_unit: block.per_unit,
                block: *block,
                ..next
            });
        }
    }
    Ok(Solution {
        targets,
        price: Price::per_unit(unmet),
        unallocated: free,
        achieved,
    })
}

/// How much of the way from `owned` toward `target` can be committed from `free` without splitting
/// an indivisible block: a required floor, an optional floor and every indivisible level are
/// acquired whole or not at all; a divisible block may be taken in part.
pub(crate) fn committable<U: Unit>(
    claim: &Claim<U>,
    owned: Amount<U>,
    target: Amount<U>,
    free: Amount<U>,
) -> Result<Amount<U>, NonFinite> {
    let reservation = claim.reservation();
    let mut segments = Vec::new();
    if reservation > Amount::ZERO {
        segments.push(Increment {
            width: reservation,
            per_unit: Value::ZERO,
            divisible: false,
        });
    }
    segments.extend(blocks(claim)?);
    let mut start = Amount::ZERO;
    let mut reach = owned;
    let mut left = free;
    for segment in segments {
        let end = start.saturating_add(segment.width);
        if end <= owned {
            start = end;
            continue;
        }
        if reach >= target || left == Amount::ZERO {
            break;
        }
        let stop = end.min(target);
        let need = stop.saturating_sub(reach);
        if need <= left {
            left = left.saturating_sub(need);
            reach = stop;
        } else if segment.divisible {
            reach = reach.saturating_add(left);
            left = Amount::ZERO;
        } else {
            break;
        }
        start = end;
    }
    Ok(reach.saturating_sub(owned))
}

/// The value per unit of the block a claim holding `owned` would acquire next; zero when it wants
/// nothing more.
pub(crate) fn next_value<U: Unit>(claim: &Claim<U>, owned: Amount<U>) -> Result<Value, NonFinite> {
    let mut start = claim.reservation();
    if owned < start {
        return Ok(Value::ZERO);
    }
    for block in blocks(claim)? {
        let end = start.saturating_add(block.width);
        if owned < end {
            return Ok(block.per_unit);
        }
        start = end;
    }
    Ok(Value::ZERO)
}

/// Whether a holding of `owned` ends on one of the claim's block boundaries (its reservation, or the
/// reservation plus whole blocks): only there can it be held without splitting a block.
pub(crate) fn on_boundary<U: Unit>(claim: &Claim<U>, owned: Amount<U>) -> Result<bool, NonFinite> {
    let mut at = claim.reservation();
    if owned == at {
        return Ok(true);
    }
    for block in blocks(claim)? {
        at = at.saturating_add(block.width);
        if owned == at {
            return Ok(true);
        }
        if owned < at {
            // Inside this block: a boundary only if the block may be split.
            return Ok(block.divisible);
        }
    }
    Ok(false)
}
