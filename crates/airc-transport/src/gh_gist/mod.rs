//! GitHub gist bootstrap/rendezvous transport.
//!
//! This adapter exists to contain the old gist-backed message wire
//! behind the same `Transport` trait while AIRC moves to real peer
//! transports. It is not intended as the steady-state chat path:
//! GitHub can help peers discover each other and exchange bootstrap
//! metadata, then LAN/Tailscale/relay transports should carry the
//! runtime stream. Nothing in this module is wired as automatic
//! fallback; consumers must opt into it deliberately for bootstrap or
//! rendezvous work.

mod adapter;
mod client;
mod error;

pub use adapter::GhGistAdapter;
pub use client::{GhCliClient, GistClient};
pub use error::GhGistError;
