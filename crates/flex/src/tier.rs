//! A tier: one resource's measured capacity, the grants its holders own, and each clearing that
//! moves them. Growth comes only from capacity no one owns; a shrink is a request whose capacity is
//! granted to no one until the holder acknowledges the release.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::amount::{Amount, Unit};
use crate::claim::{Claim, ClaimError, ClaimId, Floor, OwnerId};
use crate::provenance::Provenance;
use crate::solve::solve;
use crate::value::{NonFinite, Price};

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

/// What became of one claim in a clearing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum GrantState<U: Unit> {
    /// It owns its target.
    Granted,
    /// Its target differed from what it owns by less than its switch cost: nothing moved.
    Held,
    /// It owns more than its target: it is asked to release down to `to`. The difference is granted
    /// to no one until it acknowledges.
    ShrinkRequested { to: Amount<U> },
    /// It owns less than its target because the rest is still owned by holders asked to release.
    Waiting { target: Amount<U> },
    /// Its values are unmeasured: it is granted its reservation only.
    Unmeasured,
}

/// One claim's grant after a clearing.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub struct Grant<U: Unit> {
    pub claim: ClaimId,
    pub owner: Option<OwnerId>,
    /// What it may use now.
    pub owned: Amount<U>,
    pub state: GrantState<U>,
    /// The tier's generation when this grant was made; an older one is refused.
    pub generation: u64,
}

/// How a clearing ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(bound = "")]
pub enum Outcome<U: Unit> {
    Cleared,
    /// The required floors alone exceed capacity. Nothing moved; the named claims need
    /// scheduling, placement or a smaller footprint, never an oversubscription.
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
    pub price: Price<U>,
    /// Capacity no target asked for.
    pub unallocated: Amount<U>,
    /// Claims refused before clearing, and why.
    pub refused: Vec<(ClaimId, ClaimError)>,
}

/// Why an acknowledgement was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReleaseError {
    /// No such holder.
    Unknown,
    /// Nothing was asked of it.
    NothingRequested,
    /// The acknowledgement names an older generation than the request it answers.
    Stale {
        requested_at: u64,
        acknowledged: u64,
    },
}

#[derive(Clone, Copy, Debug)]
struct Holding<U: Unit> {
    owner: Option<OwnerId>,
    owned: Amount<U>,
    shrink_to: Option<Amount<U>>,
    generation: u64,
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
        let Ok(Targets {
            amounts,
            held,
            price,
            unallocated,
        }) = self.targets(capacity_amount, &valid)
        else {
            return self.unmoved(capacity, Outcome::NonFinite, refused);
        };
        self.generation += 1;
        let generation = self.generation;
        // Every holder without a claim this time is asked to release everything.
        for (id, holding) in self.holdings.iter_mut() {
            if !valid.iter().any(|c| c.id == *id) && holding.owned > Amount::ZERO {
                holding.shrink_to = Some(Amount::ZERO);
                holding.generation = generation;
            }
        }
        let owned_now: Amount<U> = self.holdings.values().map(|h| h.owned).sum();
        let mut free = capacity_amount.saturating_sub(owned_now);
        let mut states: BTreeMap<ClaimId, GrantState<U>> = BTreeMap::new();
        // Shrinks first (they free nothing until acknowledged), then growth in claim order from
        // what is actually free.
        for (claim, target) in valid.iter().zip(amounts) {
            let holding = self.holdings.entry(claim.id).or_insert(Holding {
                owner: Some(claim.owner),
                owned: Amount::ZERO,
                shrink_to: None,
                generation,
            });
            holding.owner = Some(claim.owner);
            holding.generation = generation;
            let state = if held.contains(&claim.id) {
                holding.shrink_to = None;
                GrantState::Held
            } else if target < holding.owned {
                holding.shrink_to = Some(target);
                GrantState::ShrinkRequested { to: target }
            } else {
                holding.shrink_to = None;
                let want = target.saturating_sub(holding.owned);
                let take = want.min(free);
                free = free.saturating_sub(take);
                holding.owned = holding.owned.saturating_add(take);
                if take < want {
                    GrantState::Waiting { target }
                } else if !claim.provenance.is_measured() {
                    GrantState::Unmeasured
                } else {
                    GrantState::Granted
                }
            };
            states.insert(claim.id, state);
        }
        let grants = self.grants(&states);
        Clearing {
            generation,
            capacity,
            outcome: Outcome::Cleared,
            grants,
            price,
            unallocated,
            refused,
        }
    }

    /// The holder of `claim` released down to what the request at `generation` asked.
    pub fn acknowledge_release(
        &mut self,
        claim: ClaimId,
        generation: u64,
    ) -> Result<Amount<U>, ReleaseError> {
        let holding = self.holdings.get_mut(&claim).ok_or(ReleaseError::Unknown)?;
        let to = holding.shrink_to.ok_or(ReleaseError::NothingRequested)?;
        if generation < holding.generation {
            return Err(ReleaseError::Stale {
                requested_at: holding.generation,
                acknowledged: generation,
            });
        }
        let freed = holding.owned.saturating_sub(to);
        holding.owned = to;
        holding.shrink_to = None;
        if to == Amount::ZERO {
            self.holdings.remove(&claim);
        }
        Ok(freed)
    }

    /// Whether a grant presented at `generation` for `claim` is current.
    pub fn is_current(&self, claim: ClaimId, generation: u64) -> bool {
        self.holdings
            .get(&claim)
            .is_some_and(|h| h.generation == generation)
    }

    /// Targets with hysteresis, aligned with `claims`: a claim whose target differs from what it
    /// owns by less value than its switch cost keeps what it owns, as a fixed amount, and the others
    /// re-clear around it. A hold is applied only when the held amounts fit the capacity.
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
        let first = solve(room, &pick(&tradeable))?;
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
            let gain = (claim.value_of(*target)?.raw() - claim.value_of(owned)?.raw()).abs();
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
                solve(room.saturating_sub(held_amount), &pick(&others))?,
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
        let states = BTreeMap::new();
        Clearing {
            generation: self.generation,
            capacity,
            outcome,
            grants: self.grants(&states),
            price: Price::per_unit(crate::value::Value::ZERO),
            unallocated: Amount::ZERO,
            refused,
        }
    }

    fn grants(&self, states: &BTreeMap<ClaimId, GrantState<U>>) -> Vec<Grant<U>> {
        self.holdings
            .iter()
            .map(|(id, h)| Grant {
                claim: *id,
                owner: h.owner,
                owned: h.owned,
                state: match (states.get(id), h.shrink_to) {
                    (Some(state), _) => *state,
                    (None, Some(to)) => GrantState::ShrinkRequested { to },
                    (None, None) => GrantState::Granted,
                },
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
    use crate::value::Value;

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

    fn owned(clearing: &Clearing<Tok>, id: u128) -> Amount<Tok> {
        clearing
            .grants
            .iter()
            .find(|g| g.claim == ClaimId(id))
            .map_or(Amount::ZERO, |g| g.owned)
    }

    fn state(clearing: &Clearing<Tok>, id: u128) -> Option<GrantState<Tok>> {
        clearing
            .grants
            .iter()
            .find(|g| g.claim == ClaimId(id))
            .map(|g| g.state)
    }

    // what this catches: a solver that splits by share instead of value. Reservations come first,
    // then the unit worth most, and the price is the value per unit of the last unit admitted.
    #[test]
    fn reservations_first_then_the_most_valuable_unit_and_the_price_is_the_last_admitted() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::Required(amount(10)), &[(30, 2.0, true)]);
        let b = claim(2, Floor::None, &[(30, 3.0, true)]);
        let clearing = tier.clear(capacity(50), &[a, b]);
        assert_eq!(clearing.outcome, Outcome::Cleared);
        assert_eq!(
            (owned(&clearing, 1), owned(&clearing, 2)),
            (amount(20), amount(30))
        );
        assert_eq!(clearing.price.value(), value(2.0));
        assert_eq!(clearing.unallocated, Amount::ZERO);
    }

    // what this catches: oversubscribing when reservations do not fit. Nothing moves, and the
    // claims that need scheduling are named.
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
        let mut tier = Tier::new();
        let mut a = claim(1, Floor::Required(amount(5)), &[(40, 9.0, true)]);
        a.provenance = Provenance::Unmeasured;
        let b = claim(2, Floor::None, &[(40, 1.0, true)]);
        let clearing = tier.clear(capacity(50), &[a, b]);
        assert_eq!(owned(&clearing, 1), amount(5));
        assert_eq!(state(&clearing, 1), Some(GrantState::Unmeasured));
        assert_eq!(owned(&clearing, 2), amount(40));
    }

    // what this catches: a clearing that depends on arrival order or breaks ties arbitrarily, so
    // two machines (or a replay) disagree. Equal value per unit goes to the lower claim id.
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

    // what this catches: two holders owning the same unit. A shrink is a request; what it frees is
    // granted to no one until the holder acknowledges, and then it is.
    #[test]
    fn a_shrink_is_regranted_only_after_its_release_is_acknowledged() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::None, &[(40, 1.0, true)]);
        let first = tier.clear(capacity(50), &[a.clone()]);
        assert_eq!(owned(&first, 1), amount(40));
        let b = claim(2, Floor::None, &[(40, 5.0, true)]);
        let second = tier.clear(capacity(50), &[a.clone(), b.clone()]);
        assert_eq!(
            state(&second, 1),
            Some(GrantState::ShrinkRequested { to: amount(10) })
        );
        assert_eq!(
            owned(&second, 1),
            amount(40),
            "it still owns what it has not released"
        );
        assert_eq!(
            owned(&second, 2),
            amount(10),
            "only the free capacity is granted"
        );
        assert_eq!(
            state(&second, 2),
            Some(GrantState::Waiting { target: amount(40) })
        );
        assert_eq!(
            tier.acknowledge_release(ClaimId(1), second.generation),
            Ok(amount(30))
        );
        let third = tier.clear(capacity(50), &[a, b]);
        assert_eq!(
            (owned(&third, 1), owned(&third, 2)),
            (amount(10), amount(40))
        );
        assert_eq!(state(&third, 2), Some(GrantState::Granted));
    }

    // what this catches: an old acknowledgement releasing against a newer request.
    #[test]
    fn an_acknowledgement_from_an_older_generation_is_refused() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::None, &[(40, 1.0, true)]);
        let first = tier.clear(capacity(50), &[a.clone()]);
        let b = claim(2, Floor::None, &[(40, 5.0, true)]);
        let second = tier.clear(capacity(50), &[a, b]);
        assert_eq!(
            tier.acknowledge_release(ClaimId(1), first.generation),
            Err(ReleaseError::Stale {
                requested_at: second.generation,
                acknowledged: first.generation
            })
        );
        assert!(tier.is_current(ClaimId(1), second.generation));
        assert!(!tier.is_current(ClaimId(1), first.generation));
    }

    // what this catches: thrash. A change worth less than what the move costs is not made.
    #[test]
    fn a_change_worth_less_than_its_switch_cost_is_held() {
        let mut tier = Tier::new();
        let mut a = claim(1, Floor::None, &[(40, 1.0, true)]);
        a.switch_cost = value(100.0);
        tier.clear(capacity(40), &[a.clone()]);
        // A newcomer wants two units more than `a` values them; moving `a` for two units is worth
        // less than its switch cost, so `a` keeps all forty and the newcomer gets none now.
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
    }

    // what this catches: a level granted in part. An item's next level is whole or not at all, and
    // a level that does not fit leaves the rest to the others.
    #[test]
    fn an_indivisible_level_that_does_not_fit_is_skipped_and_the_others_still_clear() {
        let mut tier = Tier::new();
        let a = claim(1, Floor::None, &[(30, 5.0, false)]);
        let b = claim(2, Floor::None, &[(30, 1.0, true)]);
        let clearing = tier.clear(capacity(20), &[a, b]);
        assert_eq!(
            (owned(&clearing, 1), owned(&clearing, 2)),
            (Amount::ZERO, amount(20))
        );
    }

    // what this catches: clearing against a guessed capacity.
    #[test]
    fn an_unmeasured_capacity_moves_nothing() {
        let mut tier = Tier::new();
        let clearing = tier.clear(
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
        assert_eq!(
            tier.acknowledge_release(ClaimId(1), clearing.generation),
            Ok(amount(40))
        );
        assert!(tier.clear(capacity(50), &[]).grants.is_empty());
    }
}
