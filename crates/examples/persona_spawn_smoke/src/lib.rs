//! Persona-spawn worked example (card 98bf179b — persona-peer 2/8).
//!
//! Proves the Continuum persona-spawn loop end to end on public AIRC
//! surfaces only:
//!
//! 1. a parent process spawns a persona with `AIRC_HOME` +
//!    `AIRC_PARENT_PEER_SPEC` in the environment;
//! 2. the persona `Airc::open`s its own home (its own PeerId +
//!    identity.key — personas are real peers, not sub-identities);
//! 3. it advertises [`airc_core::PersonaCapabilities`] on its identity
//!    card via the card-9e5f8844 typed accessor;
//! 4. it enrols the parent and pins it at `TrustTier::OwnMachine`
//!    through `airc_trust::set_tier` (the spawn relationship IS the
//!    same-machine relationship);
//! 5. it joins the persona room and answers `TurnRequested` events
//!    with `TurnEmitted` — the Continuum turn loop.
//!
//! Pattern sibling of `embedded_consumer_smoke::agent`: the runtime
//! surface lives in [`persona_agent`], the binary (`src/main.rs`)
//! is just env-wiring around it, and the integration test proves the
//! loop with two real `Airc` handles in tempdirs over loopback
//! LAN-TCP (the `stored_endpoint_dial.rs` style).

pub mod persona_agent;
