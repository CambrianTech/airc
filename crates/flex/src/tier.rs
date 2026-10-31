//! A tier: one resource's measured capacity, the grants its holders own, and each clearing that
//! moves them. Targets are decided on the whole capacity; growth toward them is committed only from
//! capacity no one physically holds, and only in whole blocks. A shrink is a request: what it frees
//! is charged until the holder reports its release.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::amount::{Amount, Unit};
use crate::claim::{Claim, ClaimError, ClaimId, Floor, OwnerId};
use crate::provenance::Provenance;
use crate::solve::{committable, next_value, on_boundary, solve, Shape};
use crate::value::{NonFinite, Price, Value};

/// What a tier can grant, as measured. `Unavailable` clears nothing: an unknown capacity is never a
/// default.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Capacity<U: Unit> {
    Measured {
        amount: Amount<U>,
        provenance: Provenance,
    },
    Unavailable,
}

/// Where one claim stands after a clearing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum GrantState<U: Unit> {
    /// It owns its target.
    Granted,
    /// Its target differed from what it owns by less value than its switch cost: nothing moved.
    Held,
    /// It owns more than its target and is asked to release down to `to`. The difference stays
    /// charged to it until it reports the release.
    ShrinkRequested { to: Amount<U> },
    /// Its next whole block is not free yet: others hold it and have been asked to release.
    Waiting,
    /// Its values are unmeasured: it owns its reservation only.
    Unmeasured,
}

/// One claim's grant after a clearing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Grant<U: Unit> {
    pub claim: ClaimId,
    pub owner: Option<OwnerId>,
    /// What the clearing decided it should hold.
    pub target: Amount<U>,
    /// What it may use now: physical ownership, charged until it reports a release.
    pub owned: Amount<U>,
    pub state: GrantState<U>,
    /// The tier's generation when this grant was made; a commitment presented with an older one is
    /// refused ([`Tier::is_current`]).
    pub generation: u64,
    /// The custody this grant belongs to: one incarnation of this claim's holding. A holding that
    /// ends (released to nothing, or withdrawn) and starts again is a new lease.
    pub lease: LeaseId,
    /// Rises each time the holding's charge grows. Within one epoch the holder only ever releases,
    /// so a report from this epoch can never claim less than it physically holds.
    pub epoch: u64,
}

/// One incarnation of a claim's holding.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct LeaseId(pub u64);

/// A holder's report of what it physically holds, naming the custody (lease and epoch) it describes.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Release<U: Unit> {
    pub claim: ClaimId,
    pub lease: LeaseId,
    pub epoch: u64,
    pub holds: Amount<U>,
}

/// How a clearing ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Outcome<U: Unit> {
    Cleared,
    /// The required floors alone exceed capacity. Nothing moved and no grant was issued; the named
    /// claims need scheduling, placement or a smaller footprint, never an oversubscription.
    Infeasible {
        required: Amount<U>,
        capacity: Amount<U>,
        claims: Vec<ClaimId>,
    },
    /// Capacity is not measured. Nothing moved.
    CapacityUnavailable,
    /// A value overflowed past what a finite number holds. Nothing moved.
    NonFinite,
}

/// One clearing's receipt: everything needed to replay and audit it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Clearing<U: Unit> {
    pub generation: u64,
    pub capacity: Capacity<U>,
    pub outcome: Outcome<U>,
    /// Every claim and every current holder, by claim id.
    pub grants: Vec<Grant<U>>,
    /// The scarcity price: what one more unit of capacity would buy, the value per unit of the most
    /// valuable demand left unmet. Zero when every claim's demand was met.
    pub price: Price<U>,
    /// Capacity no target asked for.
    pub unallocated: Amount<U>,
    /// How far what holders physically own exceeds the measured capacity (capacity fell below what
    /// was committed: the person's game took memory). Nothing grows while this is set; shrinks are
    /// issued; free capacity is never negative.
    pub overcommitted: Option<Amount<U>>,
    /// The value the targets achieve past the reservations: a feasible lower bound on the optimum.
    pub achieved: Value,
    /// The value with every indivisible block relaxed to divisible: an upper bound on the optimum.
    /// No optimality is claimed when they differ; the difference is the gap.
    pub upper_bound: Value,
    /// Claims refused before clearing, and why.
    pub refused: Vec<(ClaimId, ClaimError)>,
}

/// Why a release report was refused.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum ReleaseError<U: Unit> {
    /// No such holder.
    Unknown,
    /// It reported holding more than it owns: a release can only lower what is charged.
    AboveOwned {
        owned: Amount<U>,
        reported: Amount<U>,
    },
    /// The report or termination names another incarnation of this claim's holding (a delayed
    /// message from a holder that has since ended and been re-created).
    WrongLease { current: LeaseId, named: LeaseId },
    /// The report describes an epoch the holding has grown past: it may claim less than is now
    /// physically held, so it has no effect (a late or replayed report after regrowth).
    Superseded { current: u64, named: u64 },
    /// The report names an epoch that was never issued.
    NotIssued { current: u64, named: u64 },
}

#[derive(Clone, Copy, Debug)]
struct Holding<U: Unit> {
    owner: Option<OwnerId>,
    target: Amount<U>,
    owned: Amount<U>,
    state: GrantState<U>,
    generation: u64,
    lease: LeaseId,
    epoch: u64,
}

/// One claim's place in a planned clearing.
struct Step<U: Unit> {
    target: Amount<U>,
    owned: Amount<U>,
    state: GrantState<U>,
}

/// A clearing decided in full before anything is mutated, so a refused input moves nothing.
struct Plan<U: Unit> {
    steps: Vec<Step<U>>,
    price: Price<U>,
    unallocated: Amount<U>,
    overcommitted: Option<Amount<U>>,
    achieved: Value,
    upper_bound: Value,
}

/// A clearing's targets, aligned with the claims it was given.
struct Targets<U: Unit> {
    amounts: Vec<Amount<U>>,
    held: Vec<ClaimId>,
    price: Price<U>,
    unallocated: Amount<U>,
}

/// One resource's grants across clearings.
#[derive(Debug)]
pub struct Tier<U: Unit> {
    generation: u64,
    holdings: BTreeMap<ClaimId, Holding<U>>,
    next_lease: u64,
}

impl<U: Unit> Default for Tier<U> {
    fn default() -> Self {
        Self {
            generation: 0,
            holdings: BTreeMap::new(),
            next_lease: 0,
        }
    }
}

impl<U: Unit> Tier<U> {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    /// Clear `claims` against `capacity` and move the grants. A holder that presents no claim is
    /// asked to release everything.
    pub fn clear(&mut self, capacity: Capacity<U>, claims: &[Claim<U>]) -> Clearing<U> {
        let mut refused = Vec::new();
        let mut valid: Vec<&Claim<U>> = Vec::new();
        let mut seen: BTreeMap<ClaimId, usize> = BTreeMap::new();
        for claim in claims {
            *seen.entry(claim.id).or_insert(0) += 1;
        }
        for claim in claims {
            if seen.get(&claim.id).is_some_and(|&n| n > 1) {
                // Two claims under one id would overwrite each other's custody: neither is cleared.
                refused.push((claim.id, ClaimError::DuplicateId));
                continue;
            }
            let held_by = self.holdings.get(&claim.id).and_then(|h| h.owner);
            let custody = match held_by {
                Some(holder) if holder != claim.owner => Err(ClaimError::OwnerChanged { holder }),
                _ => Ok(()),
            };
            match claim.validate().and(custody) {
                Ok(()) => valid.push(claim),
                Err(error) => refused.push((claim.id, error)),
            }
        }
        valid.sort_by_key(|c| c.id);
        refused.sort_by_key(|(id, _)| *id);
        let Capacity::Measured {
            amount: capacity_amount,
            ..
        } = capacity
        else {
            return self.unmoved(capacity, Outcome::CapacityUnavailable, refused);
        };
        let required: Amount<U> = valid.iter().map(|c| c.reservation()).sum();
        if required > capacity_amount {
            let claims = valid
                .iter()
                .filter(|c| matches!(c.floor, Floor::Required(_)))
                .map(|c| c.id)
                .collect();
            return self.unmoved(
                capacity,
                Outcome::Infeasible {
                    required,
                    capacity: capacity_amount,
                    claims,
                },
                refused,
            );
        }
        let Ok(plan) = self.plan(capacity_amount, &valid) else {
            return self.unmoved(capacity, Outcome::NonFinite, refused);
        };
        self.generation += 1;
        let generation = self.generation;
        for holding in self.holdings.values_mut() {
            // A holder that no longer claims anything is asked to release everything; a claimant is
            // overwritten below.
            holding.target = Amount::ZERO;
            holding.state = GrantState::ShrinkRequested { to: Amount::ZERO };
            holding.generation = generation;
        }
        for (claim, step) in valid.iter().zip(plan.steps) {
            let (lease, epoch, was) = match self.holdings.get(&claim.id) {
                Some(h) => (h.lease, h.epoch, h.owned),
                None => {
                    self.next_lease += 1;
                    (LeaseId(self.next_lease), 0, Amount::ZERO)
                }
            };
            // Growth starts a new epoch: reports written before it can no longer lower the charge.
            let epoch = if step.owned > was { epoch + 1 } else { epoch };
            let holding = Holding {
                owner: Some(claim.owner),
                target: step.target,
                owned: step.owned,
                state: step.state,
                generation,
                lease,
                epoch,
            };
            self.holdings.insert(claim.id, holding);
        }
        Clearing {
            generation,
            capacity,
            outcome: Outcome::Cleared,
            grants: self.grants(),
            price: plan.price,
            unallocated: plan.unallocated,
            overcommitted: plan.overcommitted,
            achieved: plan.achieved,
            upper_bound: plan.upper_bound,
            refused,
        }
    }

    /// The holder of `claim` reports what it physically holds now, at or below its target as it
    /// likes. A report only ever lowers what is charged, is idempotent, and is not fenced by the
    /// clearing generation: a release can never
    /// overcommit, and a later clearing must not refuse a real release made earlier.
    pub fn report_held(&mut self, report: Release<U>) -> Result<Amount<U>, ReleaseError<U>> {
        let Release {
            claim,
            lease,
            epoch,
            holds,
        } = report;
        let holding = self.holdings.get_mut(&claim).ok_or(ReleaseError::Unknown)?;
        if lease != holding.lease {
            return Err(ReleaseError::WrongLease {
                current: holding.lease,
                named: lease,
            });
        }
        if epoch < holding.epoch {
            return Err(ReleaseError::Superseded {
                current: holding.epoch,
                named: epoch,
            });
        }
        if epoch > holding.epoch {
            return Err(ReleaseError::NotIssued {
                current: holding.epoch,
                named: epoch,
            });
        }
        if holds > holding.owned {
            return Err(ReleaseError::AboveOwned {
                owned: holding.owned,
                reported: holds,
            });
        }
        let released = holding.owned.saturating_sub(holds);
        holding.owned = holds;
        if holds == holding.target && matches!(holding.state, GrantState::ShrinkRequested { .. }) {
            holding.state = GrantState::Granted;
        } else if holds < holding.target {
            holding.state = GrantState::Waiting;
        }
        if holds == Amount::ZERO && holding.target == Amount::ZERO {
            self.holdings.remove(&claim);
        }
        Ok(released)
    }

    /// A holder known to be gone (its process exited or crashed) is withdrawn: everything it owned is
    /// free, whatever it was asked to release. Without this, a holder that disappears after a shrink
    /// request strands its units forever (Cormac on #5620).
    pub fn withdraw(
        &mut self,
        claim: ClaimId,
        lease: LeaseId,
    ) -> Result<Amount<U>, ReleaseError<U>> {
        let holding = self.holdings.get(&claim).ok_or(ReleaseError::Unknown)?;
        if lease != holding.lease {
            // A delayed termination of an earlier incarnation never ends the current one.
            return Err(ReleaseError::WrongLease {
                current: holding.lease,
                named: lease,
            });
        }
        let owned = holding.owned;
        self.holdings.remove(&claim);
        Ok(owned)
    }

    fn owned_now(&self) -> Amount<U> {
        self.holdings.values().map(|h| h.owned).sum()
    }

    /// How far physical ownership exceeds `capacity`, when it does.
    fn overcommitted(&self, capacity: Amount<U>) -> Option<Amount<U>> {
        let owned = self.owned_now();
        (owned > capacity).then(|| owned.saturating_sub(capacity))
    }

    /// Whether a commitment presented at `generation` for `claim` is current: growth on an older
    /// decision is refused.
    pub fn is_current(&self, claim: ClaimId, generation: u64) -> bool {
        self.holdings
            .get(&claim)
            .is_some_and(|h| h.generation == generation)
    }

    fn plan(&self, capacity: Amount<U>, claims: &[&Claim<U>]) -> Result<Plan<U>, NonFinite> {
        let Targets {
            amounts,
            held,
            price,
            unallocated,
        } = self.targets(capacity, claims)?;
        let unmeasured_reserved: Amount<U> = claims
            .iter()
            .filter(|c| !c.provenance.is_measured())
            .map(|c| c.reservation())
            .sum();
        let tradeable: Vec<&Claim<U>> = claims
            .iter()
            .copied()
            .filter(|c| c.provenance.is_measured())
            .collect();
        let upper_bound = solve(
            capacity.saturating_sub(unmeasured_reserved),
            &tradeable,
            Shape::Relaxed,
        )?
        .achieved;
        let owned_now = self.owned_now();
        let overcommitted = self.overcommitted(capacity);
        // Physical ownership is charged first: growth comes only from what no one holds, and when
        // holders own more than the capacity there is nothing free (never a negative free).
        let mut free = capacity.saturating_sub(owned_now);
        let mut achieved = Value::ZERO;
        let mut steps: Vec<Step<U>> = Vec::with_capacity(claims.len());
        let mut growing: Vec<usize> = Vec::new();
        for (slot, (claim, target)) in claims.iter().zip(amounts).enumerate() {
            if claim.provenance.is_measured() {
                achieved = Value::new(achieved.raw() + claim.value_at(target)?.raw())?;
            }
            let owned = self
                .holdings
                .get(&claim.id)
                .map_or(Amount::ZERO, |h| h.owned);
            let state = if held.contains(&claim.id) {
                GrantState::Held
            } else if target < owned {
                GrantState::ShrinkRequested { to: target }
            } else {
                growing.push(slot);
                GrantState::Waiting
            };
            let target = if held.contains(&claim.id) {
                owned
            } else {
                target
            };
            steps.push(Step {
                target,
                owned,
                state,
            });
        }
        // Free capacity goes first to unmet required floors, then to the most valuable next block,
        // ties by claim id: who waits longest for a release is decided by value, never by arrival.
        let mut keyed = Vec::with_capacity(growing.len());
        for &slot in &growing {
            let claim = claims[slot];
            let owned = steps[slot].owned;
            keyed.push((
                owned < claim.reservation(),
                next_value(claim, owned)?,
                claim.id,
                slot,
            ));
        }
        keyed.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then_with(|| b.1.total_cmp(&a.1))
                .then_with(|| a.2.cmp(&b.2))
        });
        for (_, _, _, slot) in keyed {
            let claim = claims[slot];
            let step = &mut steps[slot];
            let growth = committable(claim, step.owned, step.target, free)?;
            free = free.saturating_sub(growth);
            step.owned = step.owned.saturating_add(growth);
            step.state = match (step.owned == step.target, claim.provenance.is_measured()) {
                (false, _) => GrantState::Waiting,
                (true, true) => GrantState::Granted,
                (true, false) => GrantState::Unmeasured,
            };
        }
        Ok(Plan {
            steps,
            price,
            unallocated,
            overcommitted,
            achieved,
            upper_bound,
        })
    }

    /// Targets with hysteresis, aligned with `claims`: a claim whose target differs from what it
    /// owns by less value than its switch cost keeps what it owns, as a fixed amount, and the others
    /// re-clear around it. A hold is applied only when the held amounts plus every reservation fit.
    fn targets(&self, capacity: Amount<U>, claims: &[&Claim<U>]) -> Result<Targets<U>, NonFinite> {
        // Unmeasured claims get their reservation and nothing traded; every other amount starts
        // from its reservation too, and the clearing fills it in.
        let mut amounts: Vec<Amount<U>> = claims.iter().map(|c| c.reservation()).collect();
        let tradeable: Vec<usize> = (0..claims.len())
            .filter(|&i| claims[i].provenance.is_measured())
            .collect();
        let unmeasured_reserved: Amount<U> = claims
            .iter()
            .filter(|c| !c.provenance.is_measured())
            .map(|c| c.reservation())
            .sum();
        let room = capacity.saturating_sub(unmeasured_reserved);
        let pick = |slots: &[usize]| slots.iter().map(|&i| claims[i]).collect::<Vec<_>>();
        let first = solve(room, &pick(&tradeable), Shape::AsClaimed)?;
        let mut held: Vec<(usize, Amount<U>)> = Vec::new();
        for (&i, target) in tradeable.iter().zip(&first.targets) {
            let claim = claims[i];
            let owned = self
                .holdings
                .get(&claim.id)
                .map_or(Amount::ZERO, |h| h.owned);
            // A hold never keeps a holder partway into a block that may not be split, the
            // reservation included, so never below its own required floor (Codex on airc#1630).
            if owned == Amount::ZERO || owned == *target || !on_boundary(claim, owned)? {
                continue;
            }
            let gain = (claim.value_at(*target)?.raw() - claim.value_at(owned)?.raw()).abs();
            if gain <= claim.switch_cost.raw() {
                held.push((i, owned));
            }
        }
        let held_amount: Amount<U> = held.iter().map(|(_, owned)| *owned).sum();
        let others: Vec<usize> = tradeable
            .iter()
            .copied()
            .filter(|i| !held.iter().any(|(h, _)| h == i))
            .collect();
        let others_reserved: Amount<U> = others.iter().map(|&i| claims[i].reservation()).sum();
        // A hold that would overcommit the capacity is not applied this time.
        let holds_fit = held_amount.saturating_add(others_reserved) <= room;
        let (solution, cleared, held) = if held.is_empty() || !holds_fit {
            (first, tradeable, Vec::new())
        } else {
            (
                solve(
                    room.saturating_sub(held_amount),
                    &pick(&others),
                    Shape::AsClaimed,
                )?,
                others,
                held,
            )
        };
        for (&i, target) in cleared.iter().zip(&solution.targets) {
            amounts[i] = *target;
        }
        for &(i, owned) in &held {
            amounts[i] = owned;
        }
        Ok(Targets {
            amounts,
            held: held.into_iter().map(|(i, _)| claims[i].id).collect(),
            price: solution.price,
            unallocated: solution.unallocated,
        })
    }

    fn unmoved(
        &self,
        capacity: Capacity<U>,
        outcome: Outcome<U>,
        refused: Vec<(ClaimId, ClaimError)>,
    ) -> Clearing<U> {
        Clearing {
            generation: self.generation,
            capacity,
            outcome,
            grants: self.grants(),
            price: Price::per_unit(Value::ZERO),
            unallocated: Amount::ZERO,
            overcommitted: match capacity {
                Capacity::Measured { amount, .. } => self.overcommitted(amount),
                Capacity::Unavailable => None,
            },
            achieved: Value::ZERO,
            upper_bound: Value::ZERO,
            refused,
        }
    }

    fn grants(&self) -> Vec<Grant<U>> {
        self.holdings
            .iter()
            .map(|(id, h)| Grant {
                claim: *id,
                owner: h.owner,
                target: h.target,
                owned: h.owned,
                state: h.state,
                generation: h.generation,
                lease: h.lease,
                epoch: h.epoch,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
