//! Relay client transport — outbound TLS connection to an `airc-relay`
//! server, frames travel from this peer through the relay to other
//! relay-connected peers.
//!
//! Trust model: pinned Ed25519. The relay's `PeerId` and pubkey are
//! supplied by the embedder (typically out-of-band, e.g. via a signed
//! relay-config blob) and resolved through the same
//! `PeerKeyRegistry` used by the LAN-TCP adapter. The relay is a
//! single trusted peer for connection purposes; the relay-server
//! enforces a separate allowlist for which clients may connect.
//!
//! Module layout:
//!   - `config` — typed embedder-facing setup
//!   - `error` — `RelayClientError`
//!   - `adapter` — `RelayAdapter` (the `Transport` impl)

pub mod adapter;
pub mod config;
pub mod error;

pub use adapter::RelayAdapter;
pub use config::RelayClientConfig;
pub use error::RelayClientError;

#[cfg(test)]
mod tests;
