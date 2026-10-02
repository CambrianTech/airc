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

/// Header an `EphemeralLatest` envelope may carry to set ITS OWN time-to-live in
/// milliseconds, overriding the cache default. Read once, at coalesce, from the
/// header — never the payload. Presence needs it: agent heartbeats beat every
/// 60 s against a 30 s router default, so without it a live peer would expire
/// between beats (airc#1341). A malformed value is ignored (the default stands).
pub const HEADER_EPHEMERAL_TTL_MS: &str = "airc.ephemeral_ttl_ms";

/// One coalesced entry: the latest envelope for its key, the wall-clock time it
/// landed, and the TTL it lives by (its own header's, else the cache default).
struct Entry {
    env: Arc<Envelope>,
    stored_at_ms: u64,
    ttl_ms: u64,
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
        let ttl_ms = env
            .headers
            .get(HEADER_EPHEMERAL_TTL_MS)
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(self.ttl_ms); // JUSTIFIED unwrap_or: no (or malformed) per-envelope TTL = the cache default
        self.latest.insert(
            key,
            Entry {
                env,
                stored_at_ms: now_ms,
                ttl_ms,
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
        self.latest
            .retain(|_, e| !Self::is_expired(e.ttl_ms, e.stored_at_ms, now_ms));
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
        Self::is_expired(e.ttl_ms, e.stored_at_ms, now_ms)
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

    // what this catches (airc#1341): presence beats every 60 s against the router's
    // 30 s default TTL, so a live peer would drop out of the snapshot between beats
    // and every roster would flicker empty. An envelope's own TTL header must
    // govern ITS entry, and must not stretch a neighbour's that did not ask.
    #[test]
    fn an_envelope_ttl_header_governs_its_own_entry_only() {
        let mut cache = EphemeralCache::new(30_000);
        let mut beat = presence(1, "presence:alice");
        beat.headers
            .insert(HEADER_EPHEMERAL_TTL_MS.to_string(), "180000".to_string());
        cache.coalesce(Arc::new(beat), 0);
        cache.coalesce(Arc::new(presence(2, "typing:bob")), 0);
        let live = |at| {
            let mut keys: Vec<_> = cache
                .snapshot(at)
                .iter()
                .filter_map(|e| e.coalesce_key.clone())
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(
            live(60_000),
            vec!["presence:alice"],
            "past the default, only the long-lived entry survives"
        );
        assert_eq!(
            cache.sweep(60_000),
            1,
            "sweep honours the per-entry TTL too"
        );
        assert!(cache.get("presence:alice", 179_999).is_some());
        assert!(
            cache.get("presence:alice", 180_000).is_none(),
            "its own TTL still ends it"
        );
        // A malformed header is ignored, never a crash or an immortal entry.
        let mut odd = presence(3, "presence:carol");
        odd.headers
            .insert(HEADER_EPHEMERAL_TTL_MS.to_string(), "soon".to_string());
        cache.coalesce(Arc::new(odd), 0);
        assert!(
            cache.get("presence:carol", 30_000).is_none(),
            "malformed TTL falls back to the default"
        );
    }

    #[test]
    fn distinct_keys_do_not_coalesce() {
        let mut cache = EphemeralCache::new(0);
        cache.coalesce(Arc::new(presence(1, "typing:alice")), 0);
        cache.coalesce(Arc::new(presence(2, "typing:bob")), 0);
        assert_eq!(cache.live_len(0), 2);
    }
}
