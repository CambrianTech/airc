//! Whether a filtered read saw all the history it matches. A store derives the data a filter
//! reads (the header rows and addressing of each event) at append; rows stored before that data
//! existed are derived by the store's background task after open, never by a read (card
//! 70055c76: a 50-row inbox read that derived its own range took 99.8 s on a 1.55 GB store).
//! Until that task finishes, a filtered read answers from the rows already derived and says so,
//! so a missing old match is never silent.

use serde::{Deserialize, Serialize};

/// Whether every row a filtered read could match was derived when it ran.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Settlement {
    /// Every row was derived: the answer is exact.
    #[default]
    Settled,
    /// Rows stored before their derived data existed were not yet derived: an old match may be
    /// missing. The same read after the store's derivation finishes is exact.
    Unsettled,
}

impl Settlement {
    /// Combine the settlements of reads of disjoint ranges or channels: exact only if each is.
    pub fn merge(self, other: Settlement) -> Settlement {
        match (self, other) {
            (Settlement::Settled, Settlement::Settled) => Settlement::Settled,
            _ => Settlement::Unsettled,
        }
    }
}

/// A page of a filtered history read and its [`Settlement`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryPage<T> {
    pub rows: Vec<T>,
    pub settlement: Settlement,
}
