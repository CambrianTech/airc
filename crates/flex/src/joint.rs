//! One feasibility boundary for space and instantaneous concurrency together.
//! The solver checks the complete candidate (including retained custody), never
//! independently grants its components. Checking is pure, not a reservation.

use crate::{
    IntervalClock, IntervalError, LaneCapacity, LaneDemand, PoolAmounts, PoolCapacity, PoolError,
    PoolFitError, TopologyRevision, Unit,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JointDemand<U: Unit> {
    space: PoolAmounts<U>,
    clock: IntervalClock,
    lanes: Vec<LaneDemand>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum JointError<U: Unit> {
    Space(PoolError),
    Capacity(PoolFitError<U>),
    Lanes(IntervalError),
}

impl<U: Unit> JointDemand<U> {
    pub fn new(space: PoolAmounts<U>, clock: IntervalClock, lanes: Vec<LaneDemand>) -> Self {
        Self {
            space,
            clock,
            lanes,
        }
    }

    pub fn checked_add(&self, other: &Self) -> Result<Self, JointError<U>> {
        if self.clock != other.clock {
            return Err(JointError::Lanes(IntervalError::WrongClock));
        }
        let space = self
            .space
            .checked_add(&other.space)
            .map_err(JointError::Space)?;
        let mut lanes = self.lanes.clone();
        lanes.extend_from_slice(&other.lanes);
        Ok(Self {
            space,
            clock: self.clock,
            lanes,
        })
    }

    pub fn topology(&self) -> TopologyRevision {
        self.space.revision()
    }
    pub fn space(&self) -> &PoolAmounts<U> {
        &self.space
    }
    pub fn lanes(&self) -> &[LaneDemand] {
        &self.lanes
    }
}

/// Both observations must accept a candidate before it is eligible for commit.
/// No per-resource partial success is returned to the caller.
pub struct JointCapacity<U: Unit> {
    space: PoolCapacity<U>,
    lanes: LaneCapacity,
}

impl<U: Unit> JointCapacity<U> {
    pub fn new(space: PoolCapacity<U>, lanes: LaneCapacity) -> Self {
        Self { space, lanes }
    }

    pub fn check(&self, demand: &JointDemand<U>) -> Result<(), JointError<U>> {
        self.space
            .check(&demand.space)
            .map_err(JointError::Capacity)?;
        self.lanes
            .check(demand.topology(), demand.clock, &demand.lanes)
            .map_err(JointError::Lanes)
    }
}

#[cfg(test)]
mod tests;
