use super::*;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct Bytes;
impl Unit for Bytes {}

#[test]
fn unified_aliases_sum_but_discrete_devices_stay_distinct() -> Result<(), PoolError> {
    let revision = TopologyRevision(1);
    let topology = PoolTopology::<Bytes>::new(
        revision,
        [
            (PoolViewId(1), PoolId(10)),
            (PoolViewId(2), PoolId(10)),
            (PoolViewId(3), PoolId(20)),
        ],
    )?;
    let demand = topology.demand(
        revision,
        [
            (PoolViewId(1), Amount::new(7)),
            (PoolViewId(2), Amount::new(5)),
            (PoolViewId(3), Amount::new(9)),
        ],
    )?;
    assert_eq!(
        demand.components().collect::<Vec<_>>(),
        vec![(PoolId(10), Amount::new(12)), (PoolId(20), Amount::new(9)),]
    );
    let capacity = topology.capacity(
        revision,
        [
            (PoolId(10), Amount::new(10)),
            (PoolId(20), Amount::new(100)),
        ],
    )?;
    assert_eq!(
        capacity.check(&demand),
        Err(PoolFitError::Insufficient {
            pool: PoolId(10),
            required: Amount::new(12),
            available: Amount::new(10),
        })
    );
    let missing = topology.capacity(revision, [(PoolId(10), Amount::new(100))])?;
    assert_eq!(
        missing.check(&demand),
        Err(PoolFitError::Unmeasured(PoolId(20)))
    );
    assert!(matches!(
        topology.capacity(
            revision,
            [(PoolId(10), Amount::new(10)), (PoolId(10), Amount::new(10))]
        ),
        Err(PoolError::DuplicateCapacity(PoolId(10)))
    ));
    assert_eq!(
        topology.demand(TopologyRevision(2), []),
        Err(PoolError::WrongTopology)
    );
    assert_eq!(
        topology.demand(revision, [(PoolViewId(9), Amount::new(1))]),
        Err(PoolError::UnknownView(PoolViewId(9)))
    );
    assert_eq!(
        topology.demand(
            revision,
            [
                (PoolViewId(1), Amount::new(u64::MAX)),
                (PoolViewId(2), Amount::new(1)),
            ]
        ),
        Err(PoolError::Overflow(PoolId(10)))
    );
    Ok(())
}

#[test]
fn ambiguous_topology_is_rejected() {
    assert!(matches!(
        PoolTopology::<Bytes>::new(
            TopologyRevision(1),
            [(PoolViewId(1), PoolId(10)), (PoolViewId(1), PoolId(20)),]
        ),
        Err(PoolError::DuplicateView(PoolViewId(1)))
    ));
}
