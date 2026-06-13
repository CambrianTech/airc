//! Typed persona capability metadata carried via `Identity.integrations`.
//!
//! Card 9e5f8844 (persona-peer 1/8). A Continuum persona running as an
//! AIRC peer advertises WHAT it is (persona id, capability tags, model,
//! context window) on the existing [`Identity`] card rather than via a
//! new subsystem: the `integrations` map is the consumer-keyed slot the
//! substrate already persists and transports without interpreting (see
//! [`Identity::integrations`]). This module owns the ONE key Continuum
//! uses ([`PERSONA_CAPABILITIES_KEY`]) and the typed encode/decode so
//! every consumer reads the same shape instead of hand-parsing JSON
//! out of the map.
//!
//! Loud-failure doctrine: a present-but-undecodable value is an error
//! surfaced to the caller ([`PersonaCapabilitiesError::Decode`]), never
//! a silent `None` — a corrupt capability advert mis-read as "no
//! capabilities" would route persona turns to a peer that can't take
//! them, invisibly. Absent key is the honest `Ok(None)`.

use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::identity::Identity;

/// The `Identity.integrations` key under which a persona peer's
/// capability metadata is stored. Versioned so a future incompatible
/// shape gets a NEW key (`continuum.persona.v2`) instead of silently
/// changing what `v1` decodes to.
pub const PERSONA_CAPABILITIES_KEY: &str = "continuum.persona.v1";

/// What a persona peer advertises about itself on its identity card.
///
/// Stored JSON-encoded under [`PERSONA_CAPABILITIES_KEY`] in
/// [`Identity::integrations`]; read back with
/// [`PersonaCapabilities::read_from_identity`]. The substrate carries
/// it opaquely — only persona-aware consumers (Continuum's spawn loop,
/// manager-hat routing) decode it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PersonaCapabilities {
    /// Continuum persona id this peer embodies (e.g. "skylar").
    pub persona_id: String,
    /// Free-form capability tags the routing layer matches on
    /// (e.g. "code", "render", "long-context"). Default empty —
    /// a persona with no advertised tags is still a valid persona.
    #[serde(default)]
    pub capability_tags: Vec<String>,
    /// Model identifier backing this persona (e.g. "fable-5").
    pub model: String,
    /// Context window of the backing model, in tokens. Routing uses
    /// this to keep oversized activities off undersized personas.
    pub context_window_tokens: u32,
}

/// Loud-failure errors for the typed integrations accessor.
#[derive(Debug)]
pub enum PersonaCapabilitiesError {
    /// Serializing the capabilities to JSON failed (pathological —
    /// the struct is plain data — but never swallowed).
    Encode(serde_json::Error),
    /// The key was PRESENT but its value did not decode as
    /// [`PersonaCapabilities`]. Carries the raw value so the operator
    /// sees what was actually stored, not just "it broke".
    Decode {
        /// The undecodable raw value found under the key.
        raw: String,
        /// The underlying JSON error.
        source: serde_json::Error,
    },
}

impl fmt::Display for PersonaCapabilitiesError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(source) => {
                write!(f, "persona capabilities failed to encode: {source}")
            }
            Self::Decode { raw, source } => write!(
                f,
                "persona capabilities under {PERSONA_CAPABILITIES_KEY:?} \
                 failed to decode: {source}; raw value: {raw:?}",
            ),
        }
    }
}

impl Error for PersonaCapabilitiesError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Encode(source) | Self::Decode { source, .. } => Some(source),
        }
    }
}

impl PersonaCapabilities {
    /// Write these capabilities into an integrations map under
    /// [`PERSONA_CAPABILITIES_KEY`], replacing any prior value.
    pub fn write_to_integrations(
        &self,
        integrations: &mut BTreeMap<String, String>,
    ) -> Result<(), PersonaCapabilitiesError> {
        let encoded = serde_json::to_string(self).map_err(PersonaCapabilitiesError::Encode)?;
        integrations.insert(PERSONA_CAPABILITIES_KEY.to_string(), encoded);
        Ok(())
    }

    /// Typed read from an integrations map.
    ///
    /// - Key absent → `Ok(None)` — this peer advertises no persona.
    /// - Key present and valid → `Ok(Some(capabilities))`.
    /// - Key present but undecodable → `Err(Decode { .. })`, loudly.
    pub fn read_from_integrations(
        integrations: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, PersonaCapabilitiesError> {
        let Some(raw) = integrations.get(PERSONA_CAPABILITIES_KEY) else {
            return Ok(None);
        };
        serde_json::from_str(raw)
            .map(Some)
            .map_err(|source| PersonaCapabilitiesError::Decode {
                raw: raw.clone(),
                source,
            })
    }

    /// Convenience: write onto an [`Identity`] card's integrations map.
    pub fn write_to_identity(
        &self,
        identity: &mut Identity,
    ) -> Result<(), PersonaCapabilitiesError> {
        self.write_to_integrations(&mut identity.integrations)
    }

    /// Convenience: typed read off an [`Identity`] card. Same
    /// absent/valid/corrupt contract as
    /// [`Self::read_from_integrations`].
    pub fn read_from_identity(
        identity: &Identity,
    ) -> Result<Option<Self>, PersonaCapabilitiesError> {
        Self::read_from_integrations(&identity.integrations)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> PersonaCapabilities {
        PersonaCapabilities {
            persona_id: "skylar".to_string(),
            capability_tags: vec!["code".to_string(), "long-context".to_string()],
            model: "fable-5".to_string(),
            context_window_tokens: 200_000,
        }
    }

    #[test]
    fn roundtrips_through_identity_integrations() {
        let mut identity = Identity::new("skylar-peer");
        sample()
            .write_to_identity(&mut identity)
            .expect("write capabilities");

        // The value rides the existing consumer-keyed map — no new
        // Identity field, so every store/wire path that already
        // carries `integrations` carries this too.
        assert!(identity.integrations.contains_key(PERSONA_CAPABILITIES_KEY));

        let read = PersonaCapabilities::read_from_identity(&identity)
            .expect("decode must succeed")
            .expect("capabilities must be present");
        assert_eq!(read, sample());
    }

    #[test]
    fn write_replaces_prior_value() {
        let mut integrations = BTreeMap::new();
        sample()
            .write_to_integrations(&mut integrations)
            .expect("first write");
        let updated = PersonaCapabilities {
            model: "fable-6".to_string(),
            ..sample()
        };
        updated
            .write_to_integrations(&mut integrations)
            .expect("second write");

        let read = PersonaCapabilities::read_from_integrations(&integrations)
            .expect("decode")
            .expect("present");
        assert_eq!(read.model, "fable-6", "latest write must win");
        assert_eq!(integrations.len(), 1, "one key, replaced in place");
    }

    #[test]
    fn absent_key_reads_as_none_not_error() {
        let identity = Identity::new("no-persona-here");
        let read = PersonaCapabilities::read_from_identity(&identity).expect("absent key is Ok");
        assert!(read.is_none(), "absent key must be the honest None");
    }

    #[test]
    fn corrupt_value_surfaces_decode_error_loudly() {
        let mut integrations = BTreeMap::new();
        integrations.insert(
            PERSONA_CAPABILITIES_KEY.to_string(),
            "{not valid json".to_string(),
        );
        let err = PersonaCapabilities::read_from_integrations(&integrations)
            .expect_err("corrupt value must NOT read as None");
        match &err {
            PersonaCapabilitiesError::Decode { raw, .. } => {
                assert_eq!(raw, "{not valid json", "error must carry the raw value");
            }
            PersonaCapabilitiesError::Encode(_) => {
                panic!("corrupt stored value is a Decode error, not Encode")
            }
        }
        // The Display rendering names the key and the raw value so the
        // failure is actionable from a log line alone.
        let rendered = err.to_string();
        assert!(rendered.contains(PERSONA_CAPABILITIES_KEY));
        assert!(rendered.contains("{not valid json"));
    }

    #[test]
    fn wrong_shape_json_is_also_a_loud_decode_error() {
        // Valid JSON, wrong shape (missing required fields) — the
        // typed accessor must refuse, not fill in defaults silently.
        let mut integrations = BTreeMap::new();
        integrations.insert(
            PERSONA_CAPABILITIES_KEY.to_string(),
            r#"{"persona_id":"skylar"}"#.to_string(),
        );
        let result = PersonaCapabilities::read_from_integrations(&integrations);
        assert!(
            matches!(result, Err(PersonaCapabilitiesError::Decode { .. })),
            "missing required fields must surface as Decode, got {result:?}",
        );
    }

    #[test]
    fn capability_tags_default_empty_for_forward_compat() {
        // A v1 writer that omits `capability_tags` still decodes — the
        // field is the one shape-evolution slot with a safe default.
        let mut integrations = BTreeMap::new();
        integrations.insert(
            PERSONA_CAPABILITIES_KEY.to_string(),
            r#"{"persona_id":"skylar","model":"fable-5","context_window_tokens":200000}"#
                .to_string(),
        );
        let read = PersonaCapabilities::read_from_integrations(&integrations)
            .expect("decode")
            .expect("present");
        assert!(read.capability_tags.is_empty());
        assert_eq!(read.persona_id, "skylar");
        assert_eq!(read.context_window_tokens, 200_000);
    }
}
