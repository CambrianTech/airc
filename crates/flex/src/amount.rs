//! Amounts of a resource, typed by their unit so two units never mix.

use std::fmt;
use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

/// A resource's unit (tokens, bytes, lane-seconds, bytes in flight). Each consumer defines its own
/// marker type; the solver never converts between units.
pub trait Unit: Copy + Eq + Ord + fmt::Debug + Send + Sync + 'static {}

/// A whole number of `U`.
#[derive(Serialize, Deserialize)]
#[serde(transparent, bound = "")]
pub struct Amount<U: Unit> {
    raw: u64,
    #[serde(skip)]
    unit: PhantomData<U>,
}

impl<U: Unit> Amount<U> {
    pub const ZERO: Self = Self::new(0);

    pub const fn new(raw: u64) -> Self {
        Self { raw, unit: PhantomData }
    }

    pub const fn raw(self) -> u64 {
        self.raw
    }

    pub fn saturating_add(self, other: Self) -> Self {
        Self::new(self.raw.saturating_add(other.raw))
    }

    pub fn saturating_sub(self, other: Self) -> Self {
        Self::new(self.raw.saturating_sub(other.raw))
    }
}

impl<U: Unit> Clone for Amount<U> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<U: Unit> Copy for Amount<U> {}

impl<U: Unit> PartialEq for Amount<U> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<U: Unit> Eq for Amount<U> {}

impl<U: Unit> PartialOrd for Amount<U> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl<U: Unit> Ord for Amount<U> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.raw.cmp(&other.raw)
    }
}

impl<U: Unit> fmt::Debug for Amount<U> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

impl<U: Unit> std::iter::Sum for Amount<U> {
    fn sum<I: Iterator<Item = Self>>(iter: I) -> Self {
        iter.fold(Self::ZERO, Self::saturating_add)
    }
}
