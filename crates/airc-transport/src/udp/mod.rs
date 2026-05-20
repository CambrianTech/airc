//! `UdpAdapter` — low-latency datagram transport for interrupt-style
//! AIRC frames.
//!
//! UDP is intentionally narrower than `local_fs`, `lan_tcp`, or
//! `relay`: it is for event/control signaling where stale data should
//! be dropped rather than queued. It does NOT claim durable transcript
//! delivery. `Transport::send` therefore rejects `FrameKind::Message`
//! and `FrameKind::Control` explicitly. Durable traffic must use a
//! durable route.
//!
//! Security boundary: UDP does not authenticate peers at the socket
//! layer. Production callers wrap this adapter with `SignedTransport`
//! so frame signatures are verified at the substrate boundary. The
//! adapter does not degrade to unsigned trust.

mod adapter;
mod config;
mod error;

pub use adapter::UdpAdapter;
pub use config::UdpConfig;
pub use error::UdpError;

#[cfg(test)]
mod tests;
