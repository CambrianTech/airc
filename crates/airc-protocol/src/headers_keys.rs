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

/// Media projection of `Envelope.media`. Header-only surfaces (transcript
/// metadata, legacy adapters) that cannot read the structured `media` field
/// rely on this; the value is the JSON-serialized `Vec<MediaRef>`. The
/// authoritative field is `Envelope.media` — this is the projection onto the
/// headers/metadata map, mirroring [`HEADER_AIRC_REPLY_TO`].
pub const HEADER_AIRC_MEDIA: &str = "airc.media";

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
    /// `hermes.*` / `opencode.*` / `x-*` spaces (see substrate design
    /// doc, "Header namespaces" section).
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
