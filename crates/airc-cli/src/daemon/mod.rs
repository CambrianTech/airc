//! Long-running daemon that holds substrate state for short-lived
//! CLI calls.
//!
//! Architecture: one Unix socket listener accepts connections from
//! `airc-rs` CLI invocations; each connection is one
//! request/response round-trip dispatched against the typed
//! `Request` enum. The daemon owns the peer keypair, registry, and
//! any open transports; subsequent CLI commands (`airc-rs msg`,
//! `airc-rs status`) become cheap RPCs that don't re-load identity
//! or re-handshake transports.
//!
//! Module layout (one concern per file):
//!   - `server` — Unix socket listener + accept loop
//!   - `state` — shared daemon state (peer_id, keypair, transports)
//!   - `handlers` — match arms for each `Request` variant
//!
//! Adding a new operation:
//!
//!   1. Add a `Request` variant in `ipc::request`
//!   2. Add a `Response` variant if it returns data
//!   3. Add a match arm in `handlers::dispatch`
//!
//! The compiler enforces exhaustiveness — no silent gaps.

pub mod handlers;
pub mod server;
pub mod state;

pub use server::run;
pub use state::DaemonState;
// `DaemonError` is reachable via `crate::daemon::server::DaemonError`
// for callers that need it; not re-exported at this level since the
// CLI catches it via the Box<dyn Error> path.
