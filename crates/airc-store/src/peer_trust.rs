//! Peer trust store types.

use airc_core::PeerId;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;

use crate::StoreError;

/// Card 34942ec1 Sub-A — gradient on the peer trust registry.
///
/// Substrate doesn't currently distinguish between a peer that's a
/// process on the same UDS as me ([`TrustTier::OwnMachine`]), a peer
/// that's some other instance of MY user ([`TrustTier::OwnAccount`]),
/// a peer I've explicitly enrolled as a known person
/// ([`TrustTier::Friend`]), and a peer that just showed up
/// ([`TrustTier::Untrusted`]). All four are "trusted" or "not"
/// today; that's too coarse for routing decisions consumers want to
/// make (Continuum wants to know "can I ship a tensor to this
/// peer," Hermes wants to know "can this peer see my goals").
///
/// This Sub-A is the substrate dimension only — no detection logic,
/// no policy gates. Sub-B (detection) + Sub-C (consumer-side
/// policies) build on top once the column exists.
///
/// Wire strings are stable: changing one is a schema migration, not
/// a code refactor. The [`Self::ALL_VARIANTS`] round-trip test
/// catches a forgotten arm. Snake_case matches the convention used
/// for [`TranscriptKind`] across the codebase.
// Serialize/Deserialize is additive — the canonical wire form for
// peer-trust persistence remains `as_wire_str` (used explicitly there);
// these derives only enable serde for NEW embedders (e.g. signed grid-auth
// grants), which carry the tier as a typed field.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum TrustTier {
    /// Peer is co-located on the same physical machine as us —
    /// reachable via UDS sibling, shares filesystem, identity-key
    /// rotation requires no key material to leave the machine.
    /// Highest tier; this is "you and me are the same process tree."
    OwnMachine,
    /// Peer authenticates as the same GitHub / mesh account as us
    /// (e.g. joelteply on a different physical machine). Lower than
    /// OwnMachine — separate filesystem, separate runtime — but
    /// substantially higher than Friend because the identity claim
    /// is the same.
    OwnAccount,
    /// Peer is a known person we've explicitly enrolled (Toby on
    /// his 3090, friend's airc). The trust comes from an
    /// out-of-band human relationship; the substrate doesn't
    /// validate which person; it just records that the local
    /// operator vouched.
    Friend,
    /// Peer showed up but has no explicit enrolment beyond
    /// trust-on-first-use of the pubkey. Default for newly-seen
    /// peers; consumers should refuse expensive / privacy-sensitive
    /// operations against Untrusted peers.
    Untrusted,
}

impl TrustTier {
    /// Stable wire string. Schema-bound — changing one of these is a
    /// migration, not a code change.
    ///
    /// Adding a variant: extend the match below AND
    /// [`Self::from_wire_str`] AND [`Self::ALL_VARIANTS`]. The
    /// compiler enforces the first via match exhaustiveness; the
    /// round-trip unit test enforces the other two.
    pub fn as_wire_str(self) -> &'static str {
        match self {
            TrustTier::OwnMachine => "own_machine",
            TrustTier::OwnAccount => "own_account",
            TrustTier::Friend => "friend",
            TrustTier::Untrusted => "untrusted",
        }
    }

    /// Inverse of [`Self::as_wire_str`]. Returns `None` for unknown
    /// strings; callers wrap that into a typed store error so a
    /// future tier added to an older binary surfaces honestly rather
    /// than silently downgrading.
    pub fn from_wire_str(s: &str) -> Option<Self> {
        Some(match s {
            "own_machine" => TrustTier::OwnMachine,
            "own_account" => TrustTier::OwnAccount,
            "friend" => TrustTier::Friend,
            "untrusted" => TrustTier::Untrusted,
            _ => return None,
        })
    }

    /// Every variant, in trust-gradient order (highest first). The
    /// round-trip test iterates this slice; adding a variant without
    /// extending this constant fails the test.
    pub const ALL_VARIANTS: &'static [TrustTier] = &[
        TrustTier::OwnMachine,
        TrustTier::OwnAccount,
        TrustTier::Friend,
        TrustTier::Untrusted,
    ];

    /// Default for newly-observed peers: [`TrustTier::Untrusted`].
    /// Centralised so a future "Sub-B detection promoted me to
    /// OwnAccount" path always has the same starting point.
    pub const fn default_for_new_peer() -> Self {
        TrustTier::Untrusted
    }
}

impl std::fmt::Display for TrustTier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_wire_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredPeer {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
    pub added_at_ms: u64,
    /// Card 34942ec1 Sub-A: trust gradient. Pre-migration rows
    /// default to [`TrustTier::Untrusted`]; explicit enrolment
    /// (Sub-B detection, `airc peer add --tier=…`) sets a higher
    /// tier.
    pub tier: TrustTier,
    /// Card 625abe6d slice 1: serde JSON of the peer's advertised
    /// `Vec<RouteEndpoint>`. Opaque at this layer (the enum lives in
    /// airc-lib, above this crate); `None` = identity-only enrolment
    /// — the route resolver gets no dial candidates from this record.
    pub endpoints_json: Option<String>,
    /// Seam #3.2 (liveness): epoch-ms of the last contact we had with
    /// this peer. Concrete, never `None`: a row whose stored column is
    /// NULL (never touched since enrolment) reads back floored to
    /// `added_at_ms`, so this is always a defensible "no later than"
    /// recency floor the age-based eviction classifier can read.
    pub last_seen_ms: u64,
}

impl StoredPeer {
    pub fn pubkey_bytes(&self) -> Result<[u8; 32], StoreError> {
        let bytes = URL_SAFE_NO_PAD.decode(&self.pubkey_b64)?;
        if bytes.len() != 32 {
            return Err(StoreError::WrongPubkeyLength(bytes.len()));
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&bytes);
        Ok(out)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RotationAuditEntry {
    pub peer_id: PeerId,
    pub prev_pubkey_b64: String,
    pub next_pubkey_b64: String,
    pub sequence: u64,
    pub rotated_at_ms: u64,
    pub applied_at_ms: u64,
}
