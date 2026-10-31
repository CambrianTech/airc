//! Values and prices: finite numbers under a total order, so a clearing is deterministic.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use crate::amount::{Amount, Unit};

/// A finite value. NaN and infinities are refused on construction, so values compare under a
/// total order and two machines clear the same inputs the same way.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct Value(f64);

/// A value that was NaN or infinite.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NonFinite(pub f64);

impl Value {
    pub const ZERO: Self = Self(0.0);

    pub fn new(raw: f64) -> Result<Self, NonFinite> {
        if raw.is_finite() {
            Ok(Self(raw))
        } else {
            Err(NonFinite(raw))
        }
    }

    pub const fn raw(self) -> f64 {
        self.0
    }

    /// `self` times `n` units: finite because both factors are, unless it overflows, which is
    /// refused rather than turned into infinity.
    pub(crate) fn times(self, n: u64) -> Result<Self, NonFinite> {
        Self::new(self.0 * n as f64)
    }

    pub(crate) fn total_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.total_cmp(&other.0)
    }
}

impl TryFrom<f64> for Value {
    type Error = NonFinite;

    fn try_from(raw: f64) -> Result<Self, NonFinite> {
        Self::new(raw)
    }
}

impl From<Value> for f64 {
    fn from(value: Value) -> f64 {
        value.0
    }
}

impl std::fmt::Display for NonFinite {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "a value must be finite, got {}", self.0)
    }
}

impl std::error::Error for NonFinite {}

/// Value per unit of `U`: a tier's clearing price. Prices of different units are different types,
/// so they can never be compared or summed without an explicit conversion.
#[derive(Serialize, Deserialize)]
#[serde(transparent, bound = "")]
pub struct Price<U: Unit> {
    per_unit: Value,
    #[serde(skip)]
    unit: PhantomData<U>,
}

impl<U: Unit> Price<U> {
    pub const fn per_unit(per_unit: Value) -> Self {
        Self { per_unit, unit: PhantomData }
    }

    pub const fn value(self) -> Value {
        self.per_unit
    }

    /// What `amount` costs at this price.
    pub fn of(self, amount: Amount<U>) -> Result<Value, NonFinite> {
        self.per_unit.times(amount.raw())
    }
}

impl<U: Unit> Clone for Price<U> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<U: Unit> Copy for Price<U> {}

impl<U: Unit> PartialEq for Price<U> {
    fn eq(&self, other: &Self) -> bool {
        self.per_unit == other.per_unit
    }
}

impl<U: Unit> std::fmt::Debug for Price<U> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/unit", self.per_unit.0)
    }
}
