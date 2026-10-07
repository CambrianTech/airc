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
//! capacity rather than drop an unpersisted `Durable` event. A pinned floor
//! does not retain an unlimited non-durable tail: those lossy cache entries
//! remain evictable without punching a hole in the durable suffix.
//!
//! The ring is bounded twice: by entry count AND by retained bytes. A count
//! alone let one channel of large envelopes hold tens of MB (measured on
//! IntelMac 2026-10-04: 435 MB plateau at 14,687 entries, ~30 KB each, ~55
//! channels each at their 256-entry count). The byte budget evicts by the same
//! pinned/durable rules as the count, and never empties the ring: the newest
//! entry always stays so the live-edge cursor survives one oversized event.
//!
//! This type is **not** internally synchronized — the router owns it behind a
//! shard mutex and never holds that lock across an `.await`.

use std::collections::VecDeque;
use std::sync::Arc;

use crate::envelope::{Cursor, Envelope};

/// One slot in the ring.
struct Slot {
    env: Arc<Envelope>,
    /// A `Durable` slot is pinned until the sink confirms persistence. While
    /// pinned it cannot be evicted (the no-gap precondition). Non-durable
    /// slots are never pinned.
    pinned: bool,
}

/// A bounded, in-order ring of recent envelopes for one channel.
pub struct HotRing {
    slots: VecDeque<Slot>,
    capacity: usize,
    byte_budget: usize,
    /// Sum of [`retained_bytes`] over `slots`, maintained on every push/evict.
    bytes: usize,
}

/// The bytes one envelope keeps alive beyond its fixed-size fields: the opaque
/// payload, the routing headers and the coalescing key. This is what grows
/// with traffic; the fixed fields are a constant per slot.
fn retained_bytes(env: &Envelope) -> usize {
    env.payload.len()
        + env
            .headers
            .iter()
            .map(|(k, v)| k.len() + v.len())
            .sum::<usize>()
        + env.coalesce_key.as_ref().map_or(0, String::len)
}

impl HotRing {
    /// Construct with nominal `capacity` entries and `byte_budget` retained
    /// bytes. The ring may temporarily exceed either when the oldest entries
    /// are pinned (un-persisted `Durable`), preserving the §3.8 floor.
    /// `capacity` must be ≥ 1.
    pub fn new(capacity: usize, byte_budget: usize) -> Self {
        Self {
            slots: VecDeque::new(),
            capacity: capacity.max(1),
            byte_budget,
            bytes: 0,
        }
    }

    /// Push an envelope, evicting the oldest *unpinned* entries to stay at
    /// capacity. A `Durable` entry is inserted pinned (must be unpinned via
    /// [`HotRing::mark_persisted`] before it can be evicted). Returns nothing;
    /// the ring never drops a pinned entry, so an un-persisted `Durable` is
    /// always replayable from RAM until it's in the sink.
    pub fn push(&mut self, env: Arc<Envelope>) {
        let pinned = env.delivery.is_durable();
        self.bytes += retained_bytes(&env);
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

    /// Over either bound: more entries than `capacity`, or more retained bytes
    /// than `byte_budget` while more than the newest entry is held.
    fn over_bounds(&self) -> bool {
        self.slots.len() > self.capacity || (self.slots.len() > 1 && self.bytes > self.byte_budget)
    }

    /// Remove the slot at `index`, keeping the byte total in step.
    fn remove_at(&mut self, index: usize) {
        if let Some(slot) = self.slots.remove(index) {
            self.bytes -= retained_bytes(&slot.env);
        }
    }

    /// Drop the oldest safe entries until within both bounds. Behind a pinned
    /// head, ONLY non-durable entries may be reclaimed: removing a persisted
    /// durable there would punch a hole in the durable suffix that the router's
    /// ring/deep-replay merge assumes is complete. The retained writer batch
    /// plus its bounded queue bound the unpersisted durable floor separately.
    fn evict_to_capacity(&mut self) {
        while self.over_bounds() {
            match self.slots.front() {
                // Front is unpinned -> safe to drop.
                Some(slot) if !slot.pinned => {
                    self.remove_at(0);
                }
                // A failed/slow write must not pin an unbounded lossy tail.
                // Live fan-out is independent of this bounded recent cache;
                // these classes are outside the durable replay guarantee.
                Some(_) => match self
                    .slots
                    .iter()
                    .position(|slot| !slot.env.delivery.is_durable())
                {
                    // Never the newest entry: the live edge stays in RAM.
                    Some(index)
                        if index + 1 < self.slots.len() || self.slots.len() > self.capacity =>
                    {
                        self.remove_at(index);
                    }
                    Some(_) => break,
                    None => break, // only the durable floor remains
                },
                None => break,
            }
        }
    }

    /// Return refcount-bumped handles to every entry strictly *after* `from`
    /// (the recent leg of the cursor replay), in total order. `from == None`
    /// returns the whole ring. Each handle is an [`Arc::clone`] — zero deep
    /// copy of the envelope or its headers. Used at the replay-then-live seam
    /// (§3.5).
    pub fn replay_after(&self, from: Option<Cursor>) -> Vec<Arc<Envelope>> {
        self.slots
            .iter()
            .filter(|slot| match &from {
                None => true,
                Some(c) => slot.env.cursor().is_after(c),
            })
            .map(|slot| Arc::clone(&slot.env))
            .collect()
    }

    /// **Card 7d5b6a65.** The cursor of the most-recent entry currently
    /// retained, if any. Used by [`EventRouter::head_cursor`] to
    /// implement the "subscribe from the live edge" attach shape — pass
    /// this cursor as `from_cursor` and the replay-after predicate
    /// returns nothing, so the subscriber sees only events strictly
    /// after the call (the agent-Monitor live-tail).
    pub fn newest_cursor(&self) -> Option<Cursor> {
        self.slots.back().map(|slot| slot.env.cursor())
    }

    /// **Card a1562dbc.** The cursor of the most-recent **`Durable`**
    /// entry currently retained, if any — the transcript tip as seen
    /// from RAM. Walks back-to-front, so the cost is bounded by the
    /// ring capacity (a constant), never by room history.
    ///
    /// Eviction pops from the front (oldest first), so whenever the
    /// ring holds *any* durable entry, the newest one in the ring IS
    /// the globally newest durable event on this channel — the sink
    /// only ever lags the ring (write-behind), never leads it.
    pub fn newest_durable_cursor(&self) -> Option<Cursor> {
        self.slots
            .iter()
            .rev()
            .find(|slot| slot.env.delivery.is_durable())
            .map(|slot| slot.env.cursor())
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

    /// Retained bytes (payload + headers + coalesce key) across all entries.
    pub fn retained_bytes(&self) -> usize {
        self.bytes
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

    fn env_sized(counter: u64, delivery: DeliveryClass, payload_len: usize) -> Envelope {
        let mut e = env_at(counter, delivery);
        e.payload = Bytes::from(vec![0u8; payload_len]);
        e
    }

    // what this catches: the airc daemon RSS plateau (card 309566f9). A ring bounded
    // only by count held 256 large envelopes per channel; the byte budget must evict
    // under it, keep the byte total exact as entries leave, and never drop the newest
    // entry (the live-edge cursor) even when that one entry alone exceeds the budget.
    #[test]
    fn byte_budget_evicts_oldest_and_keeps_the_newest() {
        let mut ring = HotRing::new(256, 1000);
        for c in 0..10 {
            ring.push(Arc::new(env_sized(c, DeliveryClass::EphemeralWindow, 300)));
        }
        assert_eq!(
            ring.len(),
            3,
            "300-byte entries under a 1000-byte budget keep three"
        );
        assert_eq!(ring.retained_bytes(), 900);
        assert_eq!(ring.oldest_cursor().unwrap().seq.counter, 7);

        ring.push(Arc::new(env_sized(
            10,
            DeliveryClass::EphemeralWindow,
            5000,
        )));
        assert_eq!(
            ring.len(),
            1,
            "an oversized newest entry evicts everything older"
        );
        assert_eq!(ring.retained_bytes(), 5000);
        assert_eq!(
            ring.newest_cursor().unwrap().seq.counter,
            10,
            "the live edge survives"
        );
    }

    // what this catches: the byte budget must not weaken the §3.8 floor. Pinned
    // (unpersisted durable) entries stay however many bytes they hold; once
    // persisted they become evictable and the budget reclaims them.
    #[test]
    fn byte_budget_never_drops_a_pinned_durable() {
        let mut ring = HotRing::new(256, 1000);
        for c in 0..4 {
            ring.push(Arc::new(env_sized(c, DeliveryClass::Durable, 600)));
        }
        assert_eq!(
            ring.len(),
            4,
            "2400 bytes over budget, but every entry is pinned"
        );
        assert_eq!(ring.pinned_count(), 4);
        for id in 1..=4 {
            ring.mark_persisted(EventId::from_u128(id));
        }
        assert_eq!(
            ring.len(),
            1,
            "persisted entries reclaim down to the newest"
        );
        assert_eq!(ring.retained_bytes(), 600);
    }

    #[test]
    fn evicts_unpinned_to_capacity() {
        let mut ring = HotRing::new(3, usize::MAX);
        for c in 0..5 {
            ring.push(Arc::new(env_at(c, DeliveryClass::EphemeralWindow)));
        }
        assert_eq!(ring.len(), 3, "non-durable entries evict to capacity");
        // oldest retained is counter 2 (0,1 evicted)
        assert_eq!(ring.oldest_cursor().unwrap().seq.counter, 2);
    }

    #[test]
    fn pinned_durable_is_not_evicted_until_persisted() {
        let mut ring = HotRing::new(2, usize::MAX);
        // Two durable, both unpersisted -> both pinned.
        ring.push(Arc::new(env_at(0, DeliveryClass::Durable)));
        ring.push(Arc::new(env_at(1, DeliveryClass::Durable)));
        // Third push wants to evict counter 0, but it's pinned -> ring grows.
        ring.push(Arc::new(env_at(2, DeliveryClass::Durable)));
        assert_eq!(
            ring.len(),
            3,
            "ring exceeds nominal capacity rather than drop an unpersisted Durable (§3.8 floor)"
        );
        assert_eq!(ring.pinned_count(), 3);

        // A later confirmed durable MUST stay behind the pinned head: deep
        // replay reads only before the oldest ring cursor. Lossy traffic may
        // be reclaimed instead, regardless of its live delivery class.
        ring.mark_persisted(EventId::from_u128(3)); // counter 2
        for (counter, delivery) in [
            (3, DeliveryClass::EphemeralWindow),
            (4, DeliveryClass::StreamChunk),
            (5, DeliveryClass::RequestResponse),
        ] {
            ring.push(Arc::new(env_at(counter, delivery)));
            assert_eq!(ring.len(), 3, "lossy traffic cannot grow the durable floor");
        }
        assert_eq!(
            ring.replay_after(None)
                .iter()
                .map(|env| env.seq.counter)
                .collect::<Vec<_>>(),
            [0, 1, 2],
            "pinned and later persisted durables form an intact suffix"
        );
        assert_eq!(ring.pinned_count(), 2);

        // Confirm persistence of the oldest -> now evictable.
        ring.mark_persisted(EventId::from_u128(1)); // counter 0
        assert_eq!(ring.len(), 2, "unpinned oldest reclaimed back to capacity");
        assert_eq!(ring.oldest_cursor().unwrap().seq.counter, 1);
    }

    /// Card a1562dbc: the durable tip skips non-durable tail entries
    /// (a StreamChunk burst after the last chat message must not move
    /// the transcript tip) and is `None` when no durable is retained.
    #[test]
    fn newest_durable_cursor_skips_non_durable_tail() {
        let mut ring = HotRing::new(10, usize::MAX);
        assert_eq!(ring.newest_durable_cursor(), None, "empty ring");

        ring.push(Arc::new(env_at(0, DeliveryClass::StreamChunk)));
        assert_eq!(
            ring.newest_durable_cursor(),
            None,
            "non-durable only ⇒ no transcript tip in RAM"
        );

        ring.push(Arc::new(env_at(1, DeliveryClass::Durable)));
        ring.push(Arc::new(env_at(2, DeliveryClass::StreamChunk)));
        ring.push(Arc::new(env_at(3, DeliveryClass::EphemeralWindow)));

        let tip = ring.newest_durable_cursor().expect("durable tip");
        assert_eq!(tip.seq.counter, 1, "tip is the durable, not the ring back");
        assert_eq!(
            ring.newest_cursor().expect("ring back").seq.counter,
            3,
            "sanity: the unfiltered newest IS the non-durable back"
        );
    }

    #[test]
    fn replay_after_returns_strictly_newer_in_order() {
        let mut ring = HotRing::new(10, usize::MAX);
        for c in 0..5 {
            ring.push(Arc::new(env_at(c, DeliveryClass::Durable)));
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
