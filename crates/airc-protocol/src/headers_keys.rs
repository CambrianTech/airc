//! Substrate-owned header keys.
//!
//! Adapters and consumers reference these by constant, not by stringly-
//! typed lookup. The substrate owns the `airc.*` namespace; consumers own
//! `forge.*`, `continuum.*`, `openclaw.*`, `hermes.*`, `opencode.*`, and
//! `x-*` (see substrate design doc, "Header namespaces" section).
//!
//! These keys are projections onto the headers map for adapters that can
//! only inspect headers (legacy or generic gateways). The authoritative
//! field is on `Envelope` itself (e.g. `Envelope.reply_to`), and a
//! mismatch between the structured field and the header projection causes
//! validation to fail (see `signature::verify`).

/// Tracing correlation id — flows end-to-end so cross-process traces can
/// join. Adapters echo this value unchanged.
pub const HEADER_AIRC_TRACE_ID: &str = "airc.trace_id";

/// Reply-to projection of `Envelope.reply_to`. Header-only adapters that
/// cannot read the structured field rely on this. If both are present
/// they MUST agree, or `verify()` rejects the frame.
pub const HEADER_AIRC_REPLY_TO: &str = "airc.reply_to";

/// Substrate priority hint — adapters may use this to influence transport
/// queue ordering. Values are consumer-defined strings; substrate does
/// not interpret them.
pub const HEADER_AIRC_PRIORITY: &str = "airc.priority";

/// Substrate deadline hint — adapters may use this to drop a frame whose
/// deadline has passed before delivering. Format: epoch milliseconds as
/// a decimal string.
pub const HEADER_AIRC_DEADLINE: &str = "airc.deadline";

/// Runtime consumer identity. This is the human/agent process label
/// (`codex:<thread>`, `claude:<session>`, etc.) used by hooks and
/// monitors to filter their own sends without conflating every process
/// that shares the same persisted substrate `ClientId`.
pub const HEADER_AIRC_CLIENT: &str = "airc.client";

/// Body-shape hint from the forge/alloy contract layer. The string names
/// a forge contract (e.g. `"forge.persona.turn"`); consumers that
/// recognise the contract decode the body accordingly. Substrate never
/// interprets this — it only routes on it.
pub const HEADER_FORGE_BODY_HINT: &str = "forge.body_hint";

/// Request/reply correlation id. Distinct from `airc.trace_id`:
/// trace_id is end-to-end observability that spans many events;
/// correlation_id pairs ONE request with ONE reply. The command-bus
/// helpers (`Airc::request` / `Airc::reply` / `Airc::await_reply`)
/// generate + match on this header. Format: UUIDv4 string.
pub const HEADER_AIRC_CORRELATION_ID: &str = "airc.correlation_id";

/// Command-kind label for the command-bus primitive. Consumers
/// own the vocabulary (e.g. `continuum.lora.invoke`,
/// `forge.hermes.agent_command`); the substrate carries it as
/// opaque routing metadata. Useful for receivers to dispatch
/// matching handlers without parsing the body.
pub const HEADER_AIRC_COMMAND_KIND: &str = "airc.command_kind";

// ---------------------------------------------------------------------------
// Body-encryption convention — card 1224aac2 slice 2
// ---------------------------------------------------------------------------
//
// The substrate's wire crypto (frame signing, TLS-pinned transport) authenticates
// + opaque-wraps the on-the-wire frame. Consumers that need their bodies to be
// opaque ALSO to the substrate (defense-in-depth, content-key escrow, "even my
// own daemon can't read this") wrap the body with their crypto of choice (JWE,
// age, noise-box, whatever) BEFORE handing it to `publish`. The three headers
// below are the canonical labeling convention so:
//
//   - UI / renderer surfaces show `[encrypted: <scheme>]` instead of rendering
//     ciphertext as garbled text
//   - key-rotation jobs can find every event using a given key by `key_id`
//   - audit logs surface the scheme without decrypting
//
// Substrate ships the convention; consumers ship the crypto. Substrate does NOT
// validate any of these — set them or don't, the daemon routes opaque either way.

/// Body-encryption scheme identifier. Opaque consumer-defined string —
/// `"jwe.A256GCM"`, `"age.v1"`, `"continuum.aead.v1"`, etc. Absence of the
/// header is the substrate-visible signal that the body is plaintext.
///
/// Consumers MUST choose a scheme id that round-trips through their decode
/// path — the substrate compares it only for equality across events (key
/// rotation, audit grouping); never parses it.
pub const HEADER_AIRC_BODY_ENC_SCHEME: &str = "airc.body.enc.scheme";

/// Identifier of the key that encrypted this body. Free-form consumer string —
/// could be a UUID, a key thumbprint, a label like `"continuum.user.<uid>.v3"`.
/// Used by key-rotation jobs to enumerate events still encrypted under a
/// rotated key; the substrate does no key management itself.
pub const HEADER_AIRC_BODY_ENC_KEY_ID: &str = "airc.body.enc.key_id";

/// Additional authenticated data (AAD) the encryptor bound the ciphertext
/// to. Present when the consumer's scheme uses an AEAD that binds context
/// (room id, sender peer, intended audience, etc.) into the ciphertext. The
/// substrate stores + delivers it opaque; the decryptor must supply the same
/// AAD on decrypt or authentication will fail closed.
pub const HEADER_AIRC_BODY_ENC_AAD: &str = "airc.body.enc.aad";

// ---------------------------------------------------------------------------
// Canonical attribution + role + target convention — card d26cb46c
// ---------------------------------------------------------------------------
//
// The substrate already threads `from_peer` + `from_client` through IPC (per
// the owner-core attribution work; memory: owner-core-agent-attribution-gap).
// What outbound surfaces (GitHub plugin, web UI, audit log, work-board
// projection, persona renderer) need is a UNIFORM HEADER VOCABULARY so the
// originator's identity, role, target, and trust tier survive the outbound
// translation. Without this, every PR/comment/review on GitHub renders as
// "joelteply" (the account owning the API tokens) instead of as the actual
// persona/agent — Joel 2026-05-30 "the 'joelteply leak'".
//
// Substrate ships the vocabulary; consumers populate + render it. Where a
// structured equivalent exists on `Envelope` (e.g. `from_peer`), the header
// is a projection of the authoritative field — mismatch fails frame verify,
// same pattern as `HEADER_AIRC_REPLY_TO`.

// --- Identity attribution ----------------------------------------------------

/// Substrate peer_id (UUID string) of the originating peer. Always set on
/// outbound events; survives every outbound translation. Renderers use this
/// as the primary identity key for avatar / display-name lookup.
///
/// Projection of `SendRequest::from_peer` / `Envelope::from_peer`. Mismatch
/// between structured + header values causes verify to reject.
pub const HEADER_AIRC_FROM_PEER: &str = "airc.from.peer";

/// Per-session client_id (UUID string) — distinguishes tabs / sessions /
/// agent processes that share one `airc.from.peer`. Mirrors the existing
/// `airc.client` header (which stays for hook compatibility); this constant
/// lives in the canonical `airc.from.*` namespace so consumers don't have
/// to special-case the older spelling when projecting identity.
pub const HEADER_AIRC_FROM_CLIENT: &str = "airc.from.client";

/// Persona id (UUID string) when the originator is a continuum persona
/// acting as a first-class airc peer. Absence = not-a-persona (a human
/// user, an agent, a system actor). Substrate doesn't validate the id —
/// continuum owns the persona registry — but rendering / audit logic keys
/// off this header to dispatch to the persona-card layer instead of the
/// default user-card layer.
pub const HEADER_AIRC_FROM_PERSONA: &str = "airc.from.persona";

/// Cached display name for renderer convenience. The authoritative source
/// is the identity card on the wall (`TranscriptKind::IdentityPublished`);
/// this header is the projection consumers use without an extra lookup so
/// every event self-contains enough to render. Drift between cached value
/// and wall card is acceptable (renderer can re-read the wall on demand);
/// the substrate does NOT enforce equality.
pub const HEADER_AIRC_FROM_DISPLAY_NAME: &str = "airc.from.display_name";

/// Optional runtime hint: `"claude"`, `"continuum-claude"`, `"codex"`,
/// `"web"`, `"gh-bot"`, etc. Renderers use it to pick the right avatar /
/// icon family without inferring from `airc.from.client` substrings.
/// Substrate doesn't validate; consumers extend the vocabulary as they
/// add runtimes.
pub const HEADER_AIRC_FROM_RUNTIME: &str = "airc.from.runtime";

// --- Role + target -----------------------------------------------------------

/// The originator's role for THIS event: `"author"` / `"reviewer"` /
/// `"watcher"` / `"mentioned"` / `"merger"` / `"reactor"` / consumer-
/// extended. Substrate doesn't enumerate the vocabulary; renderers
/// dispatch to the right UI element (review badge, mention chip, etc.)
/// by string match and fall back to verbatim rendering for unknowns.
pub const HEADER_AIRC_ROLE: &str = "airc.role";

/// Target type the event is acting on: `"pr"` / `"card"` / `"room"` /
/// `"peer"` / `"recipe"` / `"blob"` / `"setting"`. Consumer-extensible.
/// Renderers use this to route the event to the right surface (a PR
/// review goes to the PR thread, a card update goes to the work board).
pub const HEADER_AIRC_TARGET_KIND: &str = "airc.target.kind";

/// Target identifier — semantics depend on `airc.target.kind`. For
/// `kind=pr` it's the PR number; for `kind=card` it's the card UUID;
/// for `kind=room` it's the room id; for `kind=blob` it's the blob
/// hash; for `kind=setting` it's the setting key. Substrate carries it
/// opaque.
pub const HEADER_AIRC_TARGET_ID: &str = "airc.target.id";

/// Optional GitHub-style repo identifier when the target lives in a
/// versioned repo context — e.g. `"CambrianTech/airc"` for PRs, cards
/// scoped to a repo. Absent for targets that don't have a repo (a
/// per-user setting, a free-floating room).
pub const HEADER_AIRC_TARGET_REPO: &str = "airc.target.repo";

// --- Trust + provenance ------------------------------------------------------

/// Resolved trust tier of the originator at event-emit time. Stable
/// wire-string form matching
/// `airc_store::peer_trust::TrustTier::as_wire_str` —
/// `"own_machine"` / `"own_account"` / `"friend"` / `"untrusted"`.
///
/// Snapshot at emit-time so the audit log preserves the as-believed
/// tier even if the originator's tier changes later. Renderers use it
/// to choose a confidence indicator (color / badge); review aggregates
/// use it for tier-min policy enforcement (card ae3e1a47).
pub const HEADER_AIRC_TRUST_TIER: &str = "airc.trust.tier";

#[cfg(test)]
mod tests {
    use super::*;

    /// Card 1224aac2 slice 2: the substrate's body-encryption convention
    /// must STAY in the `airc.body.enc.*` namespace — adapters + audit
    /// surfaces look up by exact string, so a rename here would silently
    /// break renderer / rotation tooling without a compile error.
    #[test]
    fn body_encryption_header_names_are_stable() {
        assert_eq!(HEADER_AIRC_BODY_ENC_SCHEME, "airc.body.enc.scheme");
        assert_eq!(HEADER_AIRC_BODY_ENC_KEY_ID, "airc.body.enc.key_id");
        assert_eq!(HEADER_AIRC_BODY_ENC_AAD, "airc.body.enc.aad");
    }

    /// Card 1224aac2 slice 2: every encryption header lives under the
    /// substrate's `airc.*` namespace and never collides with the
    /// consumer-owned `forge.*` / `continuum.*` / `openclaw.*` /
    /// `hermes.*` / `opencode.*` / `x-*` spaces.
    #[test]
    fn body_encryption_headers_in_substrate_namespace() {
        for header in [
            HEADER_AIRC_BODY_ENC_SCHEME,
            HEADER_AIRC_BODY_ENC_KEY_ID,
            HEADER_AIRC_BODY_ENC_AAD,
        ] {
            assert!(
                header.starts_with("airc."),
                "{header:?} must be in the substrate-owned `airc.*` namespace"
            );
        }
    }
}

#[cfg(test)]
mod attribution_header_tests {
    use super::*;

    /// Card d26cb46c: the attribution + role + target + trust header
    /// names MUST stay stable. Renderers (GitHub plugin, web UI, audit
    /// log) match by exact string; a silent rename here flips every
    /// existing event's identity into "unknown" at every consumer
    /// surface without a compile error.
    #[test]
    fn attribution_header_names_are_stable() {
        // Identity attribution
        assert_eq!(HEADER_AIRC_FROM_PEER, "airc.from.peer");
        assert_eq!(HEADER_AIRC_FROM_CLIENT, "airc.from.client");
        assert_eq!(HEADER_AIRC_FROM_PERSONA, "airc.from.persona");
        assert_eq!(HEADER_AIRC_FROM_DISPLAY_NAME, "airc.from.display_name");
        assert_eq!(HEADER_AIRC_FROM_RUNTIME, "airc.from.runtime");
        // Role + target
        assert_eq!(HEADER_AIRC_ROLE, "airc.role");
        assert_eq!(HEADER_AIRC_TARGET_KIND, "airc.target.kind");
        assert_eq!(HEADER_AIRC_TARGET_ID, "airc.target.id");
        assert_eq!(HEADER_AIRC_TARGET_REPO, "airc.target.repo");
        // Trust + provenance
        assert_eq!(HEADER_AIRC_TRUST_TIER, "airc.trust.tier");
    }

    /// Card d26cb46c: every attribution header lives under the
    /// substrate's `airc.*` namespace and never collides with
    /// consumer-owned `forge.*` / `continuum.*` / `openclaw.*` /
    /// `hermes.*` / `opencode.*` / `x-*` spaces.
    #[test]
    fn attribution_headers_in_substrate_namespace() {
        for header in [
            HEADER_AIRC_FROM_PEER,
            HEADER_AIRC_FROM_CLIENT,
            HEADER_AIRC_FROM_PERSONA,
            HEADER_AIRC_FROM_DISPLAY_NAME,
            HEADER_AIRC_FROM_RUNTIME,
            HEADER_AIRC_ROLE,
            HEADER_AIRC_TARGET_KIND,
            HEADER_AIRC_TARGET_ID,
            HEADER_AIRC_TARGET_REPO,
            HEADER_AIRC_TRUST_TIER,
        ] {
            assert!(
                header.starts_with("airc."),
                "{header:?} must be in the substrate-owned `airc.*` namespace"
            );
        }
    }

    /// Card d26cb46c: the identity-attribution headers MUST stay in
    /// the `airc.from.*` sub-namespace so consumers can iterate
    /// projection by prefix (e.g. "give me all from-data on this
    /// event"). Drift into a different sub-namespace would break
    /// prefix iteration silently.
    #[test]
    fn identity_attribution_headers_share_from_sub_namespace() {
        for header in [
            HEADER_AIRC_FROM_PEER,
            HEADER_AIRC_FROM_CLIENT,
            HEADER_AIRC_FROM_PERSONA,
            HEADER_AIRC_FROM_DISPLAY_NAME,
            HEADER_AIRC_FROM_RUNTIME,
        ] {
            assert!(
                header.starts_with("airc.from."),
                "{header:?} must share the `airc.from.*` identity sub-namespace"
            );
        }
    }

    /// Card d26cb46c: target headers MUST stay in `airc.target.*` so
    /// consumers can group by target dimension symmetrically with
    /// identity grouping.
    #[test]
    fn target_headers_share_target_sub_namespace() {
        for header in [
            HEADER_AIRC_TARGET_KIND,
            HEADER_AIRC_TARGET_ID,
            HEADER_AIRC_TARGET_REPO,
        ] {
            assert!(
                header.starts_with("airc.target."),
                "{header:?} must share the `airc.target.*` sub-namespace"
            );
        }
    }
}
