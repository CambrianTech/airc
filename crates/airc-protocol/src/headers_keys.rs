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
