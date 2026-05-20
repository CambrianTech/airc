//! IPC between the `airc-core` CLI and the running daemon.
//!
//! Wire protocol = newline-delimited JSON over a local IPC primitive.
//! On Unix that's a Unix-domain socket at `<home>/daemon.sock`. On
//! Windows it's a named pipe (`\\.\pipe\airc-core-<home>`). The
//! `transport` module abstracts both behind one `IpcListener` /
//! `IpcStream` API; everything above (request/response types,
//! dispatch, handlers) stays platform-agnostic.
//!
//! Why not gRPC / cap'n proto: this is local-only IPC for a
//! single-machine daemon. JSON over Unix sockets / named pipes is
//! the right amount of ceremony — typed in Rust (Request/Response
//! enums), human-debuggable, zero extra schema toolchain.

pub mod client;
pub mod request;
pub mod response;
pub mod transport;

pub use client::DaemonClient;
pub use request::SendRequest;
// transport::IpcListener / IpcStream are used by daemon::server and
// ipc::client directly via their module paths; no top-level re-export.

// `ClientError`, `Request`, `Response`, `StatusResponse` are
// accessible via their module paths for callers that want them; we
// don't re-export them at this level to keep the prelude tight.
