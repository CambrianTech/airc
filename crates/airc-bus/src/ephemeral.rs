//! Coalesced ephemeral cache — latest-wins by `(channel, coalesce_key)` (§3.4).
//!
//! `EphemeralLatest` traffic (presence, typing, resource-pressure, signaling
//! churn, avatar pose at 60-90Hz) is coalesced **latest-wins** in an in-memory
//! map with TTL — *not* one row per update. 1000 typing updates → one latest
//! value. The firehose that would kill a DB never reaches the durable tier.
//!
//! It's a projection, not a log: rebuildable from recent events. Entries
//! expire after `ttl_ms` measured against an injectable [`crate::Clock`].
//!
//! Not internally synchronized — the router owns it behind a shard mutex and
//! never holds that lock across `.await`.

use std::collections::HashMap;
use std::sync::Arc;

use crate::envelope::Envelope;

/// One coalesced entry: the latest envelope for its key, with the wall-clock
/// time it landed (for TTL).
struct Entry {
    env: Arc<Envelope>,
    stored_at_ms: u64,
}

/// Latest-wins ephemeral cache for one channel, keyed by `coalesce_key`.
pub struct EphemeralCache {
    /// coalesce_key -> latest entry.
    latest: HashMap<String, Entry>,
    ttl_ms: u64,
}

impl EphemeralCache {
    /// Construct with a TTL in milliseconds. `ttl_ms == 0` means entries never
    /// expire by time (still latest-wins).
    pub fn new(ttl_ms: u64) -> Self {
        Self {
            latest: HashMap::new(),
            ttl_ms,
        }
    }

    /// Coalesce an `EphemeralLatest` envelope: overwrite the entry for its
    /// `coalesce_key`. An envelope without a `coalesce_key` is keyed by its
    /// `event_id` (degenerate — no coalescing, but still bounded by TTL).
    pub fn coalesce(&mut self, env: Arc<Envelope>, now_ms: u64) {
        let key = env
            .coalesce_key
            .clone()
            .unwrap_or_else(|| env.event_id.to_string());
        self.latest.insert(
            key,
            Entry {
                env,
                stored_at_ms: now_ms,
            },
        );
    }

    /// The current latest value for `key`, if present and not TTL-expired.
    pub fn get(&self, key: &str, now_ms: u64) -> Option<&Arc<Envelope>> {
        self.latest.get(key).and_then(|e| {
            if self.expired(e, now_ms) {
                None
            } else {
                Some(&e.env)
            }
        })
    }

    /// Drop TTL-expired entries; return how many were removed. Callers can
    /// drive this on a cadence; `get`/`snapshot` also honor TTL so a missed
    /// sweep is never observable.
    pub fn sweep(&mut self, now_ms: u64) -> usize {
        let before = self.latest.len();
        let ttl_ms = self.ttl_ms;
        self.latest
            .retain(|_, e| !Self::is_expired(ttl_ms, e.stored_at_ms, now_ms));
        before - self.latest.len()
    }

    /// All currently-live (non-expired) latest values, for replay-on-attach of
    /// the ephemeral projection. Each handle is an [`Arc::clone`] — zero deep
    /// copy.
    pub fn snapshot(&self, now_ms: u64) -> Vec<Arc<Envelope>> {
        self.latest
            .values()
            .filter(|e| !self.expired(e, now_ms))
            .map(|e| Arc::clone(&e.env))
            .collect()
    }

    /// Number of live (non-expired) entries.
    pub fn live_len(&self, now_ms: u64) -> usize {
        self.latest
            .values()
            .filter(|e| !self.expired(e, now_ms))
            .count()
    }

    fn expired(&self, e: &Entry, now_ms: u64) -> bool {
        Self::is_expired(self.ttl_ms, e.stored_at_ms, now_ms)
    }

    fn is_expired(ttl_ms: u64, stored_at_ms: u64, now_ms: u64) -> bool {
        ttl_ms != 0 && now_ms.saturating_sub(stored_at_ms) >= ttl_ms
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::envelope::{DeliveryClass, Kind};
    use airc_core::{ClientId, EventId, PeerId, RoomId};
    use bytes::Bytes;

    fn presence(seq_marker: u8, key: &str) -> Envelope {
        Envelope::new(
            RoomId::from_u128(1),
            (PeerId::from_u128(1), ClientId::from_u128(1)),
            Kind::Signal,
            DeliveryClass::EphemeralLatest,
            Bytes::copy_from_slice(&[seq_marker]),
        )
        .with_event_id(EventId::from_u128(seq_marker as u128 + 1))
        .with_coalesce_key(key)
    }

    #[test]
    fn latest_wins_by_coalesce_key() {
        let mut cache = EphemeralCache::new(0);
        for i in 0..1000u32 {
            cache.coalesce(Arc::new(presence((i % 256) as u8, "typing:alice")), 100);
        }
        assert_eq!(cache.live_len(100), 1, "1000 updates coalesce to one entry");
        let last = presence((999 % 256) as u8, "typing:alice");
        assert_eq!(
            cache.get("typing:alice", 100).unwrap().payload,
            last.payload
        );
    }

    #[test]
    fn ttl_expires_entries() {
        let mut cache = EphemeralCache::new(50);
        cache.coalesce(Arc::new(presence(1, "k")), 1000);
        assert!(cache.get("k", 1049).is_some(), "within TTL");
        assert!(
            cache.get("k", 1050).is_none(),
            "at/after TTL boundary expires"
        );
        let removed = cache.sweep(1050);
        assert_eq!(removed, 1);
    }

    #[test]
    fn distinct_keys_do_not_coalesce() {
        let mut cache = EphemeralCache::new(0);
        cache.coalesce(Arc::new(presence(1, "typing:alice")), 0);
        cache.coalesce(Arc::new(presence(2, "typing:bob")), 0);
        assert_eq!(cache.live_len(0), 2);
    }
}
