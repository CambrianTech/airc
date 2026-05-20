//! Consumer-embedding smoke crate.
//!
//! This crate has no runtime code. Its purpose is to PROVE — by being
//! a downstream consumer of `airc-lib` — that a small consumer app
//! can embed AIRC without reaching into substrate internals or
//! shelling out to the `airc-rs` CLI.
//!
//! That's the audit's [Gate 4](Consumer Embedding) and grievance §13
//! ("Consumer Integration Is Not Proven"):
//!
//! > Pass when a small consumer app can link `airc-lib` and:
//! > create/load identity; join a channel; send typed body with
//! > headers; subscribe by header/channel/kind; fetch replay; use
//! > blobs; never shell out.
//!
//! The integration test in `tests/two_agents_via_sdk.rs` exercises
//! the embedding pattern end-to-end. Future PR-I slices will add
//! consumer-shape examples (Continuum personas, OpenClaw chat/thread
//! identity bridge, Hermes agent contracts, etc.) — this crate is the
//! foundation those build on.
//!
//! Constraint: this crate's only AIRC dependency is `airc-lib`. If a
//! future change forces a substrate-internal AIRC import in here,
//! that's a signal `airc-lib` is missing a re-export, not a sign this
//! crate should grow more substrate imports.

pub mod agent;
