//! Per-peer identity card — the "user/account" abstraction airc owns.
//!
//! Identity in airc is fields-not-subsystem. There is no separate
//! account-management layer with passwords, session tables, or recovery
//! flows. A "user" is the peer behind the identity material; the rich
//! display fields here (`name`, `pronouns`, `role`, `bio`, `status`,
//! `fingerprint`, `integrations`) are what other peers see in scrollback,
//! presence headers, and `whois` output. Consumers like continuum, OpenClaw,
//! and Hermes bind their user records to airc identities by pubkey rather
//! than maintaining parallel account semantics.
//!
//! Field shape is the stable `airc identity show` contract: same six
//! top-level fields, same defaults, same serde behavior (missing fields
//! default to empty rather than fail).

use crate::PeerId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// A peer's user-facing identity card.
///
/// Constructed from the local identity store row and exposed by
/// `airc identity show`, `airc whois`, and presence/event surfaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Identity {
    /// Display nick. Other peers see this in `from=` and `whois` output.
    pub name: String,
    /// Pronouns (e.g. "they", "she", "he"). Free-form short string.
    /// Default empty when unset.
    #[serde(default)]
    pub pronouns: String,
    /// One-tag role (e.g. "claude-arch", "device-link-coordinator",
    /// "human"). Free-form. Default empty.
    #[serde(default)]
    pub role: String,
    /// One-sentence bio describing what this identity does / focuses on.
    /// Free-form. Default empty.
    #[serde(default)]
    pub bio: String,
    /// IRC /away-style transient status. Cleared with empty string.
    /// Default empty (= not away).
    #[serde(default)]
    pub status: String,
    /// Short identity fingerprint derived from the peer's pubkey.
    /// Computed by airc identity tooling, not authored by the user.
    /// Format: short hex string matching the `airc identity show`
    /// `fingerprint:` line.
    #[serde(default)]
    pub fingerprint: String,
    /// Integration metadata for cross-system identity binding (e.g.
    /// GitHub login, Continuum persona id, OpenClaw user record). Map
    /// shape so consumers register their own keys without collision.
    /// airc never interprets the values; it just persists + transports.
    /// `BTreeMap` for deterministic serde ordering (stable diffs / cursors).
    #[serde(default)]
    pub integrations: BTreeMap<String, String>,
}

impl Identity {
    /// Construct an Identity with just a nick. All other fields default to
    /// empty/unset — same as a fresh `airc identity` with only the
    /// auto-derived nick filled in.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            ..Default::default()
        }
    }

    /// Is this identity "minimally set up" — has the user provided at
    /// least pronouns + role + bio? Used by the `airc identity` UX prompt
    /// to decide whether to nudge for completion.
    pub fn is_complete(&self) -> bool {
        !self.pronouns.is_empty() && !self.role.is_empty() && !self.bio.is_empty()
    }

    /// Mark / clear an "away" status. Empty string clears it (matches the
    /// `airc away ""` and `airc identity set --status ""` semantics).
    pub fn set_status(&mut self, status: impl Into<String>) {
        self.status = status.into();
    }

    /// Is this identity currently in an away/status-set state?
    pub fn is_away(&self) -> bool {
        !self.status.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_name_and_defaults_other_fields() {
        let id = Identity::new("claude-arch");
        assert_eq!(id.name, "claude-arch");
        assert_eq!(id.pronouns, "");
        assert_eq!(id.role, "");
        assert_eq!(id.bio, "");
        assert_eq!(id.status, "");
        assert_eq!(id.fingerprint, "");
        assert!(id.integrations.is_empty());
    }

    #[test]
    fn is_complete_requires_pronouns_role_and_bio() {
        let mut id = Identity::new("alice");
        assert!(!id.is_complete(), "nick alone is not complete");

        id.pronouns = "they".into();
        assert!(!id.is_complete(), "pronouns alone is not complete");

        id.role = "architect".into();
        assert!(!id.is_complete(), "pronouns + role still missing bio");

        id.bio = "designs things".into();
        assert!(id.is_complete(), "all three set marks complete");
    }

    #[test]
    fn away_status_lifecycle() {
        let mut id = Identity::new("alice");
        assert!(!id.is_away(), "default is not away");

        id.set_status("lunch");
        assert!(id.is_away());
        assert_eq!(id.status, "lunch");

        // Empty string clears — mirrors `airc away ""` semantics.
        id.set_status("");
        assert!(!id.is_away());
    }

    #[test]
    fn serde_roundtrips_with_defaults_for_unset_fields() {
        // Forward-compat: an Identity stored when only the nick was set
        // should deserialize cleanly — other fields default to empty
        // rather than fail.
        let stored = serde_json::json!({ "name": "bob" });
        let id: Identity = serde_json::from_value(stored).unwrap();
        assert_eq!(id.name, "bob");
        assert_eq!(id.pronouns, "");
        assert!(id.integrations.is_empty());

        // Round-trip a complete identity.
        let full = Identity {
            name: "claude-arch".into(),
            pronouns: "they".into(),
            role: "architect".into(),
            bio: "designs the airc-rust substrate".into(),
            status: "deep work".into(),
            fingerprint: "abcd1234".into(),
            integrations: {
                let mut m = BTreeMap::new();
                m.insert("github".to_string(), "joelteply".to_string());
                m
            },
        };
        let encoded = serde_json::to_value(&full).unwrap();
        let decoded: Identity = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, full);
    }

    #[test]
    fn integrations_map_namespace_collision_is_avoided() {
        // Consumers register their own keys; no airc-owned keys exist in
        // `integrations` because airc never interprets the values. Two
        // consumers can coexist.
        let mut id = Identity::new("multi-consumer");
        id.integrations
            .insert("github".to_string(), "joelteply".to_string());
        id.integrations.insert(
            "continuum.persona_state_ref".to_string(),
            "uuid-abc".to_string(),
        );
        id.integrations
            .insert("openclaw.user_id".to_string(), "42".to_string());
        assert_eq!(id.integrations.len(), 3);
        // Order is deterministic (BTreeMap) so serde encoding is stable —
        // cursors / replay records / diffs work consistently.
        let encoded = serde_json::to_string(&id.integrations).unwrap();
        assert!(encoded.find("continuum").unwrap() < encoded.find("github").unwrap());
    }
}

// ─────────────────────────────────────────────────────────────────────
// Wire event: PeerIdentityCard
//
// First slice of the identity-roster substrate (card a63ad10a; parent
// af40f46d). Defines the typed event a peer publishes so other peers
// in the room can populate a roster of "who is here." Today the body
// inlines the full `Identity` payload for slice-1 simplicity; the
// long-term shape per AGENTS.md §9 + card 5842c35c is "carry a
// Continuum persona id and let consumers fetch the persona" — that
// refactor can happen additively without changing the kind tag.
// ─────────────────────────────────────────────────────────────────────

/// Typed identity-domain events. Wire shape mirrors `airc_work::WorkEvent`:
/// internally tagged via serde so a JSON body always carries a `kind`
/// field consumers can switch on without parsing the entire payload.
///
/// One variant today (`PeerIdentityCard`); future identity-revocation
/// or identity-forking events can join here without breaking the
/// consumer codec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IdentityEvent {
    PeerIdentityCard(PeerIdentityCard),
}

/// One peer's published identity card — the `Identity` payload plus
/// the publishing peer's id and an emission timestamp. Projections
/// (roster) use `emitted_at_ms` for latest-write-wins replacement on
/// the same peer_id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PeerIdentityCard {
    /// The peer this card describes. Also the publisher (peers
    /// only publish their own cards; verification is by signed envelope).
    pub peer_id: PeerId,
    /// Full identity payload — name, pronouns, role, bio, status,
    /// fingerprint, integrations. See [`Identity`].
    pub identity: Identity,
    /// Monotonic emission time. Roster projection takes the highest
    /// `emitted_at_ms` per peer_id (LWW; ties broken by the durable
    /// log's event order, which the projection sees naturally).
    pub emitted_at_ms: u64,
}

#[cfg(test)]
mod wire_event_tests {
    use super::*;
    use serde_json::Value;

    fn sample_identity() -> Identity {
        let mut id = Identity::new("alice");
        id.pronouns = "she".into();
        id.role = "claude-arch".into();
        id.bio = "owner-daemon architect".into();
        id.fingerprint = "ff00aa11".into();
        id.integrations
            .insert("github".into(), "alice-gh".into());
        id
    }

    #[test]
    fn peer_identity_card_round_trips_through_serde() {
        let card = PeerIdentityCard {
            peer_id: PeerId::from_u128(42),
            identity: sample_identity(),
            emitted_at_ms: 1_700_000_000_000,
        };
        let event = IdentityEvent::PeerIdentityCard(card.clone());

        let json = serde_json::to_string(&event).expect("serialize");
        let decoded: IdentityEvent = serde_json::from_str(&json).expect("deserialize");
        match decoded {
            IdentityEvent::PeerIdentityCard(got) => assert_eq!(got, card),
        }
    }

    #[test]
    fn wire_shape_carries_kind_discriminator_for_consumer_codec() {
        // Other consumers (event_render, future roster projection)
        // switch on the `kind` field without parsing the full body.
        // Pin the discriminator string so a future serde change can't
        // silently rename it.
        let event = IdentityEvent::PeerIdentityCard(PeerIdentityCard {
            peer_id: PeerId::from_u128(1),
            identity: Identity::new("bob"),
            emitted_at_ms: 0,
        });
        let value: Value = serde_json::to_value(&event).expect("to_value");
        assert_eq!(
            value.get("kind").and_then(Value::as_str),
            Some("peer_identity_card"),
            "wire kind discriminator must be stable",
        );
        // Identity fields are flattened at the top level alongside
        // peer_id / emitted_at_ms — mirrors WorkEvent body shape.
        assert!(value.get("peer_id").is_some());
        assert!(value.get("identity").is_some());
        assert!(value.get("emitted_at_ms").is_some());
    }

    #[test]
    fn unknown_identity_kind_surfaces_as_decode_error() {
        // Future-version event a current consumer doesn't know about
        // must surface as a decode error (not silently mis-decode into
        // the one known variant). Keeps the upgrade path honest:
        // consumers explicitly choose how to handle unknown kinds.
        let raw = r#"{"kind":"identity_revoked","peer_id":"00000000-0000-0000-0000-000000000001"}"#;
        let result: Result<IdentityEvent, _> = serde_json::from_str(raw);
        assert!(result.is_err(), "unknown kind must error");
    }
}

