//! IPC contract between clients and the running AIRC daemon.
//!
//! Wire protocol = length-prefixed CBOR frames over a local IPC
//! primitive. The CLI derives the default endpoint from the scoped
//! home plus [`IPC_PROTOCOL_VERSION`], so protocol-incompatible
//! daemons never share a socket. The `transport` module abstracts
//! Unix sockets and Windows named pipes behind one `IpcListener` /
//! `IpcStream` API; everything above (request/response types,
//! dispatch, handlers) stays platform-agnostic.
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

/// Local daemon IPC ABI version.
///
/// Bump this when the request/response wire encoding changes in a way
/// that an already-running daemon cannot parse. The default CLI socket
/// includes this value so `airc join` starts a current daemon instead of
/// connecting to a stale daemon that speaks the previous protocol.
// v5: owner-core contract — no `wire`/`Subscribe`/`ResolveWire`; live +
// inbox events cross as airc-wire bytes; cursor is `(epoch, counter)`.
// Bumped from 4 so a v4 daemon and a v5 client never share a socket.
pub const IPC_PROTOCOL_VERSION: u16 = 5;

pub use client::{ClientError, DaemonClient};
pub use request::{
    AddPeerRequest, AttachRequest, InboxRequest, IpcCursor, IpcDelivery, IpcKind, IpcTarget,
    PublishRequest, RemovePeerRequest, Request, SendRequest,
};
pub use response::{
    InboxResponse, PeersResponse, PublishResponse, Response, StatusResponse,
};
// IpcListener / IpcStream stay under `transport` because only the
// daemon host and low-level tests need the raw byte transport.
