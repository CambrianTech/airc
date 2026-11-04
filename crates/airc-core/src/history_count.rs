//! How much matching history lies between two cursors, answered by the store.
//!
//! A reader that only needs "how many, from when to when" must never page the
//! events themselves to find out: the store counts them where they live and
//! returns three numbers. One type for every layer (store, bus sink, router,
//! IPC, client) so a count means one thing end to end.

use serde::{Deserialize, Serialize};

/// The most events a count reads before it stops and says "more": a count is shown to a
/// reader as a number, and past this one "999+" tells it as much as the exact figure, so no
/// store walks a range longer than this to answer (Joel, 2026-11-01: every read is bounded;
/// recency is the common path).
pub const HISTORY_COUNT_CAP: u64 = 999;

/// Matching events in a range, and the original times of the oldest and newest counted.
/// `more`: the range holds more than `count`; the store stopped at [`HISTORY_COUNT_CAP`] and
/// counted the newest ones, so `oldest_ms` is the oldest of those, not of the range.
/// `settlement`: whether every row the filter could match was derived ([`Settlement`]); an
/// unsettled count counted the derived rows only.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryCount {
    pub count: u64,
    pub oldest_ms: Option<u64>,
    pub newest_ms: Option<u64>,
    #[serde(default)]
    pub more: bool,
    pub settlement: crate::Settlement,
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
            more: self.more || other.more,
            settlement: self.settlement.merge(other.settlement),
        }
    }

    /// The count of the newest matching events' times, read newest first and at most
    /// `HISTORY_COUNT_CAP + 1` of them: the one fold every store uses.
    pub fn of_newest(times_newest_first: impl IntoIterator<Item = u64>) -> HistoryCount {
        let mut count = HistoryCount::default();
        for (i, ms) in times_newest_first.into_iter().enumerate() {
            if i as u64 == HISTORY_COUNT_CAP {
                count.more = true;
                break;
            }
            count = count.with_event(ms);
        }
        count
    }

    /// Count one event at `occurred_at_ms` into this range.
    pub fn with_event(self, occurred_at_ms: u64) -> HistoryCount {
        self.merge(HistoryCount {
            count: 1,
            oldest_ms: Some(occurred_at_ms),
            newest_ms: Some(occurred_at_ms),
            more: false,
            settlement: crate::Settlement::Settled,
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
                newest_ms: Some(9),
                more: false,
                settlement: crate::Settlement::Settled,
            }
        );
        // One unsettled channel makes the whole count unsettled (card 70055c76).
        let unsettled = HistoryCount {
            settlement: crate::Settlement::Unsettled,
            ..HistoryCount::default()
        };
        assert_eq!(a.merge(unsettled).settlement, crate::Settlement::Unsettled);
    }

    // what this catches: a count that walks a whole range to answer. Past the cap it stops,
    // says "more", and its times are those of the newest it counted.
    #[test]
    fn a_count_stops_at_the_cap_and_says_more() {
        use super::HISTORY_COUNT_CAP;
        let past = HistoryCount::of_newest((0..HISTORY_COUNT_CAP + 5).rev());
        assert_eq!(past.count, HISTORY_COUNT_CAP);
        assert!(past.more);
        assert_eq!(past.newest_ms, Some(HISTORY_COUNT_CAP + 4));
        let under = HistoryCount::of_newest([7, 3]);
        assert_eq!(
            (under.count, under.more, under.oldest_ms),
            (2, false, Some(3))
        );
    }
}
