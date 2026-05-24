//! IPC contract between clients and the running AIRC daemon.
//!
//! Wire protocol = length-prefixed CBOR frames over a local IPC
//! primitive. On Unix that's a Unix-domain socket at
//! `<home>/daemon.sock`. On Windows it's a named pipe
//! (`\\.\pipe\airc-core-<home>`). The `transport` module abstracts
//! both behind one `IpcListener` / `IpcStream` API; everything above
//! (request/response types, dispatch, handlers) stays
//! platform-agnostic.
//!
//! This crate intentionally contains no daemon runtime state. It is the
//! local ABI: typed request/response enums, frame codec, cross-platform
//! local transport, and a client. The daemon implements the server side;
//! consumers can use the client without depending on daemon internals.

pub mod client;
pub mod codec;
pub mod request;
pub mod response;
pub mod transport;

pub use client::{ClientError, DaemonClient};
pub use request::{
    AddPeerRequest, AttachRequest, InboxRequest, RemovePeerRequest, Request, SendRequest,
    SubscribeRequest,
};
pub use response::{InboxResponse, PeersResponse, Response, StatusResponse};
// IpcListener / IpcStream stay under `transport` because only the
// daemon host and low-level tests need the raw byte transport.
