//! Consumer-embedding smoke crate.
//!
//! This crate has no runtime code. Its purpose is to PROVE — by being
//! a downstream consumer of `airc-lib` — that a small consumer app
//! can embed AIRC without reaching into substrate internals or
//! shelling out to the `airc-core` CLI.
//!
//! That's the audit's [Gate 4](Consumer Embedding) and grievance §13
//! ("Consumer Integration Is Not Proven"):
//!
//! > Pass when a small consumer app can link `airc-lib` and:
//! > create/load identity; join a channel; send typed body with
//! > headers; subscribe by header/channel/kind; fetch replay; use
//! > blobs; never shell out.
//!
//! The integration tests exercise the embedding pattern end-to-end.
//! Future PR-I slices will add consumer-shape examples (Continuum
//! personas, OpenClaw chat/thread identity bridge, Hermes agent
//! contracts, etc.) — this crate is the foundation those build on.
//!
//! Constraint: the RUNTIME embedding surface (`src/agent.rs`) depends
//! only on `airc-lib` and reaches the substrate solely via
//! `Airc::attach`. In the owner-core model same-machine delivery is the
//! one machine daemon's job, so the integration tests stand up an
//! in-process daemon (the airc install ships it in production) — the
//! substrate deps for that live in `[dev-dependencies]` + the test
//! harness only, never in the embedding surface. If a future change
//! forces a substrate-internal import into `src/`, that's a signal
//! `airc-lib` is missing a re-export.

pub mod agent;
