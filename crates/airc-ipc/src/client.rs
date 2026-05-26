//! Typed client used by CLI commands to talk to a running daemon.
//!
//! `DaemonClient::call(request)` opens the local daemon socket, writes
//! one length-prefixed typed request frame, reads one typed response
//! frame, and closes. One round-trip per connection keeps the daemon's
//! accept loop simple while avoiding newline-sensitive parsing.
//!
//! Convenience helpers (`ping`, `status`, `send`, `stop`) wrap the
//! generic `call` and dispatch on response variants so callers don't
//! pattern-match on `Response` themselves.

use std::path::PathBuf;

use tokio::time::{timeout, Duration};

use crate::codec::{read_frame, write_frame};
use crate::transport::IpcStream;

use crate::request::{
    AddPeerRequest, AttachRequest, InboxRequest, PublishRequest, RemovePeerRequest, Request,
    ResolveWireRequest, SendRequest, SubscribeRequest,
};
use crate::response::{
    InboxResponse, PeersResponse, PublishResponse, ResolveWireResponse, Response, StatusResponse,
};

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(5);
const SUBSCRIBE_RPC_TIMEOUT: Duration = Duration::from_secs(15);

/// Reasons a daemon RPC fails.
#[derive(Debug)]
pub enum ClientError {
    /// Couldn't connect to the socket — daemon not running.
    NotConnected(std::io::Error),
    /// Underlying socket I/O failure mid-call.
    Io(std::io::Error),
    /// Request or response failed to serialize/deserialize.
    Codec(serde_json::Error),
    /// The daemon accepted or was contacted, but did not complete
    /// the request inside the RPC deadline.
    Timeout,
    /// Daemon returned `Response::Error { message }`.
    Daemon(String),
    /// Daemon returned a response variant inconsistent with the
    /// request (e.g. `Status` returning `Pong`). Indicates a daemon
    /// bug or a wire-protocol mismatch.
    UnexpectedResponse(Response),
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClientError::NotConnected(error) => {
                write!(f, "daemon not reachable: {error}")
            }
            ClientError::Io(error) => write!(f, "daemon RPC I/O: {error}"),
            ClientError::Codec(error) => write!(f, "daemon RPC codec: {error}"),
            ClientError::Timeout => write!(f, "daemon RPC timed out"),
            ClientError::Daemon(message) => write!(f, "daemon error: {message}"),
            ClientError::UnexpectedResponse(response) => {
                write!(f, "daemon returned unexpected response: {response:?}")
            }
        }
    }
}

impl std::error::Error for ClientError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            ClientError::NotConnected(error) | ClientError::Io(error) => Some(error),
            ClientError::Codec(error) => Some(error),
            ClientError::Timeout | ClientError::Daemon(_) | ClientError::UnexpectedResponse(_) => {
                None
            }
        }
    }
}

impl From<serde_json::Error> for ClientError {
    fn from(error: serde_json::Error) -> Self {
        ClientError::Codec(error)
    }
}

pub struct DaemonClient {
    socket_path: PathBuf,
}

impl DaemonClient {
    pub fn new(socket_path: PathBuf) -> Self {
        Self { socket_path }
    }

    /// Generic RPC: writes one length-prefixed request frame and reads
    /// one length-prefixed response frame. No half-close — Unix
    /// sockets support `shutdown`, but Windows named pipes don't.
    pub async fn call(&self, request: Request) -> Result<Response, ClientError> {
        self.call_with_timeout(request, DEFAULT_RPC_TIMEOUT).await
    }

    pub async fn call_with_timeout(
        &self,
        request: Request,
        deadline: Duration,
    ) -> Result<Response, ClientError> {
        timeout(deadline, self.call_inner(request))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn call_inner(&self, request: Request) -> Result<Response, ClientError> {
        let stream = IpcStream::connect(&self.socket_path)
            .await
            .map_err(ClientError::NotConnected)?;
        let (reader, mut writer) = tokio::io::split(stream);
        let mut reader = reader;

        write_frame(&mut writer, &request)
            .await
            .map_err(ClientError::Io)?;
        let response: Response = read_frame(&mut reader)
            .await
            .map_err(ClientError::Io)?
            .ok_or_else(|| {
                ClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon closed before response frame",
                ))
            })?;

        match response {
            Response::Error { message } => Err(ClientError::Daemon(message)),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::ResolveWire(_)
            | Response::Ok) => Ok(other),
        }
    }

    pub async fn ping(&self) -> Result<(), ClientError> {
        self.ping_with_timeout(DEFAULT_RPC_TIMEOUT).await
    }

    pub async fn ping_with_timeout(&self, deadline: Duration) -> Result<(), ClientError> {
        match self.call_with_timeout(Request::Ping, deadline).await? {
            Response::Pong => Ok(()),
            other @ (Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        self.status_with_timeout(DEFAULT_RPC_TIMEOUT).await
    }

    pub async fn status_with_timeout(
        &self,
        deadline: Duration,
    ) -> Result<StatusResponse, ClientError> {
        match self.call_with_timeout(Request::Status, deadline).await? {
            Response::Status(status) => Ok(status),
            other @ (Response::Pong
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn send(&self, request: SendRequest) -> Result<(), ClientError> {
        match self.call(Request::Send(request)).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn publish(&self, request: PublishRequest) -> Result<PublishResponse, ClientError> {
        match self.call(Request::Publish(request)).await? {
            Response::Publish(response) => Ok(response),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Peers(_)
            | Response::Ok
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    /// Look up the wire path the daemon uses for a given channel
    /// UUID. Returns `None` inside [`ResolveWireResponse`] when the
    /// daemon has not subscribed that channel yet — same non-auto-
    /// join discipline as [`Airc::publish`](airc-lib's publish API).
    pub async fn resolve_wire(
        &self,
        request: ResolveWireRequest,
    ) -> Result<ResolveWireResponse, ClientError> {
        match self.call(Request::ResolveWire(request)).await? {
            Response::ResolveWire(response) => Ok(response),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn subscribe(&self, request: SubscribeRequest) -> Result<(), ClientError> {
        self.subscribe_with_timeout(request, SUBSCRIBE_RPC_TIMEOUT)
            .await
    }

    pub async fn subscribe_with_timeout(
        &self,
        request: SubscribeRequest,
        deadline: Duration,
    ) -> Result<(), ClientError> {
        match self
            .call_with_timeout(Request::Subscribe(request), deadline)
            .await?
        {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn inbox(&self, request: InboxRequest) -> Result<InboxResponse, ClientError> {
        match self.call(Request::Inbox(request)).await? {
            Response::Inbox(response) => Ok(response),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn stop(&self) -> Result<(), ClientError> {
        match self.call(Request::Stop).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn add_peer(&self, request: AddPeerRequest) -> Result<(), ClientError> {
        match self.call(Request::AddPeer(request)).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn remove_peer(&self, request: RemovePeerRequest) -> Result<(), ClientError> {
        match self.call(Request::RemovePeer(request)).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Peers(_)
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    // Reserved for CLI surfaces that want the daemon's authoritative
    // in-memory peer view instead of opening the store directly.
    #[allow(dead_code)]
    pub async fn list_peers(&self) -> Result<PeersResponse, ClientError> {
        match self.call(Request::ListPeers).await? {
            Response::Peers(response) => Ok(response),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Event { .. }
            | Response::Publish(_)
            | Response::Ok
            | Response::ResolveWire(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn attach(&self, request: AttachRequest) -> Result<IpcStream, ClientError> {
        timeout(Duration::from_secs(5), self.attach_inner(request))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn attach_inner(&self, request: AttachRequest) -> Result<IpcStream, ClientError> {
        let mut stream = IpcStream::connect(&self.socket_path)
            .await
            .map_err(ClientError::NotConnected)?;
        write_frame(&mut stream, &Request::Attach(request))
            .await
            .map_err(ClientError::Io)?;
        Ok(stream)
    }
}
