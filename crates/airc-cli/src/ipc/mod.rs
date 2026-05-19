//! IPC between the `airc-rs` CLI and the running daemon.
//!
//! Wire protocol = newline-delimited JSON over a Unix socket. One
//! request per connection, one response, connection closes. Simple,
//! debuggable, extensible — adding a new op is one variant in
//! `Request` + one match arm in the daemon's dispatcher.
//!
//! Why not gRPC / cap'n proto / mDNS RPC: this is local-only IPC for
//! a single-machine daemon. JSON over Unix sockets is the right
//! amount of ceremony — typed in Rust (Request/Response enums),
//! human-debuggable via `socat - UNIX-CONNECT:...`, and zero extra
//! schema toolchain.

pub mod client;
pub mod request;
pub mod response;

pub use client::DaemonClient;
pub use request::SendRequest;

// `ClientError`, `Request`, `Response`, `StatusResponse` are
// accessible via their module paths for callers that want them; we
// don't re-export them at this level to keep the prelude tight.
