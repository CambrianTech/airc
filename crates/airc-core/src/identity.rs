//! Per-peer identity card — the "user/account" abstraction airc owns.
//!
//! Identity in airc is fields-not-subsystem. There is no separate
//! account-management layer with passwords, session tables, or recovery
//! flows. A "user" is the peer behind the identity material; the rich
//! display fields here (`name`, `pronouns`, `role`, `bio`, `status`,
//! `fingerprint`, `integrations`) are what other peers see in scrollback,
//! presence headers, and `whois` output. Consumers like Continuum, OpenClaw,
//! and Hermes bind their user records to the durable airc `PeerId` and keep
//! any consumer-specific handles in `integrations`; pubkeys are identity key
//! material that can rotate under attestation.
//!
//! Field shape mirrors the Python `airc identity show` output one-to-one
//! so the Rust port doesn't redesign — same six top-level fields, same
//! defaults, same serde behavior (missing fields default to empty rather
//! than fail).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use crate::ids::PeerId;

/// What kind of entity this identity represents.
///
/// The substrate doesn't interpret `Role` beyond passing it through —
/// consumers use it to render appropriately (humans as chat bubbles,
/// personas as avatars, devices as system indicators, grid nodes as
/// compute peers). `Other(String)` is the escape hatch for consumer-
/// defined kinds the substrate doesn't enumerate; same extension
/// discipline as the headers map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// Unknown / not set. Default. Treat as "be conservative" for UI
    /// and permission decisions.
    #[default]
    Unknown,
    /// A flesh-and-blood person operating an airc client.
    Human,
    /// A cognition/agent persona — Continuum's personas, OpenClaw
    /// agents, etc.
    Persona,
    /// A coding agent (Claude Code session, Codex session, etc.).
    Agent,
    /// A device or local-only client (phone, headless terminal,
    /// browser extension).
    Device,
    /// A compute / grid node — render box, foundry host, GPU
    /// allocator. Identifies as a peer to negotiate work.
    GridNode,
    /// An automation or rule-driven bot.
    Bot,
    /// Consumer-defined kind the substrate doesn't enumerate.
    /// Use sparingly; prefer adding a variant if the kind is generic.
    Other(String),
}

/// A peer's user-facing identity card.
///
/// Constructed from the scope's `config.json` identity record (or its Rust-
/// store equivalent) and exposed by `airc identity show`, `airc whois`,
/// and presence/event surfaces.
///
/// Three load-bearing fields per the substrate design doc:
///   - `identity_id` — UUIDv4, immutable, mesh-stable. Permissions,
///     leases, replay records, audit log all cite this.
///   - `name`        — mutable display nick (can collide; substrate
///     disambiguates via `identity_id`).
///   - `role`        — kind classifier. Consumer-side rendering varies
///     by role; substrate just passes through.
///
/// Plus the existing display metadata (pronouns/bio/status/fingerprint/
/// integrations) mirroring the Python `airc identity show` output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Identity {
    /// Canonical UUIDv4 identifier. Mesh-stable, immutable for the life
    /// of the identity. Every cross-machine reference cites this.
    ///
    /// `serde(default)` so legacy records without this field (the
    /// existing Python+bash airc identity.json on disk doesn't have it
    /// yet) deserialize cleanly — the loader assigns a fresh one as
    /// part of the migration.
    #[serde(default)]
    pub identity_id: PeerId,
    /// Display nick. Other peers see this in `from=` and `whois` output.
    pub name: String,
    /// Pronouns (e.g. "they", "she", "he"). Free-form short string.
    /// Default empty when unset.
    #[serde(default)]
    pub pronouns: String,
    /// Kind classifier — human / persona / agent / device / grid_node
    /// / bot / other. Consumer-side renders accordingly; substrate
    /// passes through.
    ///
    /// Replaces the older free-form `role: String` field. Wire-shape
    /// stays compatible because the Role enum's snake_case serde
    /// emits `"human"`, `"persona"`, etc. — flat strings the Python
    /// airc carried. Tagged variants (`{"other": "..."}`) deserialize
    /// from JSON object form.
    #[serde(default)]
    pub role: Role,
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
    /// Format: short hex string matching the Python `airc identity show`
    /// `fingerprint:` line.
    #[serde(default)]
    pub fingerprint: String,
    /// Integration metadata for cross-system identity binding (e.g.
    /// GitHub login, Continuum persona id, OpenClaw user record, Hermes
    /// agent id). Map shape so consumers register their own keys without
    /// collision. airc never interprets the values; it just persists +
    /// transports. `BTreeMap` for deterministic serde ordering (stable
    /// diffs / cursors).
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
    /// to decide whether to nudge for completion. Mirrors the Python
    /// `_identity_needs_setup` heuristic, adapted for the typed Role
    /// enum: `Role::Unknown` is treated as "not set."
    pub fn is_complete(&self) -> bool {
        !self.pronouns.is_empty() && self.role != Role::Unknown && !self.bio.is_empty()
    }

    /// Mark / clear an "away" status. Empty string clears it (matches the
    /// Python `airc away ""` and `airc identity set --status ""` semantics).
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
        assert_eq!(id.role, Role::Unknown);
        assert_eq!(id.bio, "");
        assert_eq!(id.status, "");
        assert_eq!(id.fingerprint, "");
        assert!(id.integrations.is_empty());
        // identity_id defaults to a fresh UUIDv4 via PeerId::default().
        // The exact value is random; verify it's a v4 by extracting version.
        assert_eq!(
            id.identity_id.as_uuid().get_version(),
            Some(uuid::Version::Random),
            "identity_id default must be UUIDv4"
        );
    }

    #[test]
    fn is_complete_requires_pronouns_role_and_bio() {
        let mut id = Identity::new("alice");
        assert!(!id.is_complete(), "nick alone is not complete");

        id.pronouns = "they".into();
        assert!(!id.is_complete(), "pronouns alone is not complete");

        id.role = Role::Persona;
        assert!(!id.is_complete(), "pronouns + role still missing bio");

        id.bio = "designs things".into();
        assert!(id.is_complete(), "all three set marks complete");
    }

    #[test]
    fn role_unknown_is_treated_as_unset_by_is_complete() {
        let mut id = Identity::new("bob");
        id.pronouns = "he".into();
        id.bio = "tests roles".into();
        // Role default is Unknown → is_complete() must still return false
        assert_eq!(id.role, Role::Unknown);
        assert!(
            !id.is_complete(),
            "Role::Unknown counts as 'role not set' for the UX prompt"
        );
        id.role = Role::Human;
        assert!(id.is_complete());
    }

    #[test]
    fn identity_id_is_stable_under_rename() {
        // The three-field model: nick is mutable, identity_id is not.
        // Renaming the nick must not change the canonical identifier.
        let mut id = Identity::new("alice");
        let original_id = id.identity_id;
        id.name = "alice-renamed".into();
        assert_eq!(
            id.identity_id, original_id,
            "renaming must not mutate identity_id"
        );
    }

    #[test]
    fn role_serde_uses_snake_case_for_simple_variants() {
        let r = Role::Persona;
        let encoded = serde_json::to_value(&r).unwrap();
        assert_eq!(encoded, "persona");
        let decoded: Role = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, Role::Persona);

        // Two-word variant serializes snake_case
        let gn = Role::GridNode;
        assert_eq!(serde_json::to_value(&gn).unwrap(), "grid_node");
    }

    #[test]
    fn role_other_carries_consumer_string() {
        let r = Role::Other("forge.foundry".to_string());
        let encoded = serde_json::to_value(&r).unwrap();
        // Tagged variant: {"other": "..."}
        assert_eq!(encoded["other"], "forge.foundry");
        let decoded: Role = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, Role::Other("forge.foundry".into()));
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
        // Forward-compat: an Identity stored in scope config.json when only
        // the nick was set should deserialize cleanly — other fields default
        // to empty rather than fail.
        let stored = serde_json::json!({ "name": "bob" });
        let id: Identity = serde_json::from_value(stored).unwrap();
        assert_eq!(id.name, "bob");
        assert_eq!(id.pronouns, "");
        assert!(id.integrations.is_empty());

        // Round-trip a complete identity. Use a deterministic identity_id
        // so the comparison is stable across runs.
        let full = Identity {
            identity_id: PeerId::from_u128(0x550e8400_e29b_41d4_a716_446655440001),
            name: "claude-arch".into(),
            pronouns: "they".into(),
            role: Role::Persona,
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
    fn legacy_record_without_identity_id_gets_default_uuid() {
        // Forward-compat: a Python-airc-era identity.json carries no
        // `identity_id` field. Deserialize must succeed (via serde(default))
        // and the resulting Identity must have a freshly-minted UUIDv4.
        let stored = serde_json::json!({
            "name": "legacy-peer",
            "pronouns": "they",
            "role": "human",
            "bio": "migrated from python era",
        });
        let id: Identity = serde_json::from_value(stored).unwrap();
        assert_eq!(id.name, "legacy-peer");
        assert_eq!(id.role, Role::Human);
        assert_eq!(
            id.identity_id.as_uuid().get_version(),
            Some(uuid::Version::Random),
            "missing identity_id must be backfilled with a UUIDv4"
        );
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
        id.integrations
            .insert("hermes.agent_id".to_string(), "agent-7".to_string());
        assert_eq!(id.integrations.len(), 4);
        // Order is deterministic (BTreeMap) so serde encoding is stable —
        // cursors / replay records / diffs work consistently.
        let encoded = serde_json::to_string(&id.integrations).unwrap();
        assert!(encoded.find("continuum").unwrap() < encoded.find("github").unwrap());
    }

    #[test]
    fn external_consumers_bind_to_airc_peer_identity_not_pubkey_aliases() {
        let mut id = Identity::new("openclaw-bridge");
        // Consumer-defined kind that the substrate's Role enum doesn't
        // enumerate goes through Role::Other(...).
        id.role = Role::Other("external-agent-bridge".to_string());
        id.integrations.insert(
            "openclaw.user_id".to_string(),
            "oc-user-550e8400-e29b-41d4-a716-446655440000".to_string(),
        );
        id.integrations.insert(
            "hermes.agent_id".to_string(),
            "hermes-agent-calendar".to_string(),
        );

        assert_eq!(
            id.integrations["openclaw.user_id"],
            "oc-user-550e8400-e29b-41d4-a716-446655440000"
        );
        assert_eq!(id.integrations["hermes.agent_id"], "hermes-agent-calendar");
        assert!(
            !id.integrations.contains_key("pubkey"),
            "consumer records must not treat current key material as the durable user id"
        );
    }
}
