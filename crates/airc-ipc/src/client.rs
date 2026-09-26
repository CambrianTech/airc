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
    AddPeerRequest, AttachRequest, InboxRequest, PeerIdentityCardRequest, PresenceRequest,
    PublishRequest, RemovePeerRequest, Request, RoomTipRequest, SendRequest,
};
use crate::response::{
    DeliveryStatsResponse, InboxResponse, PeerIdentityCardResponse, PeersResponse,
    PresenceResponse, PublishResponse, Response, RoomTipResponse, RoomsResponse,
    RouteEndpointsResponse, StatusResponse,
};

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(5);

/// Completed boundaries of an RPC, for opt-in diagnostic observation.
/// Failure or cancellation emits no boundary for the incomplete phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RpcPhase {
    Connected,
    RequestWritten,
    ResponseRead,
}

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
    ///
    /// Boxed: an error must not grow with the status wire — every field
    /// added to `StatusResponse` used to widen every `Result<_, ClientError>`
    /// (clippy `result_large_err` tripped at 136 bytes the day
    /// `connections` landed).
    UnexpectedResponse(Box<Response>),
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
        self.call_observed(request, deadline, |_| {}).await
    }

    /// Run the normal RPC path with synchronous phase-completion callbacks.
    /// Observers should be cheap: their work is inside the deadline and perturbs
    /// measured latency. ResponseRead means framing/decoding completed, including
    /// a decoded daemon error. Failed phases and cancellation emit no callback.
    /// Ordinary calls use a monomorphized no-op, with no clocks or allocations.
    pub async fn call_observed(
        &self,
        request: Request,
        deadline: Duration,
        observer: impl FnMut(RpcPhase),
    ) -> Result<Response, ClientError> {
        timeout(deadline, self.call_inner(request, observer))
            .await
            .map_err(|_| ClientError::Timeout)?
    }

    async fn call_inner(
        &self,
        request: Request,
        mut observer: impl FnMut(RpcPhase),
    ) -> Result<Response, ClientError> {
        let stream = IpcStream::connect(&self.socket_path)
            .await
            .map_err(ClientError::NotConnected)?;
        observer(RpcPhase::Connected);
        let (reader, mut writer) = tokio::io::split(stream);
        let mut reader = reader;

        write_frame(&mut writer, &request)
            .await
            .map_err(ClientError::Io)?;
        observer(RpcPhase::RequestWritten);
        let response: Response = read_frame(&mut reader)
            .await
            .map_err(ClientError::Io)?
            .ok_or_else(|| {
                ClientError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "daemon closed before response frame",
                ))
            })?;
        observer(RpcPhase::ResponseRead);

        match response {
            Response::Error { message } => Err(ClientError::Daemon(message)),
            other => Ok(other),
        }
    }

    pub async fn ping(&self) -> Result<(), ClientError> {
        self.ping_with_timeout(DEFAULT_RPC_TIMEOUT).await
    }

    pub async fn ping_with_timeout(&self, deadline: Duration) -> Result<(), ClientError> {
        match self.call_with_timeout(Request::Ping, deadline).await? {
            Response::Pong => Ok(()),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
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
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    /// Send a text message on a channel. Returns the owner-assigned
    /// receipt — the daemon publishes it through its `EventRouter`.
    pub async fn send(&self, request: SendRequest) -> Result<PublishResponse, ClientError> {
        match self.call(Request::Send(request)).await? {
            Response::Publish(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    pub async fn publish(&self, request: PublishRequest) -> Result<PublishResponse, ClientError> {
        match self.call(Request::Publish(request)).await? {
            Response::Publish(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    pub async fn inbox(&self, request: InboxRequest) -> Result<InboxResponse, ClientError> {
        match self.call(Request::Inbox(request)).await? {
            Response::Inbox(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    /// Card a1562dbc: the O(1) tip probe — cursor of the newest durable
    /// event on a channel, without replaying the room. The cheap
    /// freshness/watermark query; pair the returned cursor with
    /// `inbox(since: tip)` or `AttachStart::After(tip)`.
    /// airc#1341: the channel's live presence from the daemon's ephemeral cache.
    pub async fn presence(
        &self,
        request: PresenceRequest,
    ) -> Result<PresenceResponse, ClientError> {
        match self.call(Request::Presence(request)).await? {
            Response::Presence(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    pub async fn room_tip(&self, request: RoomTipRequest) -> Result<RoomTipResponse, ClientError> {
        match self.call(Request::RoomTip(request)).await? {
            Response::RoomTip(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    /// Resolve one peer's durable identity card from the daemon's
    /// owner-core identity index (`scoped_state`, `user:<peer>`, key
    /// `identity.card`). The attached-client read path for peer names: a
    /// client's local store never holds foreign peers' cards, so
    /// `peer_alias` / `peer_identity_card` ask the daemon — the identity
    /// analog of `room_tip` for the transcript.
    pub async fn peer_identity_card(
        &self,
        request: PeerIdentityCardRequest,
    ) -> Result<PeerIdentityCardResponse, ClientError> {
        match self.call(Request::PeerIdentityCard(request)).await? {
            Response::PeerIdentityCard(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    pub async fn stop(&self) -> Result<(), ClientError> {
        match self.call(Request::Stop).await? {
            Response::Ok => Ok(()),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    pub async fn add_peer(&self, request: AddPeerRequest) -> Result<(), ClientError> {
        match self.call(Request::AddPeer(request)).await? {
            Response::Ok => Ok(()),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    pub async fn remove_peer(&self, request: RemovePeerRequest) -> Result<(), ClientError> {
        match self.call(Request::RemovePeer(request)).await? {
            Response::Ok => Ok(()),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    /// Card 4b6a0ffa (#33): the dialable endpoints this daemon
    /// currently advertises in its account-registry beacon. An old
    /// daemon that predates the verb fails the call (decode error or
    /// `Response::Error`) — callers must treat ANY failure as
    /// "endpoints unavailable", never as publishable-empty.
    pub async fn route_endpoints(&self) -> Result<RouteEndpointsResponse, ClientError> {
        match self.call(Request::RouteEndpoints).await? {
            Response::RouteEndpoints(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    /// #1306 slice 2: per-peer end-to-end delivery accounting. The
    /// delivery-truth read — doctor prints "last confirmed delivery to
    /// X: N ago" from this instead of inferring health from TCP state.
    pub async fn delivery_stats(&self) -> Result<DeliveryStatsResponse, ClientError> {
        match self.call(Request::DeliveryStats).await? {
            Response::DeliveryStats(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    /// #270/#241: the scope's durable subscribed-room registry. The
    /// membership read continuum's nav seeds from — a member's rooms
    /// exist in the interface before their first event, not after.
    pub async fn list_rooms(&self) -> Result<RoomsResponse, ClientError> {
        match self.call(Request::ListRooms).await? {
            Response::Rooms(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
        }
    }

    // Reserved for CLI surfaces that want the daemon's authoritative
    // in-memory peer view instead of opening the store directly.
    #[allow(dead_code)]
    pub async fn list_peers(&self) -> Result<PeersResponse, ClientError> {
        match self.call(Request::ListPeers).await? {
            Response::Peers(response) => Ok(response),
            other => Err(ClientError::UnexpectedResponse(Box::new(other))),
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

#[cfg(test)]
mod observation_tests {
    use super::*;
    use crate::transport::IpcListener;

    // A server that holds the connection open must still hit the normal RPC
    // deadline, without reporting the unfinished response phase as complete.
    #[tokio::test]
    async fn observed_rpc_timeout_does_not_complete_response_phase() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("timeout.sock");
        let listener = IpcListener::bind(&socket).await.unwrap();
        let (release, hold) = tokio::sync::oneshot::channel::<()>();
        let server = tokio::spawn(async move {
            let mut stream = listener.accept().await.unwrap();
            assert!(matches!(
                read_frame::<_, Request>(&mut stream).await.unwrap(),
                Some(Request::Ping)
            ));
            let _ = hold.await;
            drop(stream);
            listener.cleanup();
        });
        let client = DaemonClient::new(socket);
        let mut phases = Vec::new();
        let result = client
            .call_observed(Request::Ping, Duration::from_secs(1), |phase| {
                phases.push(phase)
            })
            .await;
        assert!(matches!(result, Err(ClientError::Timeout)));
        assert_eq!(phases, [RpcPhase::Connected, RpcPhase::RequestWritten]);
        release.send(()).unwrap();
        server.await.unwrap();
    }

    // Completion callbacks follow the real codec path, including a decoded
    // daemon error; EOF must not advertise a response completion.
    #[tokio::test]
    async fn observed_rpc_preserves_responses_errors_and_incomplete_phases() {
        let home = tempfile::tempdir().unwrap();
        let socket = home.path().join("observed.sock");
        let listener = IpcListener::bind(&socket).await.unwrap();
        let server = tokio::spawn(async move {
            for response in [
                Some(Response::Pong),
                Some(Response::Error {
                    message: "rejected".into(),
                }),
                None,
            ] {
                let mut stream = listener.accept().await.unwrap();
                assert!(matches!(
                    read_frame::<_, Request>(&mut stream).await.unwrap(),
                    Some(Request::Ping)
                ));
                if let Some(response) = response {
                    write_frame(&mut stream, &response).await.unwrap();
                }
            }
            listener.cleanup();
        });
        let client = DaemonClient::new(socket);
        for n in 0..3 {
            let mut phases = Vec::new();
            let result = client
                .call_observed(Request::Ping, Duration::from_secs(5), |phase| {
                    phases.push(phase)
                })
                .await;
            assert_eq!(
                &phases[..2],
                &[RpcPhase::Connected, RpcPhase::RequestWritten]
            );
            if n < 2 {
                assert_eq!(
                    phases,
                    [
                        RpcPhase::Connected,
                        RpcPhase::RequestWritten,
                        RpcPhase::ResponseRead
                    ]
                );
            } else {
                assert_eq!(phases.len(), 2);
            }
            match n {
                0 => assert!(matches!(result, Ok(Response::Pong))),
                1 => assert!(
                    matches!(result, Err(ClientError::Daemon(message)) if message == "rejected")
                ),
                _ => assert!(
                    matches!(result, Err(ClientError::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof)
                ),
            }
        }
        server.await.unwrap();
    }
}
