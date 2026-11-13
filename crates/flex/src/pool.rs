//! Physical-pool amounts. Views of one pool add demand to the same component;
//! equal units on different devices never become interchangeable capacity.

use std::collections::BTreeMap;

use crate::{Amount, Unit};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub struct PoolId(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TopologyRevision(pub u128);

/// A semantic view (for example host RAM or GPU-visible memory).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct PoolViewId(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolError {
    DuplicateView(PoolViewId),
    UnknownView(PoolViewId),
    WrongTopology,
    Overflow(PoolId),
    UnknownPool(PoolId),
    DuplicateCapacity(PoolId),
}

/// Measured alias topology for one unit. Construction is explicit; there is no
/// assumption that a node has one GPU, or that RAM and GPU memory are separate.
#[derive(Clone, Debug)]
pub struct PoolTopology<U: Unit> {
    revision: TopologyRevision,
    views: BTreeMap<PoolViewId, PoolId>,
    unit: std::marker::PhantomData<U>,
}

impl<U: Unit> PoolTopology<U> {
    /// One reading per physical pool. Missing observations remain unavailable;
    /// aliased capacity must never be summed like demand.
    pub fn capacity(
        &self,
        revision: TopologyRevision,
        readings: impl IntoIterator<Item = (PoolId, Amount<U>)>,
    ) -> Result<PoolCapacity<U>, PoolError> {
        if revision != self.revision {
            return Err(PoolError::WrongTopology);
        }
        let mut amounts = BTreeMap::new();
        for (pool, amount) in readings {
            if !self.views.values().any(|id| *id == pool) {
                return Err(PoolError::UnknownPool(pool));
            }
            if amounts.insert(pool, amount).is_some() {
                return Err(PoolError::DuplicateCapacity(pool));
            }
        }
        Ok(PoolCapacity { revision, amounts })
    }

    pub fn new(
        revision: TopologyRevision,
        views: impl IntoIterator<Item = (PoolViewId, PoolId)>,
    ) -> Result<Self, PoolError> {
        let mut bindings = BTreeMap::new();
        for (view, pool) in views {
            if bindings.insert(view, pool).is_some() {
                return Err(PoolError::DuplicateView(view));
            }
        }
        Ok(Self {
            revision,
            views: bindings,
            unit: std::marker::PhantomData,
        })
    }

    /// Independent demands through aliases are summed, never overwritten or
    /// maximized. Capacity observations are published once per physical pool;
    /// they must not be passed through this demand constructor.
    pub fn demand(
        &self,
        revision: TopologyRevision,
        views: impl IntoIterator<Item = (PoolViewId, Amount<U>)>,
    ) -> Result<PoolAmounts<U>, PoolError> {
        if revision != self.revision {
            return Err(PoolError::WrongTopology);
        }
        let mut amounts = BTreeMap::<PoolId, Amount<U>>::new();
        for (view, amount) in views {
            let pool = *self.views.get(&view).ok_or(PoolError::UnknownView(view))?;
            let previous = amounts.entry(pool).or_insert(Amount::ZERO);
            let total = previous
                .raw()
                .checked_add(amount.raw())
                .ok_or(PoolError::Overflow(pool))?;
            *previous = Amount::new(total);
        }
        Ok(PoolAmounts { revision, amounts })
    }
}

/// Available capacity is distinct from demand: absent means unmeasured, not
/// zero demand and not unlimited room. No custody changes occur in this type.
#[derive(Clone, Debug)]
pub struct PoolCapacity<U: Unit> {
    revision: TopologyRevision,
    amounts: BTreeMap<PoolId, Amount<U>>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PoolFitError<U: Unit> {
    WrongTopology,
    Unmeasured(PoolId),
    Insufficient {
        pool: PoolId,
        required: Amount<U>,
        available: Amount<U>,
    },
}

impl<U: Unit> PoolCapacity<U> {
    /// Check every component before a caller can commit a bundle. This is pure;
    /// checking one component never reserves it ahead of the others.
    pub fn check(&self, demand: &PoolAmounts<U>) -> Result<(), PoolFitError<U>> {
        if self.revision != demand.revision {
            return Err(PoolFitError::WrongTopology);
        }
        for (pool, required) in demand.components() {
            let available = *self
                .amounts
                .get(&pool)
                .ok_or(PoolFitError::Unmeasured(pool))?;
            if required > available {
                return Err(PoolFitError::Insufficient {
                    pool,
                    required,
                    available,
                });
            }
        }
        Ok(())
    }
}

/// Normalized demand; callers cannot insert a second component for an alias.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PoolAmounts<U: Unit> {
    revision: TopologyRevision,
    amounts: BTreeMap<PoolId, Amount<U>>,
}

impl<U: Unit> PoolAmounts<U> {
    /// Combine complete demands without changing either input on refusal.
    pub fn checked_add(&self, other: &Self) -> Result<Self, PoolError> {
        if self.revision != other.revision {
            return Err(PoolError::WrongTopology);
        }
        let mut result = self.clone();
        for (pool, amount) in other.components() {
            let previous = result.amounts.entry(pool).or_insert(Amount::ZERO);
            *previous = Amount::new(
                previous
                    .raw()
                    .checked_add(amount.raw())
                    .ok_or(PoolError::Overflow(pool))?,
            );
        }
        Ok(result)
    }

    pub fn revision(&self) -> TopologyRevision {
        self.revision
    }

    pub fn components(&self) -> impl Iterator<Item = (PoolId, Amount<U>)> + '_ {
        self.amounts.iter().map(|(pool, amount)| (*pool, *amount))
    }
}

#[cfg(test)]
mod tests;
