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

/// The holder of `id` reports `holds`, naming the custody its grant in `clearing` carried.
fn release(clearing: &Clearing<Tok>, id: u128, holds: u64) -> Release<Tok> {
    let g = grant(clearing, id).expect("test: the claim holds a grant");
    Release {
        claim: ClaimId(id),
        lease: g.lease,
        epoch: g.epoch,
        holds: amount(holds),
    }
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
    tier.clear(capacity(50), std::slice::from_ref(&a));
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
    assert_eq!(tier.report_held(release(&second, 1, 10)), Ok(amount(30)));
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
    tier.clear(capacity(50), std::slice::from_ref(&a));
    let b = claim(2, Floor::None, &[(40, 5.0, true)]);
    let second = tier.clear(capacity(50), &[a.clone(), b.clone()]);
    let third = tier.clear(capacity(50), &[a, b]);
    assert_eq!(
        tier.report_held(release(&second, 1, 50)),
        Err(ReleaseError::AboveOwned {
            owned: amount(40),
            reported: amount(50)
        })
    );
    assert_eq!(tier.report_held(release(&second, 1, 10)), Ok(amount(30)));
    assert_eq!(
        tier.report_held(release(&second, 1, 10)),
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
    tier.clear(capacity(10), std::slice::from_ref(&a));
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
    assert_eq!(tier.report_held(release(&second, 1, 0)), Ok(amount(6)));
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
    tier.clear(capacity(40), std::slice::from_ref(&a));
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
    tier.clear(capacity(40), std::slice::from_ref(&a));
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

// what this catches: a holder that exits or crashes after a shrink request stranding its units
// forever (Cormac on #5620). Withdrawn, everything it owned is free at once.
#[test]
fn a_withdrawn_holder_frees_everything_it_owned() {
    let mut tier = Tier::new();
    let a = claim(1, Floor::None, &[(40, 1.0, true)]);
    tier.clear(capacity(50), std::slice::from_ref(&a));
    let b = claim(2, Floor::None, &[(40, 5.0, true)]);
    let second = tier.clear(capacity(50), &[a, b.clone()]);
    let lease = grant(&second, 1)
        .map(|g| g.lease)
        .expect("test: A holds a lease");
    assert_eq!(tier.withdraw(ClaimId(1), lease), Ok(amount(40)));
    assert_eq!(tier.withdraw(ClaimId(1), lease), Err(ReleaseError::Unknown));
    let after = tier.clear(capacity(50), &[b]);
    assert_eq!(
        (owned(&after, 2), state(&after, 2)),
        (amount(40), Some(GrantState::Granted))
    );
}

// what this catches: a late or replayed release freeing units the holder physically holds again
// after it regrew (Codex's P0 on airc#1630: capacity 10, A shrinks to 4 and reports, regrows to
// 10, then the old report is replayed; without custody epochs B would acquire 6 while A held 10).
#[test]
fn a_report_from_before_regrowth_has_no_effect() {
    let mut tier = Tier::new();
    let a = claim(1, Floor::None, &[(10, 1.0, true)]);
    tier.clear(capacity(10), std::slice::from_ref(&a));
    let b = claim(2, Floor::None, &[(6, 5.0, true)]);
    let shrink = tier.clear(capacity(10), &[a.clone(), b]);
    assert_eq!(
        state(&shrink, 1),
        Some(GrantState::ShrinkRequested { to: amount(4) })
    );
    let old = release(&shrink, 1, 4);
    assert_eq!(tier.report_held(old), Ok(amount(6)));
    // B leaves; A regrows to 10 and physically takes it.
    let regrown = tier.clear(capacity(10), std::slice::from_ref(&a));
    assert_eq!(owned(&regrown, 1), amount(10));
    assert_eq!(
        tier.report_held(old),
        Err(ReleaseError::Superseded {
            current: old.epoch + 1,
            named: old.epoch
        })
    );
    let b = claim(2, Floor::None, &[(6, 5.0, true)]);
    let contested = tier.clear(capacity(10), &[a, b]);
    assert_eq!(
        owned(&contested, 1),
        amount(10),
        "A is still charged what it holds"
    );
    assert_eq!(
        owned(&contested, 2),
        Amount::ZERO,
        "B never acquires a unit A holds"
    );
}

// what this catches: a delayed termination of an old process ending a re-created holder, and
// custody changing hands inside a clearing (Codex on airc#1630).
#[test]
fn custody_is_ended_only_by_its_own_lease_and_never_changes_hands_in_a_clearing() {
    let mut tier = Tier::new();
    let a = claim(1, Floor::None, &[(10, 1.0, true)]);
    let first = tier.clear(capacity(20), std::slice::from_ref(&a));
    let old_lease = grant(&first, 1).map(|g| g.lease).expect("test: a lease");
    assert_eq!(tier.withdraw(ClaimId(1), old_lease), Ok(amount(10)));
    let recreated = tier.clear(capacity(20), std::slice::from_ref(&a));
    let new_lease = grant(&recreated, 1)
        .map(|g| g.lease)
        .expect("test: a lease");
    assert_ne!(old_lease, new_lease);
    assert_eq!(
        tier.withdraw(ClaimId(1), old_lease),
        Err(ReleaseError::WrongLease {
            current: new_lease,
            named: old_lease
        })
    );
    assert_eq!(
        owned(&tier.clear(capacity(20), std::slice::from_ref(&a)), 1),
        amount(10)
    );
    let mut impostor = a.clone();
    impostor.owner = OwnerId(99);
    let refused = tier.clear(capacity(20), &[impostor]);
    assert_eq!(
        refused.refused,
        vec![(ClaimId(1), ClaimError::OwnerChanged { holder: OwnerId(1) })]
    );
}

// what this catches: measured capacity falling below what holders own (S4: the person's game
// took memory) read as a negative free, or as room to grow.
#[test]
fn capacity_below_what_is_owned_is_overcommitted_and_nothing_grows() {
    let mut tier = Tier::new();
    let a = claim(1, Floor::None, &[(40, 1.0, true)]);
    tier.clear(capacity(50), std::slice::from_ref(&a));
    let b = claim(2, Floor::None, &[(10, 5.0, true)]);
    let clearing = tier.clear(capacity(30), &[a, b]);
    assert_eq!(clearing.overcommitted, Some(amount(10)));
    assert_eq!(
        state(&clearing, 1),
        Some(GrantState::ShrinkRequested { to: amount(20) })
    );
    assert_eq!(
        (owned(&clearing, 2), state(&clearing, 2)),
        (Amount::ZERO, Some(GrantState::Waiting))
    );
    assert_eq!(tier.report_held(release(&clearing, 1, 20)), Ok(amount(20)));
    let settled = tier.clear(
        capacity(30),
        &[
            claim(1, Floor::None, &[(40, 1.0, true)]),
            claim(2, Floor::None, &[(10, 5.0, true)]),
        ],
    );
    assert_eq!(settled.overcommitted, None);
    assert_eq!(
        (owned(&settled, 1), owned(&settled, 2)),
        (amount(20), amount(10))
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
    assert_eq!(tier.report_held(release(&clearing, 1, 0)), Ok(amount(40)));
    assert!(tier.clear(capacity(50), &[]).grants.is_empty());
}
