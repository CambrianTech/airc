//! Per-channel hot ring — recent-event cache (§3.2, §3.8 pinned-until-persisted).
//!
//! Each active channel holds a fixed-capacity ring of recent envelopes. It
//! serves live fan-out and tail-N / recent-replay entirely from RAM; the
//! durable tier is consulted only for cold/deep replay past the ring (§3.5).
//!
//! The correctness teeth (§3.8): a `Durable` ring entry is **pinned** (not
//! evictable) until the write-behind path confirms it is persisted. This is
//! the precondition that makes "no gap" true — otherwise a seam replay could
//! find an event that is neither in the ring nor in the ORM yet. The ring
//! therefore enforces a **capacity floor ≥ max un-persisted backlog**: if the
//! oldest entry is still pinned when we need room, the ring grows past nominal
//! capacity rather than drop an unpersisted `Durable` event.
//!
//! This type is **not** internally synchronized — the router owns it behind a
//! shard mutex and never holds that lock across an `.await`.

use std::collections::VecDeque;

use crate::envelope::{Cursor, Envelope};

/// One slot in the ring.
struct Slot {
    env: Envelope,
    /// A `Durable` slot is pinned until the sink confirms persistence. While
    /// pinned it cannot be evicted (the no-gap precondition). Non-durable
    /// slots are never pinned.
    pinned: bool,
}

/// A bounded, in-order ring of recent envelopes for one channel.
pub struct HotRing {
    slots: VecDeque<Slot>,
    capacity: usize,
}

impl HotRing {
    /// Construct with nominal `capacity`. The ring may temporarily exceed this
    /// when the oldest entries are pinned (un-persisted `Durable`), preserving
    /// the §3.8 floor. `capacity` must be ≥ 1.
    pub fn new(capacity: usize) -> Self {
        Self {
            slots: VecDeque::with_capacity(capacity.max(1)),
            capacity: capacity.max(1),
        }
    }

    /// Push an envelope, evicting the oldest *unpinned* entries to stay at
    /// capacity. A `Durable` entry is inserted pinned (must be unpinned via
    /// [`HotRing::mark_persisted`] before it can be evicted). Returns nothing;
    /// the ring never drops a pinned entry, so an un-persisted `Durable` is
    /// always replayable from RAM until it's in the sink.
    pub fn push(&mut self, env: Envelope) {
        let pinned = env.delivery.is_durable();
        self.slots.push_back(Slot { env, pinned });
        self.evict_to_capacity();
    }

    /// Mark the entry with this `event_id` as persisted — unpins it so the
    /// ring may evict it under capacity pressure. Called by the write-behind
    /// path once [`crate::DurableSink::append`] confirms (§3.8).
    pub fn mark_persisted(&mut self, event_id: airc_core::EventId) {
        if let Some(slot) = self.slots.iter_mut().find(|s| s.env.event_id == event_id) {
            slot.pinned = false;
        }
        // Persistence may have unblocked an eviction that capacity pressure
        // wanted earlier; reclaim now.
        self.evict_to_capacity();
    }

    /// Drop oldest entries until at nominal capacity, but **never** evict a
    /// pinned (un-persisted `Durable`) entry — that would violate the no-gap
    /// precondition. Eviction stops at the first pinned entry from the front.
    fn evict_to_capacity(&mut self) {
        while self.slots.len() > self.capacity {
            match self.slots.front() {
                // Front is unpinned -> safe to drop.
                Some(slot) if !slot.pinned => {
                    self.slots.pop_front();
                }
                // Front is pinned -> floor reached; stop. The ring exceeds
                // nominal capacity until write-behind confirms (§3.8 floor).
                _ => break,
            }
        }
    }

    /// Return clones of every entry strictly *after* `from` (the recent leg of
    /// the cursor replay), in total order. `from == None` returns the whole
    /// ring. Used at the replay-then-live seam (§3.5).
    pub fn replay_after(&self, from: Option<Cursor>) -> Vec<Envelope> {
        self.slots
            .iter()
            .filter(|slot| match &from {
                None => true,
                Some(c) => slot.env.cursor().is_after(c),
            })
            .map(|slot| slot.env.clone())
            .collect()
    }

    /// The cursor of the oldest entry currently retained, if any. A
    /// `from_cursor` older than this means the ring cannot serve the full
    /// replay and the deep (sink) leg must cover `(from, oldest_in_ring)`.
    pub fn oldest_cursor(&self) -> Option<Cursor> {
        self.slots.front().map(|slot| slot.env.cursor())
    }

    /// Number of entries currently retained (including over-floor pinned).
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// True iff the ring holds no entries.
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// Count of currently-pinned (un-persisted `Durable`) entries — the live
    /// un-persisted backlog. Test/diagnostic lever for the §3.8 floor.
    pub fn pinned_count(&self) -> usize {
        self.slots.iter().filter(|s| s.pinned).count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{DeliveryClass, Kind};
    use crate::Seq;
    use airc_core::{ClientId, EventId, PeerId, RoomId};
    use bytes::Bytes;

    fn env_at(counter: u64, delivery: DeliveryClass) -> Envelope {
        let mut e = Envelope::new(
            RoomId::from_u128(1),
            (PeerId::from_u128(1), ClientId::from_u128(1)),
            Kind::Message,
            delivery,
            Bytes::from_static(b"x"),
        )
        .with_event_id(EventId::from_u128(counter as u128 + 1));
        e.seq = Seq::new(1, counter);
        e
    }

    #[test]
    fn evicts_unpinned_to_capacity() {
        let mut ring = HotRing::new(3);
        for c in 0..5 {
            ring.push(env_at(c, DeliveryClass::EphemeralWindow));
        }
        assert_eq!(ring.len(), 3, "non-durable entries evict to capacity");
        // oldest retained is counter 2 (0,1 evicted)
        assert_eq!(ring.oldest_cursor().unwrap().seq.counter, 2);
    }

    #[test]
    fn pinned_durable_is_not_evicted_until_persisted() {
        let mut ring = HotRing::new(2);
        // Two durable, both unpersisted -> both pinned.
        ring.push(env_at(0, DeliveryClass::Durable));
        ring.push(env_at(1, DeliveryClass::Durable));
        // Third push wants to evict counter 0, but it's pinned -> ring grows.
        ring.push(env_at(2, DeliveryClass::Durable));
        assert_eq!(
            ring.len(),
            3,
            "ring exceeds nominal capacity rather than drop an unpersisted Durable (§3.8 floor)"
        );
        assert_eq!(ring.pinned_count(), 3);

        // Confirm persistence of the oldest -> now evictable.
        ring.mark_persisted(EventId::from_u128(1)); // counter 0
        assert_eq!(ring.len(), 2, "unpinned oldest reclaimed back to capacity");
        assert_eq!(ring.oldest_cursor().unwrap().seq.counter, 1);
    }

    #[test]
    fn replay_after_returns_strictly_newer_in_order() {
        let mut ring = HotRing::new(10);
        for c in 0..5 {
            ring.push(env_at(c, DeliveryClass::Durable));
        }
        let from = env_at(1, DeliveryClass::Durable).cursor();
        let got: Vec<u64> = ring
            .replay_after(Some(from))
            .iter()
            .map(|e| e.seq.counter)
            .collect();
        assert_eq!(got, vec![2, 3, 4]);
    }
}
