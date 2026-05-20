//! `airc-relay` — the relay server crate.
//!
//! A relay is a forwarding service that two peers can connect OUT to
//! when they cannot connect to each other directly (different LANs,
//! no Tailscale, NAT). Both peers maintain outbound TLS connections
//! to the relay; the relay routes frames between them.
//!
//! Trust model: mTLS with Ed25519-pinned identities, same primitives
//! as `airc-transport::lan_tcp`. The relay has its own Ed25519
//! identity; clients pin its pubkey. Clients present their own certs;
//! the relay binds each connection to a `PeerId` derived from the
//! client's cert.
//!
//! The relay does NOT decrypt frame bodies. It inspects the envelope's
//! routing fields (`target`, `channel`) to decide where to forward.
//! Frame signatures are end-to-end between peers and unchanged by
//! the relay.
//!
//! What this crate ships:
//!   - [`RelayServer`] — the runtime that accepts connections + routes.
//!   - [`RelayServerConfig`] — the typed setup the embedder provides.
//!   - [`RelayServerError`] — fail-closed errors with no silent fall-back.
//!
//! Out of scope (later sub-PRs):
//!   - Clustering / HA across multiple relay instances.
//!   - Durable offline mailbox beyond a bounded in-memory queue.
//!   - Federation between relays.
//!   - E2E content encryption above signing (signing is the baseline
//!     authenticity contract; encryption on top would not change the
//!     relay's routing role).

mod connection;
mod error;
mod server;

pub use error::RelayServerError;
pub use server::{RelayServer, RelayServerConfig};
