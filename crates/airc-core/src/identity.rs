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
