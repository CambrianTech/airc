//! How much matching history lies between two cursors, answered by the store.
//!
//! A reader that only needs "how many, from when to when" must never page the
//! events themselves to find out: the store counts them where they live and
//! returns three numbers. One type for every layer (store, bus sink, router,
//! IPC, client) so a count means one thing end to end.

use serde::{Deserialize, Serialize};

/// Matching events in a range, and the original times of the oldest and newest.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCount {
    pub count: u64,
    pub oldest_ms: Option<u64>,
    pub newest_ms: Option<u64>,
}

impl HistoryCount {
    /// Combine counts of disjoint ranges or channels.
    pub fn merge(self, other: HistoryCount) -> HistoryCount {
        let pick = |a: Option<u64>, b: Option<u64>, f: fn(u64, u64) -> u64| match (a, b) {
            (Some(a), Some(b)) => Some(f(a, b)),
            (a, None) => a,
            (None, b) => b,
        };
        HistoryCount {
            count: self.count + other.count,
            oldest_ms: pick(self.oldest_ms, other.oldest_ms, u64::min),
            newest_ms: pick(self.newest_ms, other.newest_ms, u64::max),
        }
    }

    /// Count one event at `occurred_at_ms` into this range.
    pub fn with_event(self, occurred_at_ms: u64) -> HistoryCount {
        self.merge(HistoryCount {
            count: 1,
            oldest_ms: Some(occurred_at_ms),
            newest_ms: Some(occurred_at_ms),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::HistoryCount;

    // what this catches: merging per-channel counts keeps the true oldest and newest.
    #[test]
    fn merge_keeps_the_extremes_and_sums() {
        let a = HistoryCount::default().with_event(5).with_event(9);
        let b = HistoryCount::default().with_event(2);
        assert_eq!(
            a.merge(b).merge(HistoryCount::default()),
            HistoryCount {
                count: 3,
                oldest_ms: Some(2),
                newest_ms: Some(9)
            }
        );
    }
}
