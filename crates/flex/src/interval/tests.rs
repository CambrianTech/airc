use super::*;

#[test]
fn overlapping_claims_cannot_spend_the_same_seat() -> Result<(), IntervalError> {
    let topology = TopologyRevision(1);
    let clock = IntervalClock(2);
    let capacity = LaneCapacity::new(topology, clock, [(PoolId(1), Amount::new(1))])?;
    let first = LaneDemand {
        pool: PoolId(1),
        interval: LaneInterval::new(0, 10)?,
        lanes: Amount::new(1),
    };
    let second = LaneDemand {
        interval: LaneInterval::new(5, 15)?,
        ..first
    };
    assert_eq!(
        capacity.check(topology, clock, &[first, second]),
        Err(IntervalError::Overlap {
            pool: PoolId(1),
            at: 5,
            required: 2,
            available: Amount::new(1),
        })
    );
    let adjacent = LaneDemand {
        interval: LaneInterval::new(10, 20)?,
        ..first
    };
    assert_eq!(capacity.check(topology, clock, &[first, adjacent]), Ok(()));
    assert_eq!(
        capacity.check(topology, IntervalClock(3), &[first]),
        Err(IntervalError::WrongClock)
    );
    assert_eq!(
        capacity.check(TopologyRevision(2), clock, &[first]),
        Err(IntervalError::WrongTopology)
    );
    assert_eq!(
        LaneInterval::new(10, 10),
        Err(IntervalError::EmptyOrReversed)
    );
    Ok(())
}

#[test]
fn separate_pools_do_not_share_seats_or_hide_unmeasured_capacity() -> Result<(), IntervalError> {
    let topology = TopologyRevision(1);
    let clock = IntervalClock(1);
    let first = LaneDemand {
        pool: PoolId(1),
        interval: LaneInterval::new(0, 10)?,
        lanes: Amount::new(1),
    };
    let other = LaneDemand {
        pool: PoolId(2),
        ..first
    };
    let capacity = LaneCapacity::new(
        topology,
        clock,
        [(PoolId(1), Amount::new(1)), (PoolId(2), Amount::new(1))],
    )?;
    assert_eq!(capacity.check(topology, clock, &[first, other]), Ok(()));
    assert_eq!(
        capacity.check(
            topology,
            clock,
            &[LaneDemand {
                pool: PoolId(3),
                ..first
            }]
        ),
        Err(IntervalError::Unmeasured(PoolId(3)))
    );
    Ok(())
}
