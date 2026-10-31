//! Randomized conservation over the public API (Cormac, review of airc#1630): every step is a random
//! clearing (capacity measured, unavailable, or below what is owned; random concave claims), then
//! random releases and withdrawals. After every clearing the invariants hold; a failure prints the
//! seed and the step. Seeded xorshift, so the run is deterministic.

use cambrian_flex::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct B;
impl Unit for B {}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }

    fn coin(&mut self) -> bool {
        self.next() & 1 == 1
    }
}

fn measured() -> Provenance {
    Provenance::Measured {
        source: SourceId(1),
        at_ms: 0,
        version: 1,
    }
}

fn value(raw: f64) -> Value {
    Value::new(raw).expect("test: a finite value")
}

fn random_claim(rng: &mut Rng, id: u128) -> Claim<B> {
    let floor = match rng.below(3) {
        0 => Floor::Required(Amount::new(rng.below(6))),
        1 => Floor::Optional(Amount::new(1 + rng.below(6))),
        _ => Floor::None,
    };
    let mut per_unit = 1.0 + rng.below(100) as f64;
    let increments = (0..rng.below(4))
        .map(|_| {
            per_unit = (per_unit - rng.below(20) as f64).max(0.5);
            Increment {
                width: Amount::new(1 + rng.below(8)),
                per_unit: value(per_unit),
                divisible: rng.coin(),
            }
        })
        .collect();
    Claim {
        id: ClaimId(id),
        owner: OwnerId(id),
        floor,
        increments,
        provenance: if rng.below(8) == 0 {
            Provenance::Unmeasured
        } else {
            measured()
        },
        switch_cost: value(rng.below(3) as f64),
    }
}

fn owned_sum(clearing: &Clearing<B>) -> u64 {
    clearing.grants.iter().map(|g| g.owned.raw()).sum()
}

// what this catches: an invariant that holds for each transition alone but breaks across a sequence
// of clearings, releases and withdrawals (proven by mutation in the review: dropping the overcommit
// report, or letting two claims draw the same free capacity, each fails here).
#[test]
fn conservation_holds_under_random_clearings_releases_and_withdrawals() {
    for seed in 1..=200u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let mut tier: Tier<B> = Tier::new();
        let mut owned_before: std::collections::HashMap<u128, u64> = Default::default();
        for step in 0..100 {
            let cap = rng.below(40);
            let capacity = if rng.below(10) == 0 {
                Capacity::Unavailable
            } else {
                Capacity::Measured {
                    amount: Amount::new(cap),
                    provenance: measured(),
                }
            };
            let n = 1 + rng.below(6);
            let claims: Vec<Claim<B>> = (1..=n)
                .map(|id| random_claim(&mut rng, id as u128))
                .collect();
            let before_total: u64 = owned_before.values().sum();
            let c = tier.clear(capacity, &claims);
            let after_total = owned_sum(&c);
            let ctx = format!(
                "seed {seed} step {step}: cap {capacity:?} before {before_total} after {after_total} outcome {:?} over {:?}",
                c.outcome, c.overcommitted
            );
            match capacity {
                Capacity::Measured { amount, .. } => {
                    let cap = amount.raw();
                    // Never more than capacity, unless it already was and is reported.
                    if after_total > cap {
                        assert!(
                            c.overcommitted.is_some(),
                            "owned above capacity and not reported: {ctx}"
                        );
                        assert!(
                            after_total <= before_total,
                            "grew while overcommitted: {ctx}"
                        );
                    }
                    // Free is never negative.
                    assert!(c.unallocated.raw() <= cap, "free exceeds capacity: {ctx}");
                }
                Capacity::Unavailable => {
                    assert!(
                        after_total <= before_total,
                        "grew on unavailable capacity: {ctx}"
                    );
                }
            }
            // Nothing grows on an infeasible clearing.
            if matches!(c.outcome, Outcome::Infeasible { .. }) {
                for g in &c.grants {
                    let prev = owned_before.get(&g.claim.0).copied().unwrap_or(0);
                    assert!(
                        g.owned.raw() <= prev,
                        "grew on infeasible: {ctx} claim {:?}",
                        g.claim
                    );
                }
            }
            // Owning above target only while asked to shrink or held.
            for g in &c.grants {
                if g.owned > g.target {
                    assert!(
                        matches!(
                            g.state,
                            GrantState::ShrinkRequested { .. } | GrantState::Held
                        ),
                        "owns {:?} > target {:?} in {:?}: {ctx}",
                        g.owned,
                        g.target,
                        g.state
                    );
                }
            }
            // Random holder behaviour.
            owned_before.clear();
            for g in &c.grants {
                let mut now = g.owned.raw();
                match rng.below(5) {
                    0 => {
                        let _ = tier.withdraw(g.claim);
                        now = 0;
                    }
                    1 | 2 => {
                        let to = if now == 0 { 0 } else { rng.below(now + 1) };
                        if tier.report_held(g.claim, Amount::new(to)).is_ok() {
                            now = to;
                        }
                    }
                    _ => {}
                }
                if now > 0 {
                    owned_before.insert(g.claim.0, now);
                }
            }
        }
    }
}
