//! GitHub gist bootstrap/rendezvous support.
//!
//! GitHub is a public invite beacon, not a runtime transport. It can
//! publish signed connection metadata so peers can move onto admitted
//! live routes such as local-fs, LAN/Tailscale TCP, relay, Reticulum,
//! UDP, or WebRTC. It must not carry sustained chat/event frames.

mod client;
mod error;
mod invite;

pub use client::{GhCliClient, GistClient};
pub use error::GhGistError;
pub use invite::{GhGistInviteStore, GIST_INVITE_FILE};
