//! Typed client used by CLI commands to talk to a running daemon.
//!
//! `DaemonClient::call(request)` opens a Unix socket, writes the
//! request as one newline-delimited JSON line, reads the response,
//! and parses it. One round-trip per connection — simple to debug
//! with `socat`, and the daemon's accept loop can spawn each
//! connection independently.
//!
//! Convenience helpers (`ping`, `status`, `send`, `stop`) wrap the
//! generic `call` and dispatch on response variants so callers don't
//! pattern-match on `Response` themselves.

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::ipc::transport::IpcStream;

use crate::ipc::request::{AddPeerRequest, InboxRequest, Request, SendRequest, SubscribeRequest};
use crate::ipc::response::{InboxResponse, PeersResponse, Response, StatusResponse};

/// Reasons a daemon RPC fails.
#[derive(Debug)]
pub enum ClientError {
    /// Couldn't connect to the socket — daemon not running.
    NotConnected(std::io::Error),
    /// Underlying socket I/O failure mid-call.
    Io(std::io::Error),
    /// Request or response failed to serialize/deserialize.
    Codec(serde_json::Error),
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
            ClientError::Daemon(_) | ClientError::UnexpectedResponse(_) => None,
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

    /// Generic RPC: writes a single newline-terminated request line,
    /// reads a single newline-terminated response line. No half-close
    /// — newline framing avoids the Windows-vs-Unix asymmetry where
    /// Unix sockets support `shutdown` half-close but named pipes
    /// don't. Both sides read until `\n`, parse, drop.
    pub async fn call(&self, request: Request) -> Result<Response, ClientError> {
        let stream = IpcStream::connect(&self.socket_path)
            .await
            .map_err(ClientError::NotConnected)?;
        let (reader, mut writer) = tokio::io::split(stream);
        let mut reader = BufReader::new(reader);

        let mut buffer = serde_json::to_vec(&request)?;
        buffer.push(b'\n');
        writer.write_all(&buffer).await.map_err(ClientError::Io)?;
        writer.flush().await.map_err(ClientError::Io)?;

        let mut response_line = String::new();
        reader
            .read_line(&mut response_line)
            .await
            .map_err(ClientError::Io)?;
        let response: Response = serde_json::from_str(response_line.trim_end_matches('\n'))?;

        match response {
            Response::Error { message } => Err(ClientError::Daemon(message)),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Ok) => Ok(other),
        }
    }

    pub async fn ping(&self) -> Result<(), ClientError> {
        match self.call(Request::Ping).await? {
            Response::Pong => Ok(()),
            other @ (Response::Status(_)
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn status(&self) -> Result<StatusResponse, ClientError> {
        match self.call(Request::Status).await? {
            Response::Status(status) => Ok(status),
            other @ (Response::Pong
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn send(&self, request: SendRequest) -> Result<(), ClientError> {
        match self.call(Request::Send(request)).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn subscribe(&self, request: SubscribeRequest) -> Result<(), ClientError> {
        match self.call(Request::Subscribe(request)).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn inbox(&self, request: InboxRequest) -> Result<InboxResponse, ClientError> {
        match self.call(Request::Inbox(request)).await? {
            Response::Inbox(response) => Ok(response),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Peers(_)
            | Response::Ok
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn stop(&self) -> Result<(), ClientError> {
        match self.call(Request::Stop).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    pub async fn add_peer(&self, request: AddPeerRequest) -> Result<(), ClientError> {
        match self.call(Request::AddPeer(request)).await? {
            Response::Ok => Ok(()),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Peers(_)
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }

    // Reserved for future CLI surfaces that want the daemon's
    // authoritative in-memory view (rather than reading peers.json
    // directly). The current `airc peer list` reads the file.
    #[allow(dead_code)]
    pub async fn list_peers(&self) -> Result<PeersResponse, ClientError> {
        match self.call(Request::ListPeers).await? {
            Response::Peers(response) => Ok(response),
            other @ (Response::Pong
            | Response::Status(_)
            | Response::Inbox(_)
            | Response::Ok
            | Response::Error { .. }) => Err(ClientError::UnexpectedResponse(other)),
        }
    }
}
