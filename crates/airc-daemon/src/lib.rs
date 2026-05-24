//! `airc-daemon` — long-running runtime process for AIRC.
//!
//! Architecture: one cross-platform IPC listener (Unix socket on
//! Unix, named pipe on Windows) accepts connections from short-lived
//! CLI invocations; each connection is one request/response round-
//! trip dispatched against the typed [`Request`] enum. The daemon
//! owns the peer keypair, registry, and any open transports;
//! subsequent CLI commands (`airc msg`, `airc status`) become
//! cheap RPCs that don't re-load identity or re-handshake transports.
//!
//! Module layout (one concern per file):
//!   - `server` — IPC listener + accept loop, [`run`].
//!   - `state` — shared daemon state ([`DaemonState`]).
//!   - `handlers` — match arms for each `Request` variant.
//!   - `airc-ipc` — wire-protocol types shared by daemon and clients.
//!
//! Adding a new operation:
//!
//!   1. Add a `Request` variant in `airc-ipc`.
//!   2. Add a `Response` variant if it returns data.
//!   3. Add a match arm in `handlers::dispatch`.
//!
//! The compiler enforces exhaustiveness — no silent gaps.
//!
//! Consumers host the daemon through `run` / `DaemonState`. IPC client
//! code lives in `airc-ipc` so consumer SDKs do not depend on daemon
//! runtime internals.

#![deny(unsafe_code)]

pub mod handlers;
pub mod identity;
pub mod peers_store;
pub mod server;
pub mod state;

pub use identity::{IdentityError, LocalIdentity};

pub use server::{run, DaemonError};
pub use state::DaemonState;
