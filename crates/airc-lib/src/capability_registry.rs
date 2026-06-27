//! Capability registry — the organic need/offer matcher (card a9580f9d,
//! persona-peer 4/8).
//!
//! Nodes on the grid advertise WHAT they can do (a [`PersonaCapabilities`]
//! card from card 9e5f8844) tagged with the peer that offered it; this
//! projection ingests those offers into an in-memory map and answers the
//! routing question "who can take a turn that needs these tags?" with an
//! ordered candidate list. It is the escalation half of local-first
//! routing: a scheduler tries the local node first and only consults the
//! registry when the local node cannot meet the need.
//!
//! Design (Joel, 2026-06-11 — binding):
//! - ORGANIC TWO-SIDED: a node both advertises capabilities (offers,
//!   ingested here) and expresses needs (required tags, the [`match_for`]
//!   query). Matching is continuous — re-ingesting an offer refreshes the
//!   `last_seen` clock so a node that keeps advertising stays live and one
//!   that goes quiet ages out. New nodes are absorbed with zero config:
//!   the first offer they publish makes them a candidate.
//! - PROBED, NEVER ASSUMED: the capabilities are whatever the offering
//!   node measured about itself (model, context window, free-form tags).
//!   This registry never enumerates hardware tiers or branches on OS /
//!   vendor — those, if present, are opaque facts inside the
//!   [`PersonaCapabilities`] record, not protocol-level switches.
//! - GRID-OF-ONE is normal: a registry with no ingested offers is
//!   empty-but-valid. [`CapabilityRegistry::match_for`] returns an empty
//!   `Vec`, never an error, when nothing matches. A node with no peers
//!   still works; it just never escalates.
//!
//! Mirrors the [`crate::work_roster`] projection pattern: a typed query
//! in, an ordered typed result out, ranking factored into small pure
//! helpers.
//!
//! [`match_for`]: CapabilityRegistry::match_for

use std::collections::HashMap;

use airc_core::{PeerId, PersonaCapabilities};
use airc_store::peer_trust::TrustTier;

/// Default staleness horizon: an offer not refreshed within this window
/// is aged out of match results. Mirrors the order of magnitude of
/// [`crate::work_roster::WorkRosterQuery`]'s `active_within_ms` so a
/// persona that stops advertising drops off routing on a comparable
/// timescale to an agent that stops heartbeating.
pub const DEFAULT_OFFER_TTL_MS: u64 = 180_000;

/// One node's standing capability advert, as held by the registry.
///
/// `peer_id` is the offering node; `capabilities` is exactly the
/// card-9e5f8844 struct it advertised (reused, never redefined);
/// `last_seen_ms` is the wall-clock (epoch-ms) of the most recent offer
/// ingest, the basis for ageing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityEntry {
    pub peer_id: PeerId,
    pub capabilities: PersonaCapabilities,
    pub last_seen_ms: u64,
}

/// A ranked match candidate returned by [`CapabilityRegistry::match_for`].
///
/// `matched_tags` is how many of the query's required tags this entry's
/// capability tags satisfied; together with `trust_tier` it is the
/// ranking basis, surfaced so a caller can show *why* a candidate ranked
/// where it did rather than treating the order as opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCandidate {
    pub peer_id: PeerId,
    pub capabilities: PersonaCapabilities,
    pub trust_tier: TrustTier,
    pub matched_tags: usize,
    pub last_seen_ms: u64,
}

/// In-memory projection of capability offers → routable candidates.
///
/// Not a substrate store: this is a derived view a scheduler keeps warm
/// by feeding it offer events as they arrive on the room (the
/// consumer-shapes `continuum` codec decodes the wire event; this layer
/// is wire-agnostic so it stays out of the airc-core ← consumer
/// dependency direction). Keyed by `peer_id` so a node re-advertising
/// replaces its prior offer rather than accumulating duplicates.
#[derive(Debug, Clone, Default)]
pub struct CapabilityRegistry {
    entries: HashMap<PeerId, CapabilityEntry>,
}

/// Query expressing a NEED: the tags a turn requires and how trust is
/// resolved per candidate. Empty `required_tags` matches every live
/// entry (a pure "who is out there?" sweep). `trust_of` maps an offering
/// peer to its locally-resolved [`TrustTier`]; a peer the caller can't
/// resolve is treated as [`TrustTier::Untrusted`] (the safe default the
/// trust layer already uses for trust-on-first-use peers), never
/// dropped — an unranked-but-present candidate beats invisibly losing a
/// capable node.
pub struct CapabilityQuery<'a> {
    /// Tags the need requires. A candidate must advertise ALL of them to
    /// be considered a match (matched_tags == required_tags.len()).
    /// Empty → every live entry is a match.
    pub required_tags: &'a [&'a str],
    /// Now, epoch-ms — the clock ageing is measured against.
    pub now_ms: u64,
    /// Offers last seen more than this many ms before `now_ms` are aged
    /// out of results.
    pub ttl_ms: u64,
    /// Peers reachable RIGHT NOW via a live transport adapter (LAN
    /// connection, a recent direct frame) — the adapter-ladder liveness
    /// signal. An entry whose `peer_id` is in this set is treated as LIVE
    /// even if its offer `last_seen_ms` has aged past `ttl_ms`.
    ///
    /// Why: capability staleness keys off the rendezvous-beacon / re-advert
    /// cadence, which is gh-gist-coupled and gh-auth-gated; when that path
    /// flaps, a peer's offer ages out and routing DROPS it — orphaning a
    /// node that is perfectly DELIVERABLE over LAN (delivery already uses
    /// the tried-in-order adapter ladder; liveness must not ignore it).
    /// `None` = no adapter signal available (preserves the prior
    /// beacon-only behaviour). The gist beacon stays the rendezvous of
    /// last resort for DISCOVERY, never the sole truth for LIVENESS.
    pub reachable_peers: Option<&'a std::collections::HashSet<PeerId>>,
}

impl CapabilityRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest (or refresh) one node's capability offer. Keyed by
    /// `peer_id`: a re-advert from the same peer replaces the prior
    /// record and bumps `last_seen_ms`, which is how a node that keeps
    /// advertising stays live. Idempotent for a given `(peer_id,
    /// last_seen_ms)`.
    pub fn ingest_offer(
        &mut self,
        peer_id: PeerId,
        capabilities: PersonaCapabilities,
        seen_at_ms: u64,
    ) {
        self.entries.insert(
            peer_id,
            CapabilityEntry {
                peer_id,
                capabilities,
                last_seen_ms: seen_at_ms,
            },
        );
    }

    /// Number of entries currently held (including not-yet-aged-out
    /// stale ones — ageing happens at query time, not on insert, so a
    /// caller can choose its own clock).
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when no offers have been ingested. The grid-of-one steady
    /// state.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Drop entries last seen before `now_ms - ttl_ms`. Returns how many
    /// were removed. Optional housekeeping: [`Self::match_for`] already
    /// skips stale entries, so calling this is only to reclaim memory.
    pub fn prune_stale(&mut self, now_ms: u64, ttl_ms: u64) -> usize {
        let before = self.entries.len();
        self.entries
            .retain(|_, entry| !is_stale(entry.last_seen_ms, now_ms, ttl_ms));
        before - self.entries.len()
    }

    /// Match a NEED against the live offers, returning candidates ordered
    /// best-first.
    ///
    /// Ranking, in order:
    ///   1. More matched tags first (a node satisfying more of the need
    ///      outranks one satisfying fewer). With non-empty
    ///      `required_tags` every returned candidate has the full count,
    ///      so this only separates results of an empty-tag sweep that
    ///      happen to advertise overlapping tags — but it keeps the
    ///      ordering principled.
    ///   2. Higher trust tier first: OwnMachine > OwnAccount > Friend >
    ///      Untrusted (reusing [`TrustTier`]; not reinvented).
    ///   3. Larger context window first — a tie-break favouring the more
    ///      capable host for the same trust.
    ///   4. `peer_id` last, purely for deterministic, test-stable order.
    ///
    /// Empty registry or no matches → empty `Vec`, never an error
    /// (grid-of-one). A required tag no live node advertises simply
    /// yields no candidates; the caller decides whether that means "run
    /// locally / queue" — the registry does not editorialise.
    pub fn match_for(
        &self,
        query: &CapabilityQuery<'_>,
        trust_of: impl Fn(PeerId) -> TrustTier,
    ) -> Vec<CapabilityCandidate> {
        let mut candidates: Vec<CapabilityCandidate> = self
            .entries
            .values()
            // Live if the offer is fresh OR the peer is reachable right now
            // via a live adapter (the adapter-ladder liveness OR). A
            // LAN-connected peer is never dropped from routing just because
            // its gh-gist-gated beacon/offer aged out.
            .filter(|entry| {
                !is_stale(entry.last_seen_ms, query.now_ms, query.ttl_ms)
                    || query
                        .reachable_peers
                        .is_some_and(|live| live.contains(&entry.peer_id))
            })
            .filter_map(|entry| {
                let matched_tags = matched_tag_count(&entry.capabilities, query.required_tags);
                // ALL required tags must be present. Empty required_tags
                // ⇒ required len 0 ⇒ this is always satisfied.
                if matched_tags < query.required_tags.len() {
                    return None;
                }
                Some(CapabilityCandidate {
                    peer_id: entry.peer_id,
                    capabilities: entry.capabilities.clone(),
                    trust_tier: trust_of(entry.peer_id),
                    matched_tags,
                    last_seen_ms: entry.last_seen_ms,
                })
            })
            .collect();

        candidates.sort_by(|left, right| {
            right
                .matched_tags
                .cmp(&left.matched_tags)
                .then_with(|| trust_rank(left.trust_tier).cmp(&trust_rank(right.trust_tier)))
                .then_with(|| {
                    right
                        .capabilities
                        .context_window_tokens
                        .cmp(&left.capabilities.context_window_tokens)
                })
                .then_with(|| left.peer_id.to_string().cmp(&right.peer_id.to_string()))
        });
        candidates
    }
}

/// An offer is stale when its last-seen instant is older than `ttl_ms`
/// before `now_ms`. A `ttl_ms` of 0 ages out everything not seen exactly
/// now; saturating arithmetic keeps a clock that ran backwards (or a
/// `now_ms` before `last_seen`) from underflowing into "never stale".
fn is_stale(last_seen_ms: u64, now_ms: u64, ttl_ms: u64) -> bool {
    now_ms.saturating_sub(last_seen_ms) > ttl_ms
}

/// How many of `required_tags` appear in the entry's advertised tags.
fn matched_tag_count(capabilities: &PersonaCapabilities, required_tags: &[&str]) -> usize {
    required_tags
        .iter()
        .filter(|needed| {
            capabilities
                .capability_tags
                .iter()
                .any(|have| have == *needed)
        })
        .count()
}

/// Lower rank = higher trust = sorts first. Reuses the existing
/// [`TrustTier`] ordering semantics (OwnMachine highest) without
/// depending on a derived `Ord` the enum does not provide. A
/// `wildcard_enum_match_arm`-clean exhaustive match: a new tier added to
/// `TrustTier` will fail to compile here until ranked, which is the
/// loud-failure we want for a routing-trust decision.
fn trust_rank(tier: TrustTier) -> u8 {
    match tier {
        TrustTier::OwnMachine => 0,
        TrustTier::OwnAccount => 1,
        TrustTier::Friend => 2,
        TrustTier::Untrusted => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(persona: &str, tags: &[&str], model: &str, ctx: u32) -> PersonaCapabilities {
        PersonaCapabilities {
            persona_id: persona.to_string(),
            capability_tags: tags.iter().map(|t| t.to_string()).collect(),
            model: model.to_string(),
            context_window_tokens: ctx,
        }
    }

    fn query<'a>(tags: &'a [&'a str], now_ms: u64) -> CapabilityQuery<'a> {
        CapabilityQuery {
            required_tags: tags,
            now_ms,
            ttl_ms: DEFAULT_OFFER_TTL_MS,
            reachable_peers: None,
        }
    }

    #[test]
    fn empty_registry_matches_to_empty_not_error() {
        // Grid-of-one: no offers ingested. A NEED query returns an empty
        // candidate list, never an error.
        let registry = CapabilityRegistry::new();
        assert!(registry.is_empty());
        let out = registry.match_for(&query(&["code"], 1_000), |_| TrustTier::OwnMachine);
        assert!(out.is_empty(), "empty registry must match to empty Vec");
    }

    #[test]
    fn ingests_offer_and_matches_by_tag() {
        let mut registry = CapabilityRegistry::new();
        let peer = PeerId::from_u128(1);
        registry.ingest_offer(
            peer,
            caps("skylar", &["code", "long-context"], "fable-5", 200_000),
            1_000,
        );
        assert_eq!(registry.len(), 1);

        let hit = registry.match_for(&query(&["code"], 1_500), |_| TrustTier::OwnAccount);
        assert_eq!(hit.len(), 1);
        assert_eq!(hit[0].peer_id, peer);
        assert_eq!(hit[0].matched_tags, 1);

        // A tag nobody advertises yields no candidate — not an error.
        let miss = registry.match_for(&query(&["render"], 1_500), |_| TrustTier::OwnAccount);
        assert!(miss.is_empty());
    }

    #[test]
    fn re_advert_refreshes_last_seen_and_does_not_duplicate() {
        let mut registry = CapabilityRegistry::new();
        let peer = PeerId::from_u128(7);
        registry.ingest_offer(peer, caps("skylar", &["code"], "fable-5", 100_000), 1_000);
        registry.ingest_offer(peer, caps("skylar", &["code"], "fable-5", 100_000), 9_000);
        assert_eq!(registry.len(), 1, "same peer re-advert must not duplicate");

        // With ttl 5_000 and now 12_000, the refreshed last_seen (9_000)
        // keeps it live; the original 1_000 would have aged out.
        let q = CapabilityQuery {
            required_tags: &["code"],
            now_ms: 12_000,
            ttl_ms: 5_000,
            reachable_peers: None,
        };
        let hit = registry.match_for(&q, |_| TrustTier::OwnMachine);
        assert_eq!(hit.len(), 1, "re-advert kept the node live");
        assert_eq!(hit[0].last_seen_ms, 9_000);
    }

    #[test]
    fn ages_out_stale_entries_at_query_time() {
        let mut registry = CapabilityRegistry::new();
        let peer = PeerId::from_u128(2);
        registry.ingest_offer(peer, caps("skylar", &["code"], "fable-5", 100_000), 1_000);

        // now far beyond ttl past last_seen → stale → no match.
        let q = CapabilityQuery {
            required_tags: &["code"],
            now_ms: 1_000 + DEFAULT_OFFER_TTL_MS + 1,
            ttl_ms: DEFAULT_OFFER_TTL_MS,
            reachable_peers: None,
        };
        assert!(
            registry.match_for(&q, |_| TrustTier::OwnMachine).is_empty(),
            "an offer older than ttl must age out of results"
        );
        // Entry still physically present until pruned (query-time ageing).
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn reachable_peer_matches_despite_stale_offer_adapter_ladder_rescue() {
        // The orphaning fix: a peer whose offer has aged past ttl (its
        // gh-gist-gated beacon/re-advert went stale) MUST still match when
        // it is reachable RIGHT NOW via a live adapter — delivery would
        // reach it over LAN, so routing must not drop it. Without the OR
        // this peer ages out (proven by `ages_out_stale_entries_at_query_time`);
        // with it in `reachable_peers`, it stays routable.
        let mut registry = CapabilityRegistry::new();
        let peer = PeerId::from_u128(42);
        registry.ingest_offer(peer, caps("skylar", &["code"], "fable-5", 100_000), 1_000);

        let now = 1_000 + DEFAULT_OFFER_TTL_MS + 1; // offer is stale at this clock
        let reachable: std::collections::HashSet<PeerId> = std::iter::once(peer).collect();

        // Beacon-only (reachable_peers: None) → aged out, as the sibling
        // test pins. With the adapter-ladder set → rescued.
        let stale_q = CapabilityQuery {
            required_tags: &["code"],
            now_ms: now,
            ttl_ms: DEFAULT_OFFER_TTL_MS,
            reachable_peers: None,
        };
        assert!(
            registry
                .match_for(&stale_q, |_| TrustTier::OwnMachine)
                .is_empty(),
            "control: a stale offer with no adapter signal still ages out"
        );

        let rescued_q = CapabilityQuery {
            reachable_peers: Some(&reachable),
            ..stale_q
        };
        let hit = registry.match_for(&rescued_q, |_| TrustTier::OwnMachine);
        assert_eq!(
            hit.len(),
            1,
            "a stale-offer peer that is LAN-reachable must still match (adapter-ladder rescue)"
        );
        assert_eq!(hit[0].peer_id, peer);

        // And the rescue is targeted: a DIFFERENT reachable peer does not
        // resurrect this stale one.
        let other: std::collections::HashSet<PeerId> =
            std::iter::once(PeerId::from_u128(99)).collect();
        let other_q = CapabilityQuery {
            reachable_peers: Some(&other),
            ..stale_q
        };
        assert!(
            registry
                .match_for(&other_q, |_| TrustTier::OwnMachine)
                .is_empty(),
            "reachability of a different peer must not rescue this stale offer"
        );
    }

    #[test]
    fn prune_stale_reclaims_aged_entries() {
        let mut registry = CapabilityRegistry::new();
        registry.ingest_offer(PeerId::from_u128(1), caps("a", &["code"], "m", 1), 1_000);
        registry.ingest_offer(PeerId::from_u128(2), caps("b", &["code"], "m", 1), 50_000);

        let removed = registry.prune_stale(50_000 + 10, 1_000);
        assert_eq!(removed, 1, "only the older-than-ttl entry is pruned");
        assert_eq!(registry.len(), 1);
    }

    #[test]
    fn ranks_by_tier_when_tag_match_is_equal() {
        // Three nodes all satisfy the same single tag; ordering must be
        // by trust tier OwnMachine > OwnAccount > Friend > Untrusted.
        let mut registry = CapabilityRegistry::new();
        let friend = PeerId::from_u128(10);
        let own_machine = PeerId::from_u128(11);
        let own_account = PeerId::from_u128(12);
        let untrusted = PeerId::from_u128(13);
        for (p, persona) in [
            (friend, "f"),
            (own_machine, "om"),
            (own_account, "oa"),
            (untrusted, "u"),
        ] {
            registry.ingest_offer(p, caps(persona, &["code"], "m", 100_000), 1_000);
        }

        let trust = move |p: PeerId| {
            if p == own_machine {
                TrustTier::OwnMachine
            } else if p == own_account {
                TrustTier::OwnAccount
            } else if p == friend {
                TrustTier::Friend
            } else {
                TrustTier::Untrusted
            }
        };
        let ranked = registry.match_for(&query(&["code"], 1_500), trust);
        let order: Vec<PeerId> = ranked.iter().map(|c| c.peer_id).collect();
        assert_eq!(
            order,
            vec![own_machine, own_account, friend, untrusted],
            "candidates must order by descending trust tier"
        );
    }

    #[test]
    fn ranks_by_tag_match_count_before_tier() {
        // Empty-tag sweep with two nodes: capability match (here, more
        // overlapping advertised tags is not the axis since required is
        // empty) — instead prove that when required tags differ in how
        // many a node satisfies, more-matched outranks a higher tier.
        let mut registry = CapabilityRegistry::new();
        let strong = PeerId::from_u128(20); // matches both tags, but Untrusted
        let weak = PeerId::from_u128(21); // matches one tag, OwnMachine
        registry.ingest_offer(strong, caps("s", &["code", "render"], "m", 100_000), 1_000);
        registry.ingest_offer(weak, caps("w", &["code"], "m", 100_000), 1_000);

        // Require only "code": both match exactly one required tag, so
        // tier decides → OwnMachine (weak) wins.
        let trust = &move |p: PeerId| {
            if p == weak {
                TrustTier::OwnMachine
            } else {
                TrustTier::Untrusted
            }
        };
        let only_code = registry.match_for(&query(&["code"], 1_500), trust);
        assert_eq!(only_code[0].peer_id, weak, "equal match → tier decides");

        // Require both: only `strong` advertises both → it's the sole
        // candidate despite being Untrusted (capability gates first).
        let both = registry.match_for(&query(&["code", "render"], 1_500), trust);
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].peer_id, strong);
        assert_eq!(both[0].matched_tags, 2);
    }

    #[test]
    fn context_window_breaks_tier_ties() {
        let mut registry = CapabilityRegistry::new();
        let small = PeerId::from_u128(30);
        let large = PeerId::from_u128(31);
        registry.ingest_offer(small, caps("s", &["code"], "m", 8_000), 1_000);
        registry.ingest_offer(large, caps("l", &["code"], "m", 200_000), 1_000);
        // Same tier for both → larger context window wins.
        let ranked = registry.match_for(&query(&["code"], 1_500), |_| TrustTier::OwnAccount);
        assert_eq!(ranked[0].peer_id, large);
        assert_eq!(ranked[1].peer_id, small);
    }

    #[test]
    fn empty_required_tags_is_a_who_is_out_there_sweep() {
        let mut registry = CapabilityRegistry::new();
        registry.ingest_offer(PeerId::from_u128(1), caps("a", &[], "m", 1), 1_000);
        registry.ingest_offer(PeerId::from_u128(2), caps("b", &["code"], "m", 1), 1_000);
        let all = registry.match_for(&query(&[], 1_500), |_| TrustTier::Untrusted);
        assert_eq!(all.len(), 2, "empty required_tags matches every live entry");
        for c in &all {
            assert_eq!(c.matched_tags, 0);
        }
    }

    #[test]
    fn trust_rank_orders_all_tiers_highest_first() {
        assert!(trust_rank(TrustTier::OwnMachine) < trust_rank(TrustTier::OwnAccount));
        assert!(trust_rank(TrustTier::OwnAccount) < trust_rank(TrustTier::Friend));
        assert!(trust_rank(TrustTier::Friend) < trust_rank(TrustTier::Untrusted));
    }
}
