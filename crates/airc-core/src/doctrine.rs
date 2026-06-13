//! Room operating doctrine — the substrate-published "how we work
//! here" that every attaching agent loads on join.
//!
//! Card 2903a8ef (engine keystone — "the user is not the engine"):
//! AGENTS.md sitting in a repo doesn't reach agents in foreign scopes.
//! The fix is to publish the operating doctrine as a typed substrate
//! event so any agent attaching to the room receives it via the same
//! transcript subscribe path they already use for chat + lifecycle.
//!
//! This module defines just the wire shape — the publish path, the
//! projection that lets attachers query the current doctrine, and the
//! agent-side "load on attach" rendering ship in follow-up slices.
//! Same incremental pattern as PeerIdentityCard (card a63ad10a):
//! foundational type first, plumbing second.

use crate::ids::{PeerId, RoomId};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Typed room-doctrine + wall domain events. Wire shape mirrors
/// `airc_core::identity::IdentityEvent` and `airc_work::WorkEvent`:
/// internally tagged via serde so a JSON body always carries a `kind`
/// field consumers can switch on without parsing the entire payload.
///
/// Two variants today:
///   - `RoomDoctrinePublished` — the original engine-keystone slice
///     (card 2903a8ef): one-doc-per-room operating doctrine
///     auto-loaded on attach.
///   - `WallPostPublished` — card b4742d9c: the generalized "wall"
///     of pinned typed posts per room. Doctrine becomes the special
///     case of a wall post with `category = "doctrine"`; rules,
///     agendas, RAG knowledge, decisions, principles, and any
///     consumer-defined category ride the same mechanism.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DoctrineEvent {
    RoomDoctrinePublished(RoomDoctrinePublished),
    WallPostPublished(WallPostPublished),
}

/// A room's operating doctrine published on the substrate. Body is
/// the raw markdown (today: the contents of `AGENTS.md` from the
/// repo); `version` is a short content hash so attachers can detect
/// "doctrine I have differs from what just landed" without diffing
/// the full body. `published_by` is the peer that emitted it —
/// authority gradient is OUT of scope for this slice (per AGENTS.md
/// §6: no role-based dispatch); every peer has equal authority to
/// publish. Roster + the timestamps make "whose doctrine is current"
/// queryable by latest-write-wins on `published_at_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoomDoctrinePublished {
    /// The room this doctrine applies to.
    pub room_id: RoomId,
    /// Markdown content. Agents render verbatim on attach; downstream
    /// hooks/runners may inject as a system message into agent
    /// context.
    pub body: String,
    /// Short content hash (e.g. first 12 chars of a SHA-256 of `body`)
    /// so consumers can compare "what I last loaded" against "what's
    /// current" without storing the full body in their cache. Format
    /// is intentionally a free-form string today; the hash function +
    /// truncation are a stability concern for the publish slice
    /// (follow-up card), not the wire shape.
    pub version: String,
    /// Peer that emitted this version. Not gating — see module doc.
    pub published_by: PeerId,
    /// Monotonic emission time. Projection takes the highest
    /// `published_at_ms` per `room_id` (LWW; ties broken by the
    /// durable log's event order, which the projection sees
    /// naturally).
    pub published_at_ms: u64,
}

/// A pinned typed post on a room's wall — the living-document
/// mechanism that captures every room's purpose + evolving rules +
/// agreed decisions (card b4742d9c). Every room has a wall; new
/// attachees walk in and see the current set of pinned posts
/// (by category) on join.
///
/// The substrate guarantees delivery + replay; it does NOT define
/// what a post MEANS. `category` is a free-form string — common
/// values include `"doctrine"`, `"rules"`, `"agenda"`,
/// `"principles"`, `"rag"`, `"decision"`, but the substrate has no
/// opinion. `body` is opaque text (markdown is convention; consumers
/// may store JSON, references, whatever fits their schema).
///
/// Living-document via supersede chains:
///   - Each post has a stable `post_id`. Edits never mutate the
///     post in place — they publish a NEW WallPostPublished with
///     `supersedes = Some(prior.post_id)`.
///   - The wall projection (`Airc::wall_posts`) follows supersede
///     chains: only the latest version per chain shows up as
///     "currently pinned." History remains in the transcript so
///     anyone walking in months later can see how the wall evolved
///     ("Joel proposed X, we tried Y, settled on Z").
///   - Unpinning (archival) is a supersede whose body is empty (or
///     a tombstone marker the consumer recognizes).
///
/// `category` is informational — consumers filter on it via header
/// to subscribe to (e.g.) only `"doctrine"` updates. The substrate
/// doesn't enforce category semantics; if hermes invents
/// `"plan-step"` and openclaw invents `"tool-permission"`, they
/// coexist on the same wall without coordination.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WallPostPublished {
    /// The room whose wall this post is pinned on.
    pub room_id: RoomId,
    /// Stable id for this post across its supersede chain.
    /// A new pin generates a fresh UUID; revisions (supersedes)
    /// generate a NEW UUID and point at the prior one — so every
    /// post can be referenced unambiguously by URL/anchor even
    /// after it's been superseded.
    pub post_id: Uuid,
    /// Consumer-defined category. Common values: `"doctrine"`,
    /// `"rules"`, `"agenda"`, `"principles"`, `"rag"`. The substrate
    /// makes no inference from the string; it's the same shape as
    /// a header — middleware (continuum routers, agent renderers,
    /// hermes adapters) filter on it.
    pub category: String,
    /// Opaque post body. Markdown is the convention for
    /// human-readable categories; structured categories (e.g.
    /// `"rag"` indexes, `"decision"` records) ship JSON. No
    /// substrate-side schema.
    pub body: String,
    /// `Some(prior_post_id)` when this post revises an earlier one.
    /// `None` when this is a fresh pin. The wall projection walks
    /// the chain to surface only currently-pinned posts; history
    /// stays in the transcript for audit / "how did we get here"
    /// queries.
    pub supersedes: Option<Uuid>,
    /// Peer that emitted this version. Authority is flat in the
    /// substrate; consumers MAY enforce author rules in their own
    /// middleware (e.g. "only room-owner may pin in 'rules'").
    pub published_by: PeerId,
    /// Monotonic emission time. Projection ordering is by
    /// `published_at_ms` then durable-log order, same as other
    /// substrate events.
    pub published_at_ms: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn sample_card() -> RoomDoctrinePublished {
        RoomDoctrinePublished {
            room_id: RoomId::from_u128(7),
            body: "# AGENTS.md\nuse your own judgment".to_string(),
            version: "abc123def456".to_string(),
            published_by: PeerId::from_u128(42),
            published_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn room_doctrine_event_round_trips_through_serde() {
        let event = DoctrineEvent::RoomDoctrinePublished(sample_card());
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: DoctrineEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, decoded);
    }

    #[test]
    fn wire_shape_carries_kind_discriminator_for_consumer_codec() {
        // Consumers (agent renderers, future projection) switch on
        // the `kind` field without parsing the full body. Pin the
        // discriminator string so a serde change can't silently
        // rename it (same lesson as IdentityEvent — kink 0cfcc8db).
        let event = DoctrineEvent::RoomDoctrinePublished(sample_card());
        let value: Value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(
            value.get("kind").and_then(Value::as_str),
            Some("room_doctrine_published"),
            "wire kind discriminator must be stable",
        );
        assert!(value.get("room_id").is_some());
        assert!(value.get("body").is_some());
        assert!(value.get("version").is_some());
        assert!(value.get("published_by").is_some());
        assert!(value.get("published_at_ms").is_some());
    }

    #[test]
    fn unknown_doctrine_kind_surfaces_as_decode_error() {
        // Future-version event a current consumer doesn't know about
        // must surface as a decode error (not silently mis-decode
        // into the one known variant). Keeps the upgrade path honest.
        let raw = r#"{"kind":"room_doctrine_deprecated","room_id":"00000000-0000-0000-0000-000000000001"}"#;
        let result: Result<DoctrineEvent, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown kind must error");
    }

    #[test]
    fn empty_body_round_trips_unchanged() {
        // Defensive: a doctrine with empty body (e.g. an early
        // "doctrine cleared" semantic if we ever add one) must still
        // round-trip cleanly — the field is required, not optional.
        let mut card = sample_card();
        card.body.clear();
        let event = DoctrineEvent::RoomDoctrinePublished(card.clone());
        let json = serde_json::to_string(&event).unwrap();
        let decoded: DoctrineEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, DoctrineEvent::RoomDoctrinePublished(card));
    }

    // ========================================================================
    // Wall post tests — card b4742d9c.
    //
    // The substrate guarantees: serde round-trip stability, kind
    // discriminator stability, supersede-chain integrity at the wire
    // level. The PROJECTION (apply supersedes, return the currently-
    // pinned set per category) lives in `airc-lib`'s wall_posts
    // method — tests for that ride alongside it.
    // ========================================================================

    fn sample_wall_post() -> WallPostPublished {
        WallPostPublished {
            room_id: RoomId::from_u128(11),
            post_id: Uuid::from_u128(0x1234_5678_9abc_def0_1234_5678_9abc_def0),
            category: "rules".to_string(),
            body: "Branches: rust-rewrite is the integration branch. No PRs to main.".to_string(),
            supersedes: None,
            published_by: PeerId::from_u128(42),
            published_at_ms: 1_700_000_000_000,
        }
    }

    #[test]
    fn wall_post_event_round_trips_through_serde() {
        let event = DoctrineEvent::WallPostPublished(sample_wall_post());
        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: DoctrineEvent = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(event, decoded);
    }

    #[test]
    fn wall_post_wire_kind_discriminator_is_stable() {
        // Renaming the variant would break every consumer that filters
        // wall events by `kind`. Pin it so a serde change can't
        // silently shift the discriminator.
        let event = DoctrineEvent::WallPostPublished(sample_wall_post());
        let value: serde_json::Value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(
            value.get("kind").and_then(serde_json::Value::as_str),
            Some("wall_post_published"),
            "wall post wire kind must be stable across serde upgrades",
        );
        for field in [
            "room_id",
            "post_id",
            "category",
            "body",
            "supersedes",
            "published_by",
            "published_at_ms",
        ] {
            assert!(value.get(field).is_some(), "wire shape must carry {field}");
        }
    }

    #[test]
    fn wall_post_supersedes_round_trips_when_some_and_when_none() {
        // The supersede chain is the living-document mechanism. A
        // post with no supersede is a fresh pin; a post that
        // supersedes another links via the prior post_id. Both
        // shapes MUST round-trip cleanly.
        let fresh = WallPostPublished {
            supersedes: None,
            ..sample_wall_post()
        };
        let revision = WallPostPublished {
            post_id: Uuid::from_u128(2),
            supersedes: Some(sample_wall_post().post_id),
            body: "Branches: rust-rewrite is the integration branch. Main is frozen.".to_string(),
            ..sample_wall_post()
        };

        for evt in [
            DoctrineEvent::WallPostPublished(fresh),
            DoctrineEvent::WallPostPublished(revision),
        ] {
            let json = serde_json::to_string(&evt).unwrap();
            let decoded: DoctrineEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(evt, decoded);
        }
    }

    #[test]
    fn wall_posts_in_different_categories_coexist_independently() {
        // A "doctrine" post and a "rules" post in the same room are
        // independent items; the substrate doesn't impose a cross-
        // category ordering or supersede relationship. Verified by
        // confirming both wire shapes are valid + that the category
        // string is preserved verbatim (the substrate isn't allowed
        // to normalize / canonicalize it).
        for cat in [
            "doctrine",
            "rules",
            "agenda",
            "principles",
            "rag",
            "decision",
        ] {
            let post = WallPostPublished {
                category: cat.to_string(),
                ..sample_wall_post()
            };
            let json = serde_json::to_string(&DoctrineEvent::WallPostPublished(post.clone()))
                .expect("serialize");
            let value: serde_json::Value = serde_json::from_str(&json).unwrap();
            assert_eq!(
                value.get("category").and_then(serde_json::Value::as_str),
                Some(cat),
                "category string preserved verbatim for {cat}",
            );
        }
    }

    #[test]
    fn wall_post_category_can_be_consumer_defined_arbitrary_string() {
        // The substrate has no enum of categories. A future consumer
        // (hermes, continuum, openclaw, codex, a friend's adapter)
        // can invent its own category — `"plan-step"`,
        // `"tool-permission"`, `"recipe"` — and the substrate carries
        // it without rejecting. This test pins that property.
        for cat in [
            "hermes:plan-step",
            "continuum:capability-ad",
            "openclaw:tool-permission",
            "joel:reminders",
            "",
        ] {
            let post = WallPostPublished {
                category: cat.to_string(),
                ..sample_wall_post()
            };
            let event = DoctrineEvent::WallPostPublished(post);
            let json = serde_json::to_string(&event).unwrap();
            let decoded: DoctrineEvent = serde_json::from_str(&json).unwrap();
            assert_eq!(decoded, event);
        }
    }

    #[test]
    fn mixed_doctrine_and_wall_events_decode_via_same_enum() {
        // Wire-compatibility: a room's transcript will carry both
        // legacy RoomDoctrinePublished events (from before b4742d9c)
        // AND new WallPostPublished events. A consumer decoding the
        // transcript via DoctrineEvent MUST handle both variants
        // without coordinating with the publisher about which version
        // to expect.
        let doctrine_json =
            serde_json::to_string(&DoctrineEvent::RoomDoctrinePublished(sample_card())).unwrap();
        let wall_json =
            serde_json::to_string(&DoctrineEvent::WallPostPublished(sample_wall_post())).unwrap();

        let doctrine_decoded: DoctrineEvent = serde_json::from_str(&doctrine_json).unwrap();
        let wall_decoded: DoctrineEvent = serde_json::from_str(&wall_json).unwrap();

        assert!(matches!(
            doctrine_decoded,
            DoctrineEvent::RoomDoctrinePublished(_)
        ));
        assert!(matches!(wall_decoded, DoctrineEvent::WallPostPublished(_)));
    }
}
