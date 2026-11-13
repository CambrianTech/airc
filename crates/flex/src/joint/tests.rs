use super::*;
use crate::{Amount, LaneInterval, PoolId, PoolTopology, PoolViewId};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Bytes;
impl Unit for Bytes {}

#[test]
fn whole_bundle_must_fit_both_space_and_overlapping_lanes() -> Result<(), JointError<Bytes>> {
    let revision = TopologyRevision(1);
    let clock = IntervalClock(1);
    let topology = PoolTopology::<Bytes>::new(revision, [(PoolViewId(1), PoolId(1))])
        .map_err(JointError::Space)?;
    let space = topology
        .demand(revision, [(PoolViewId(1), Amount::new(5))])
        .map_err(JointError::Space)?;
    let demand = JointDemand::new(
        space,
        clock,
        vec![LaneDemand {
            pool: PoolId(2),
            interval: LaneInterval::new(0, 10).map_err(JointError::Lanes)?,
            lanes: Amount::new(1),
        }],
    );
    let snapshot = demand.clone();
    let capacity = JointCapacity::new(
        topology
            .capacity(revision, [(PoolId(1), Amount::new(20))])
            .map_err(JointError::Space)?,
        LaneCapacity::new(revision, clock, [(PoolId(2), Amount::new(1))])
            .map_err(JointError::Lanes)?,
    );
    assert_eq!(capacity.check(&demand), Ok(()));
    let combined = demand.checked_add(&demand)?;
    assert!(matches!(
        capacity.check(&combined),
        Err(JointError::Lanes(IntervalError::Overlap { .. }))
    ));
    assert_eq!(
        demand, snapshot,
        "pure candidate evaluation preserves existing custody input"
    );
    assert_eq!(capacity.check(&demand), Ok(()));
    let tight_space = JointCapacity::new(
        topology
            .capacity(revision, [(PoolId(1), Amount::new(4))])
            .map_err(JointError::Space)?,
        LaneCapacity::new(revision, clock, [(PoolId(2), Amount::new(10))])
            .map_err(JointError::Lanes)?,
    );
    assert!(matches!(
        tight_space.check(&demand),
        Err(JointError::Capacity(PoolFitError::Insufficient { .. }))
    ));
    Ok(())
}
