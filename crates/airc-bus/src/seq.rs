//! Generational sequence assignment (§3.8 generational order).
//!
//! `seq = (epoch, counter)`:
//!
//! - **`epoch`** is read from a persisted [`EpochStore`] on construction and
//!   **bumped once per daemon start**. Deliver-first (§3.3) can ack a
//!   `counter` the ORM hasn't flushed; a crash loses that tail, and a counter
//!   rebuilt from ORM-max would *reissue* numbers live subscribers already
//!   saw. Bumping `epoch` makes every post-restart event sort strictly after
//!   anything pre-restart — even if the counter rewinds.
//! - **`counter`** is the in-memory monotonic, assigned atomically per
//!   publish.
//!
//! Both are injectable so tests are deterministic (§9). The
//! [`InMemoryEpochStore`] models the persisted epoch cell; a restart is
//! modeled by constructing a fresh [`SeqSource`] against the *same*
//! `InMemoryEpochStore`, which bumps the epoch exactly as a real daemon start
//! would.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

/// Persisted home of the `epoch` value. The only piece of [`SeqSource`] that
/// survives a restart. The ORM-backed impl lands in a later slice; tests use
/// [`InMemoryEpochStore`].
///
/// Contract: [`EpochStore::bump_and_load`] atomically increments the stored
/// epoch and returns the *new* value. Called exactly once per daemon start.
pub trait EpochStore: Send + Sync {
    /// Atomically bump the persisted epoch and return the new value. The first
    /// ever call on a fresh store returns `1` (epoch `0` is reserved for the
    /// "never started" sentinel / unstamped envelopes).
    fn bump_and_load(&self) -> u64;
}

/// In-memory persisted-epoch model. A clone shares the same underlying cell,
/// so a "restart" = constructing a new [`SeqSource`] from a clone of this
/// store. That clone keeps the bumped epoch, exactly like a row that survives
/// the process.
#[derive(Debug, Clone, Default)]
pub struct InMemoryEpochStore {
    epoch: Arc<AtomicU64>,
}

impl InMemoryEpochStore {
    /// A store that has never been started (epoch sentinel `0`). The first
    /// [`SeqSource`] built against it gets epoch `1`.
    pub fn new() -> Self {
        Self {
            epoch: Arc::new(AtomicU64::new(0)),
        }
    }

    /// The currently-persisted epoch (for test assertions). Not part of the
    /// hot path.
    pub fn current(&self) -> u64 {
        self.epoch.load(Ordering::SeqCst)
    }
}

impl EpochStore for InMemoryEpochStore {
    fn bump_and_load(&self) -> u64 {
        // fetch_add returns the previous value; the new epoch is +1.
        self.epoch.fetch_add(1, Ordering::SeqCst) + 1
    }
}

/// Assigns `(epoch, counter)` for one daemon lifetime (§3.8).
///
/// Built once per daemon start: it bumps the persisted epoch and starts the
/// in-memory counter from `start_counter` (normally `0`; a restart may seed it
/// from the durable max, but the bumped epoch dominates regardless so the seed
/// is an optimization, never a correctness lever — see crash-safe-seq test).
pub struct SeqSource {
    epoch: u64,
    counter: AtomicU64,
}

impl SeqSource {
    /// Construct for a new daemon start: bump the persisted epoch, begin the
    /// counter at `0`.
    pub fn start(epoch_store: &dyn EpochStore) -> Self {
        Self::start_at_counter(epoch_store, 0)
    }

    /// Construct for a new daemon start, seeding the in-memory counter at
    /// `start_counter`. The bumped epoch still dominates the total order, so
    /// even a *wrong* seed can never reissue a `(epoch, counter)` pair an
    /// earlier epoch used — the crash-safety guarantee does not rest on the
    /// seed (§3.8).
    pub fn start_at_counter(epoch_store: &dyn EpochStore, start_counter: u64) -> Self {
        Self {
            epoch: epoch_store.bump_and_load(),
            counter: AtomicU64::new(start_counter),
        }
    }

    /// The epoch this source stamps. Stable for the source's lifetime.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Assign the next `(epoch, counter)`. Counter is strictly monotonic
    /// within the epoch.
    pub fn next(&self) -> crate::Seq {
        let counter = self.counter.fetch_add(1, Ordering::SeqCst);
        crate::Seq::new(self.epoch, counter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_start_gets_epoch_one() {
        let store = InMemoryEpochStore::new();
        let src = SeqSource::start(&store);
        assert_eq!(src.epoch(), 1);
        assert_eq!(store.current(), 1);
    }

    #[test]
    fn restart_bumps_epoch_against_same_store() {
        let store = InMemoryEpochStore::new();
        let s1 = SeqSource::start(&store);
        assert_eq!(s1.epoch(), 1);
        // "restart": new source, same persisted store.
        let s2 = SeqSource::start(&store.clone());
        assert_eq!(s2.epoch(), 2, "every daemon start bumps the epoch");
    }

    #[test]
    fn counter_is_monotonic_within_epoch() {
        let store = InMemoryEpochStore::new();
        let src = SeqSource::start(&store);
        let a = src.next();
        let b = src.next();
        let c = src.next();
        assert_eq!(a, crate::Seq::new(1, 0));
        assert_eq!(b, crate::Seq::new(1, 1));
        assert_eq!(c, crate::Seq::new(1, 2));
    }

    #[test]
    fn post_restart_seq_sorts_after_pre_restart_even_with_rewound_counter() {
        // The crux of §3.8: pre-crash counter ran high; post-crash counter
        // (rebuilt from durable max which is lower) rewinds — but epoch+1
        // makes the post-crash event sort strictly after.
        let store = InMemoryEpochStore::new();
        let pre = SeqSource::start(&store);
        let last_pre = {
            let mut s = pre.next();
            for _ in 0..999 {
                s = pre.next();
            }
            s
        };
        assert_eq!(last_pre, crate::Seq::new(1, 999));

        // Restart, seed counter low (durable only had a handful flushed).
        let post = SeqSource::start_at_counter(&store, 10);
        let first_post = post.next();
        assert_eq!(first_post, crate::Seq::new(2, 10));
        assert!(
            first_post > last_pre,
            "post-restart seq must sort strictly after pre-restart even though counter rewound"
        );
    }
}
