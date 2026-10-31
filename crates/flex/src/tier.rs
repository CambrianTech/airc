//! A tier: one resource's measured capacity, the grants its holders own, and each clearing that
//! moves them. Targets are decided on the whole capacity; growth toward them is committed only from
//! capacity no one physically holds, and only in whole blocks. A shrink is a request: what it frees
//! is charged until the holder reports its release.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::amount::{Amount, Unit};
use crate::claim::{Claim, ClaimError, ClaimId, Floor, OwnerId};
use crate::provenance::Provenance;
use crate::solve::{committable, next_value, solve, Shape};
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
}

#[derive(Clone, Copy, Debug)]
struct Holding<U: Unit> {
    owner: Option<OwnerId>,
    target: Amount<U>,
    owned: Amount<U>,
    state: GrantState<U>,
    generation: u64,
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
}

impl<U: Unit> Default for Tier<U> {
    fn default() -> Self {
        Self {
            generation: 0,
            holdings: BTreeMap::new(),
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
        for claim in claims {
            match claim.validate() {
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
            let holding = Holding {
                owner: Some(claim.owner),
                target: step.target,
                owned: step.owned,
                state: step.state,
                generation,
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
            achieved: plan.achieved,
            upper_bound: plan.upper_bound,
            refused,
        }
    }

    /// The holder of `claim` reports what it physically holds now. A report only ever lowers what is
    /// charged, is idempotent, and is not fenced by the clearing generation: a release can never
    /// overcommit, and a later clearing must not refuse a real release made earlier.
    pub fn report_held(
        &mut self,
        claim: ClaimId,
        holds: Amount<U>,
    ) -> Result<Amount<U>, ReleaseError<U>> {
        let holding = self.holdings.get_mut(&claim).ok_or(ReleaseError::Unknown)?;
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
        let owned_now: Amount<U> = self.holdings.values().map(|h| h.owned).sum();
        // Physical ownership is charged first: growth comes only from what no one holds.
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
            if owned == Amount::ZERO || owned == *target {
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
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::claim::Increment;
    use crate::provenance::SourceId;

    #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
    struct Tok;
    impl Unit for Tok {}

    fn amount(n: u64) -> Amount<Tok> {
        Amount::new(n)
    }

    fn value(v: f64) -> Value {
        Value::new(v).expect("test: a finite value")
    }

    fn measured() -> Provenance {
        Provenance::Measured {
            source: SourceId(1),
            at_ms: 0,
            version: 1,
        }
    }

    fn capacity(n: u64) -> Capacity<Tok> {
        Capacity::Measured {
            amount: amount(n),
            provenance: measured(),
        }
    }

    fn claim(id: u128, floor: Floor<Tok>, increments: &[(u64, f64, bool)]) -> Claim<Tok> {
        Claim {
            id: ClaimId(id),
            owner: OwnerId(id),
            floor,
            increments: increments
                .iter()
                .map(|&(width, per_unit, divisible)| Increment {
                    width: amount(width),
                    per_unit: value(per_unit),
                    divisible,
                })
                .collect(),
            provenance: measured(),
            switch_cost: Value::ZERO,
        }
    }

    fn grant(clearing: &Clearing<Tok>, id: u128) -> Option<Grant<Tok>> {
        clearing
            .grants
            .iter()
            .find(|g| g.claim == ClaimId(id))
            .copied()
    }

    fn owned(clearing: &Clearing<Tok>, id: u128) -> Amount<Tok> {
        grant(clearing, id).map_or(Amount::ZERO, |g| g.owned)
    }

    fn state(clearing: &Clearing<Tok>, id: u128) -> Option<GrantState<Tok>> {
        grant(clearing, id).map(|g| g.state)
    }

    // what this catches: a solver that splits by share instead of value. Reservations come first,
    // then the unit worth most; the price is what one more unit would buy (A's unmet 2.0).
    #[test]
    fn reservations_first_then_the_most_valuable_unit_at_the_scarcity_price() {
        let a = claim(1, Floor::Required(amount(10)), &[(30, 2.0, true)]);
        let b = claim(2, Floor::None, &[(30, 3.0, true)]);
        let clearing = Tier::new().clear(capacity(50), &[a, b]);
        assert_eq!(clearing.outcome, Outcome::Cleared);
        assert_eq!(
            (owned(&clearing, 1), owned(&clearing, 2)),
            (amount(20), amount(30))
        );
        assert_eq!(clearing.price.value(), value(2.0));
        assert_eq!(
            (clearing.achieved, clearing.upper_bound),
            (value(110.0), value(110.0))
        );
    }

    // what this catches: a positive price published while capacity sits idle with every demand met
    // (Codex on #5620).
    #[test]
    fn a_fully_met_demand_publishes_a_zero_price_idle_or_exact() {
        let slack = Tier::new().clear(capacity(50), &[claim(1, Floor::None, &[(30, 2.0, true)])]);
        assert_eq!(
            (slack.price.value(), slack.unallocated),
            (Value::ZERO, amount(20))
        );
        let exact = Tier::new().clear(capacity(30), &[claim(1, Floor::None, &[(30, 2.0, true)])]);
        assert_eq!(
            (exact.price.value(), exact.unallocated),
            (Value::ZERO, Amount::ZERO)
        );
    }

    // what this catches: oversubscribing when reservations do not fit. Nothing moves, no grant is
    // issued, and the claims that need scheduling are named.
    #[test]
    fn infeasible_required_floors_are_named_and_nothing_moves() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::Required(amount(30)), &[]);
        let b = claim(2, Floor::Required(amount(30)), &[]);
        let clearing = tier.clear(capacity(50), &[a, b]);
        assert_eq!(
            clearing.outcome,
            Outcome::Infeasible {
                required: amount(60),
                capacity: amount(50),
                claims: vec![ClaimId(1), ClaimId(2)]
            }
        );
        assert!(clearing.grants.is_empty());
        assert_eq!(
            tier.generation(),
            0,
            "an infeasible clearing is not a generation"
        );
    }

    // what this catches: trading on a value nobody measured. It gets its reservation only.
    #[test]
    fn an_unmeasured_claim_gets_its_reservation_only() {
        let mut a = claim(1, Floor::Required(amount(5)), &[(40, 9.0, true)]);
        a.provenance = Provenance::Unmeasured;
        let b = claim(2, Floor::None, &[(40, 1.0, true)]);
        let clearing = Tier::new().clear(capacity(50), &[a, b]);
        assert_eq!(
            (owned(&clearing, 1), state(&clearing, 1)),
            (amount(5), Some(GrantState::Unmeasured))
        );
        assert_eq!(owned(&clearing, 2), amount(40));
    }

    // what this catches: a clearing that depends on arrival order or breaks ties arbitrarily, so two
    // machines (or a replay) disagree. Equal value per unit goes to the lower claim id.
    #[test]
    fn the_same_claims_in_any_order_give_the_same_clearing() {
        let claims = [
            claim(3, Floor::None, &[(20, 2.0, true)]),
            claim(1, Floor::None, &[(20, 2.0, true)]),
            claim(
                2,
                Floor::Optional(amount(10)),
                &[(10, 4.0, false), (10, 1.0, true)],
            ),
        ];
        let forward = Tier::new().clear(capacity(35), &claims);
        let mut reversed = claims.to_vec();
        reversed.reverse();
        let backward = Tier::new().clear(capacity(35), &reversed);
        assert_eq!(forward, backward);
        assert_eq!(
            (owned(&forward, 1), owned(&forward, 2), owned(&forward, 3)),
            (amount(20), amount(10), amount(5))
        );
    }

    // what this catches: two holders owning the same unit (Codex's case: A holds, B proposes). What a
    // shrink frees stays charged to A until A reports the release; only then does B grow into it.
    #[test]
    fn physical_ownership_is_charged_until_the_holder_reports_its_release() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::None, &[(40, 1.0, true)]);
        tier.clear(capacity(50), &[a.clone()]);
        let b = claim(2, Floor::None, &[(40, 5.0, true)]);
        let second = tier.clear(capacity(50), &[a.clone(), b.clone()]);
        assert_eq!(
            state(&second, 1),
            Some(GrantState::ShrinkRequested { to: amount(10) })
        );
        assert_eq!(
            owned(&second, 1),
            amount(40),
            "A still owns what it has not released"
        );
        assert_eq!(
            (owned(&second, 2), state(&second, 2)),
            (amount(10), Some(GrantState::Waiting))
        );
        assert_eq!(grant(&second, 2).map(|g| g.target), Some(amount(40)));
        assert_eq!(tier.report_held(ClaimId(1), amount(10)), Ok(amount(30)));
        let third = tier.clear(capacity(50), &[a, b]);
        assert_eq!(
            (owned(&third, 1), owned(&third, 2)),
            (amount(10), amount(40))
        );
        assert_eq!(state(&third, 2), Some(GrantState::Granted));
    }

    // what this catches: a later clearing refusing a real release made after an earlier request,
    // and a report raising what is charged (Codex on airc#1630).
    #[test]
    fn a_release_reported_after_a_later_clearing_is_still_credited() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::None, &[(40, 1.0, true)]);
        tier.clear(capacity(50), &[a.clone()]);
        let b = claim(2, Floor::None, &[(40, 5.0, true)]);
        let second = tier.clear(capacity(50), &[a.clone(), b.clone()]);
        let third = tier.clear(capacity(50), &[a, b]);
        assert_eq!(
            tier.report_held(ClaimId(1), amount(50)),
            Err(ReleaseError::AboveOwned {
                owned: amount(40),
                reported: amount(50)
            })
        );
        assert_eq!(tier.report_held(ClaimId(1), amount(10)), Ok(amount(30)));
        assert_eq!(
            tier.report_held(ClaimId(1), amount(10)),
            Ok(Amount::ZERO),
            "a repeated report is idempotent"
        );
        assert!(tier.is_current(ClaimId(1), third.generation));
        assert!(
            !tier.is_current(ClaimId(1), second.generation),
            "a commitment from an older generation is refused"
        );
    }

    // what this catches: partial custody of a block that is whole or nothing (Codex on airc#1630:
    // capacity 10, A holds 6 and is asked to release, B's optional floor is 10; B must not get 4).
    #[test]
    fn growth_never_commits_part_of_an_optional_floor_or_an_indivisible_level() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::None, &[(6, 1.0, true)]);
        tier.clear(capacity(10), &[a.clone()]);
        let b = claim(2, Floor::Optional(amount(10)), &[(10, 5.0, true)]);
        let second = tier.clear(capacity(10), &[a.clone(), b.clone()]);
        assert_eq!(
            state(&second, 1),
            Some(GrantState::ShrinkRequested { to: Amount::ZERO })
        );
        assert_eq!(
            (owned(&second, 2), state(&second, 2)),
            (Amount::ZERO, Some(GrantState::Waiting))
        );
        assert_eq!(tier.report_held(ClaimId(1), Amount::ZERO), Ok(amount(6)));
        let third = tier.clear(capacity(10), &[a, b]);
        assert_eq!(
            (owned(&third, 2), state(&third, 2)),
            (amount(10), Some(GrantState::Granted))
        );
    }

    // what this catches: thrash. A change worth less than what the move costs is not made.
    #[test]
    fn a_change_worth_less_than_its_switch_cost_is_held() {
        let mut tier = Tier::new();
        let mut a = claim(1, Floor::None, &[(40, 1.0, true)]);
        a.switch_cost = value(100.0);
        tier.clear(capacity(40), &[a.clone()]);
        let b = claim(2, Floor::None, &[(2, 3.0, true)]);
        let held = tier.clear(capacity(40), &[a, b]);
        assert_eq!(state(&held, 1), Some(GrantState::Held));
        assert_eq!(
            (owned(&held, 1), owned(&held, 2)),
            (amount(40), Amount::ZERO)
        );
    }

    // what this catches: holding a change that would overcommit the capacity.
    #[test]
    fn a_hold_that_would_overcommit_is_not_applied() {
        let mut tier = Tier::new();
        let mut a = claim(1, Floor::None, &[(40, 1.0, true)]);
        a.switch_cost = value(100.0);
        tier.clear(capacity(40), &[a.clone()]);
        let b = claim(2, Floor::Required(amount(20)), &[]);
        let clearing = tier.clear(capacity(40), &[a, b]);
        assert_eq!(
            state(&clearing, 1),
            Some(GrantState::ShrinkRequested { to: amount(20) })
        );
        assert_eq!(
            (owned(&clearing, 2), state(&clearing, 2)),
            (Amount::ZERO, Some(GrantState::Waiting))
        );
    }

    // what this catches: claiming optimality where indivisible blocks make the problem non-convex.
    // Greedy takes B's small, dense unit and then cannot fit A's whole floor; the receipt brackets the
    // brute-forced optimum (A alone, worth 10) between what was achieved and the relaxed bound.
    #[test]
    fn a_non_convex_clearing_reports_bounds_that_bracket_the_optimum() {
        let a = claim(1, Floor::Optional(amount(10)), &[(10, 1.0, true)]);
        let b = claim(2, Floor::None, &[(1, 2.0, true)]);
        let clearing = Tier::new().clear(capacity(10), &[a.clone(), b.clone()]);
        let brute = [(0u64, 0u64), (0, 1), (10, 0)]
            .iter()
            .map(|&(x, y)| {
                a.value_of(amount(x)).expect("finite").raw()
                    + b.value_of(amount(y)).expect("finite").raw()
            })
            .fold(f64::MIN, f64::max);
        assert_eq!(clearing.achieved, value(2.0));
        assert!(
            clearing.achieved.raw() <= brute && brute <= clearing.upper_bound.raw(),
            "{clearing:?} vs optimum {brute}"
        );
        assert_eq!(clearing.upper_bound, value(11.0));
    }

    // what this catches: clearing against a guessed capacity.
    #[test]
    fn an_unmeasured_capacity_moves_nothing() {
        let clearing = Tier::new().clear(
            Capacity::Unavailable,
            &[claim(1, Floor::None, &[(10, 1.0, true)])],
        );
        assert_eq!(clearing.outcome, Outcome::CapacityUnavailable);
        assert!(clearing.grants.is_empty());
    }

    // what this catches: inputs the greedy's optimality and determinism do not hold for.
    #[test]
    fn non_concave_values_and_non_finite_numbers_are_refused() {
        assert!(Value::new(f64::NAN).is_err());
        assert!(Value::new(f64::INFINITY).is_err());
        let rising = claim(1, Floor::None, &[(10, 1.0, true), (10, 2.0, true)]);
        let clearing = Tier::new().clear(capacity(50), &[rising]);
        assert_eq!(
            clearing.refused,
            vec![(ClaimId(1), ClaimError::NotConcave { at: 1 })]
        );
    }

    // what this catches: a holder that stops claiming keeping its grant forever.
    #[test]
    fn a_holder_that_no_longer_claims_is_asked_to_release_everything() {
        let mut tier = Tier::new();
        tier.clear(capacity(50), &[claim(1, Floor::None, &[(40, 1.0, true)])]);
        let clearing = tier.clear(capacity(50), &[]);
        assert_eq!(
            state(&clearing, 1),
            Some(GrantState::ShrinkRequested { to: Amount::ZERO })
        );
        assert_eq!(tier.report_held(ClaimId(1), Amount::ZERO), Ok(amount(40)));
        assert!(tier.clear(capacity(50), &[]).grants.is_empty());
    }
}
