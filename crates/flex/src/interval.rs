//! Pure instantaneous-concurrency constraint for a joint clearing.
//! Half-open intervals share a seat at a boundary, never during an overlap.
//! This module does not schedule, reserve, or release physical custody.

use std::collections::BTreeMap;

use crate::{Amount, PoolId, TopologyRevision, Unit};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Lanes;
impl Unit for Lanes {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IntervalClock(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneInterval {
    start: u64,
    end: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IntervalError {
    EmptyOrReversed,
    WrongTopology,
    WrongClock,
    DuplicatePool(PoolId),
    Unmeasured(PoolId),
    Overflow(PoolId),
    Overlap {
        pool: PoolId,
        at: u64,
        required: u128,
        available: Amount<Lanes>,
    },
}

impl LaneInterval {
    pub fn new(start: u64, end: u64) -> Result<Self, IntervalError> {
        if start >= end {
            return Err(IntervalError::EmptyOrReversed);
        }
        Ok(Self { start, end })
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LaneDemand {
    pub pool: PoolId,
    pub interval: LaneInterval,
    pub lanes: Amount<Lanes>,
}

/// Capacity belongs to the measured topology and clock incarnation. A reboot
/// cannot accidentally reinterpret an old time range as a current reservation.
pub struct LaneCapacity {
    topology: TopologyRevision,
    clock: IntervalClock,
    pools: BTreeMap<PoolId, Amount<Lanes>>,
}

impl LaneCapacity {
    pub fn new(
        topology: TopologyRevision,
        clock: IntervalClock,
        pools: impl IntoIterator<Item = (PoolId, Amount<Lanes>)>,
    ) -> Result<Self, IntervalError> {
        let mut measured = BTreeMap::new();
        for (pool, amount) in pools {
            if measured.insert(pool, amount).is_some() {
                return Err(IntervalError::DuplicatePool(pool));
            }
        }
        Ok(Self {
            topology,
            clock,
            pools: measured,
        })
    }

    /// Check the entire candidate set, including existing reservations. Summed
    /// lane-seconds are insufficient: every event boundary must fit the knee.
    pub fn check(
        &self,
        topology: TopologyRevision,
        clock: IntervalClock,
        demands: &[LaneDemand],
    ) -> Result<(), IntervalError> {
        if topology != self.topology {
            return Err(IntervalError::WrongTopology);
        }
        if clock != self.clock {
            return Err(IntervalError::WrongClock);
        }
        let mut events = BTreeMap::<(PoolId, u64), i128>::new();
        for demand in demands {
            let pool = demand.pool;
            if !self.pools.contains_key(&pool) {
                return Err(IntervalError::Unmeasured(pool));
            }
            for (at, delta) in [
                (demand.interval.start, i128::from(demand.lanes.raw())),
                (demand.interval.end, -i128::from(demand.lanes.raw())),
            ] {
                let value = events.entry((pool, at)).or_insert(0);
                *value = value
                    .checked_add(delta)
                    .ok_or(IntervalError::Overflow(pool))?;
            }
        }
        let mut active = 0_i128;
        let mut previous_pool = None;
        for ((pool, at), delta) in events {
            if previous_pool != Some(pool) {
                active = 0;
                previous_pool = Some(pool);
            }
            active = active
                .checked_add(delta)
                .ok_or(IntervalError::Overflow(pool))?;
            let available = *self
                .pools
                .get(&pool)
                .ok_or(IntervalError::Unmeasured(pool))?;
            if active > i128::from(available.raw()) {
                return Err(IntervalError::Overlap {
                    pool,
                    at,
                    required: active as u128,
                    available,
                });
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
