//! Consumer integration shape examples.
//!
//! Three downstream consumers — Continuum, OpenClaw, Hermes —
//! each get a module that demonstrates the AIRC header/body contract
//! shape that real integrations should mirror. The fixture tests
//! prove encode → decode round-trips and that the supplied
//! [`airc_lib::EventFilter`] correctly admits events of the right
//! shape and rejects unrelated ones.
//!
//! What this crate is:
//! - typed event vocabularies (just enough to demonstrate the
//!   pattern; integrations extend with their own events)
//! - codec functions producing `(Headers, Body)` ready to feed into
//!   [`airc_lib::Airc::send`]
//! - subscription filters ready to feed into
//!   [`airc_lib::Airc::subscribe_filtered`]
//!
//! What this crate is NOT:
//! - a real Continuum / OpenClaw / Hermes integration
//! - a substrate change — codecs ride on existing
//!   `Headers` + `Body` + `EventFilter` surfaces with no airc-lib
//!   edits required
//! - exhaustive — each module ships a representative subset; real
//!   integrations add more event variants following the same shape
//!
//! The audit gaps this closes (first slice):
//! - §13 *"Consumer Integration Is Not Proven"* — shape proof.
//! - Consumer Integration Gaps:
//!   - *"Continuum: persona/chat/activity events over AIRC envelopes
//!     with forge-alloy contracts"*
//!   - *"OpenClaw: adapter mapping existing chat/thread identity into
//!     AIRC PeerId/ClientId/channel"*
//!   - *"Hermes: agent command/events over headers and typed payload
//!     contracts"*
//!
//! opencode/Codex/Claude (the agent-INBOUND subscription shape) is
//! deliberately omitted from this slice — Codex's PR-I2 dogfood
//! lane covers that surface.

#![deny(unsafe_code)]

pub mod continuum;
pub mod hermes;
pub mod openclaw;
