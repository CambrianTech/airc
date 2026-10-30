//! Connection-bound addressed authentication and local opaque delivery.
use std::sync::Arc;

use airc_core::{MentionTarget, PeerId};
use airc_ipc::codec::{read_frame, write_frame};
use airc_ipc::{Request, Response};
use airc_protocol::addressed::addressed_channel;
use airc_protocol::addressed_attach::AttachChallenge;
use airc_protocol::{Frame, FrameKind, Signature, VerificationPolicy};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite};

use crate::{DaemonError, DaemonState};

fn refusal(message: impl Into<String>) -> Response {
    Response::Error {
        message: message.into(),
    }
}

impl DaemonState {
    /// Receive an opaque transport frame through the same authenticated local
    /// admission as IPC, without entering the durable room router.
    pub async fn receive_addressed_frame(
        &self,
        frame: Frame,
    ) -> Result<(), AddressedAdmissionError> {
        let deadline = tokio::time::Instant::now() + airc_ipc::client::DEFAULT_RPC_TIMEOUT;
        tokio::time::timeout_at(deadline, admit_local(self, frame, deadline))
            .await
            .unwrap_or(Err(AddressedAdmissionError::Queue(
                crate::addressed_switch::RouteError::DeadlineElapsed,
            )))
    }
}

#[derive(Debug)]
pub enum AddressedAdmissionError {
    WrongKindOrChannel,
    WrongTarget,
    SenderMustSign,
    Trust(crate::trust_refresh::TrustRefreshError),
    Signature(airc_protocol::VerificationError),
    Queue(crate::addressed_switch::RouteError),
    Forward(crate::addressed_switch::ForwardError),
    Expiry(airc_protocol::addressed_deadline::DeadlineError),
}

impl std::fmt::Display for AddressedAdmissionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "addressed admission refused: {self:?}")
    }
}
impl std::error::Error for AddressedAdmissionError {}

pub(crate) async fn route_request(
    state: &DaemonState,
    request: airc_ipc::AddressedRequest,
) -> Response {
    let Some(expires) = std::time::UNIX_EPOCH
        .checked_add(std::time::Duration::from_millis(request.deadline_unix_ms))
    else {
        return refusal("addressed deadline is out of range");
    };
    let Ok(remaining) = expires.duration_since(std::time::SystemTime::now()) else {
        return refusal("addressed deadline elapsed");
    };
    if remaining.is_zero() {
        return refusal("addressed deadline elapsed");
    }
    let remaining = remaining.min(airc_ipc::client::DEFAULT_RPC_TIMEOUT);
    let deadline = tokio::time::Instant::now() + remaining;
    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);
    shutdown.as_mut().enable();
    tokio::select! {
        biased;
        _ = &mut shutdown => refusal("addressed daemon shutting down"),
        result = tokio::time::timeout_at(deadline, admit_ipc(state, request.frame, deadline)) => match result {
            Ok(Ok(())) => Response::Ok,
            Ok(Err(error)) => refusal(error.to_string()),
            Err(_) => refusal("addressed deadline elapsed"),
        },
    }
}

async fn admit_local(
    state: &DaemonState,
    frame: Frame,
    deadline: tokio::time::Instant,
) -> Result<(), AddressedAdmissionError> {
    let peer = verify_addressed(state, &frame).await?;
    let deadline = sender_deadline(&frame, deadline)?;
    state
        .addressed
        .route(peer, frame, deadline)
        .map_err(AddressedAdmissionError::Queue)
}

async fn admit_ipc(
    state: &DaemonState,
    frame: Frame,
    deadline: tokio::time::Instant,
) -> Result<(), AddressedAdmissionError> {
    let peer = verify_addressed(state, &frame).await?;
    let deadline = sender_deadline(&frame, deadline)?;
    match state
        .addressed
        .route_if_owned(peer, frame, deadline)
        .map_err(AddressedAdmissionError::Queue)?
    {
        crate::addressed_switch::LocalAdmission::Delivered => Ok(()),
        crate::addressed_switch::LocalAdmission::Unowned(frame) => state
            .addressed
            .forward(peer, *frame, deadline)
            .await
            .map_err(AddressedAdmissionError::Forward),
    }
}

fn sender_deadline(
    frame: &Frame,
    local: tokio::time::Instant,
) -> Result<tokio::time::Instant, AddressedAdmissionError> {
    let monotonic = tokio::time::Instant::now();
    let remaining = airc_protocol::addressed_deadline::remaining(
        &frame.envelope.headers,
        std::time::SystemTime::now(),
    )
    .map_err(AddressedAdmissionError::Expiry)?;
    // Cap before adding, so a malicious far-future expiry cannot overflow Instant.
    Ok(monotonic + remaining.min(local.saturating_duration_since(monotonic)))
}

async fn verify_addressed(
    state: &DaemonState,
    frame: &Frame,
) -> Result<PeerId, AddressedAdmissionError> {
    if frame.kind != FrameKind::Addressed || frame.envelope.channel != addressed_channel() {
        return Err(AddressedAdmissionError::WrongKindOrChannel);
    }
    let MentionTarget::Peer(peer) = &frame.envelope.target else {
        return Err(AddressedAdmissionError::WrongTarget);
    };
    if !matches!(&frame.envelope.signature, Signature::Ed25519 { signer, .. } if *signer == frame.envelope.sender)
    {
        return Err(AddressedAdmissionError::SenderMustSign);
    }
    let registry =
        match crate::trust_refresh::admission_registry(&state.home, frame.envelope.sender).await {
            Ok(registry) => registry,
            Err(error) => return Err(AddressedAdmissionError::Trust(error)),
        };
    if let Err(error) = airc_protocol::verify(frame, VerificationPolicy::Strict, &registry) {
        return Err(AddressedAdmissionError::Signature(error));
    }
    Ok(*peer)
}

pub(crate) async fn serve<R, W>(
    reader: R,
    writer: W,
    state: Arc<DaemonState>,
    peer: PeerId,
) -> Result<(), DaemonError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    // Register before authentication, retain across every read/write and queue wait.
    let shutdown = state.shutdown.notified();
    tokio::pin!(shutdown);
    shutdown.as_mut().enable();
    tokio::select! {
        biased;
        _ = &mut shutdown => Ok(()),
        result = serve_live(reader, writer, state.clone(), peer) => result,
    }
}

async fn serve_live<R, W>(
    mut reader: R,
    mut writer: W,
    state: Arc<DaemonState>,
    peer: PeerId,
) -> Result<(), DaemonError>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let challenge = AttachChallenge {
        daemon: state.peer_id,
        peer,
        nonce: uuid::Uuid::new_v4(),
    };
    let authenticated = tokio::time::timeout(airc_ipc::client::DEFAULT_RPC_TIMEOUT, async {
        write_frame(
            &mut writer,
            &Response::AddressedChallenge(challenge.clone()),
        )
        .await?;
        let request = read_frame::<_, Request>(&mut reader).await?;
        Ok::<_, std::io::Error>(match request {
            Some(Request::ProveAddressed(proof)) => {
                let registry = crate::trust_refresh::admission_registry(&state.home, peer)
                    .await
                    .map_err(std::io::Error::other)?;
                challenge.verify(&proof, &registry).is_ok()
            }
            _ => false,
        })
    })
    .await;
    match authenticated {
        Ok(Ok(true)) => {}
        Ok(Err(error)) => return Err(error.into()),
        _ => {
            write_frame(
                &mut writer,
                &refusal("addressed connection authentication refused"),
            )
            .await?;
            return Ok(());
        }
    }
    let mut lease = match state.addressed.register(peer) {
        Ok(lease) => lease,
        Err(error) => {
            write_frame(
                &mut writer,
                &refusal(format!("addressed attach refused: {error:?}")),
            )
            .await?;
            return Ok(());
        }
    };
    // Every wait and socket write is interruptible by reconnect or shutdown.
    // Dropping this lease cannot remove a more recent registration.
    tokio::select! {
        biased;
        _ = lease.superseded.changed() => return Ok(()),
        result = write_frame(&mut writer, &Response::Ok) => result?,
    }
    let mut unexpected = [0u8; 1];
    loop {
        tokio::select! {
            biased;
            _ = lease.superseded.changed() => return Ok(()),
                _ = reader.read(&mut unexpected) => return Ok(()),
            next = lease.rx.recv() => {
                let Some(queued) = next else { return Ok(()); };

                tokio::select! {
                    biased;
                    _ = lease.superseded.changed() => return Ok(()),
                                result = write_queued(&mut writer, queued) => result?,
                }

            }
        }
    }
}

// A timeout may leave a partial IPC frame: the caller must close the stream.
async fn write_queued<W: AsyncWrite + Unpin>(
    writer: &mut W,
    queued: crate::addressed_switch::QueuedFrame,
) -> std::io::Result<()> {
    let crate::addressed_switch::QueuedFrame {
        frame,
        deadline,
        _permit,
    } = queued;
    if tokio::time::Instant::now() >= deadline {
        return Ok(());
    }
    tokio::time::timeout_at(
        deadline,
        write_frame(writer, &Response::Addressed { frame }),
    )
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "addressed write deadline elapsed",
        )
    })?
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_protocol::{PeerKeyRegistry, PeerKeypair};
    use airc_store::InMemoryEventStore;
    use tokio::io::AsyncWriteExt;

    #[tokio::test]
    async fn signed_missing_or_expired_sender_budget_never_reaches_local_queue() {
        let (_home, state, peer, key) = fixture().await;
        let mut receiver = state.addressed.register(peer).unwrap();
        let mut frame = test_frame(peer);
        frame.envelope.signature = key.sign_envelope(&frame.envelope, peer, 0).unwrap();
        assert!(matches!(
            state.receive_addressed_frame(frame.clone()).await,
            Err(AddressedAdmissionError::Expiry(
                airc_protocol::addressed_deadline::DeadlineError::Missing
            ))
        ));
        frame.envelope.headers.insert(
            airc_protocol::addressed_deadline::EXPIRY_HEADER.into(),
            "1".into(),
        );
        frame.envelope.signature = key.sign_envelope(&frame.envelope, peer, 0).unwrap();
        assert!(matches!(
            state.receive_addressed_frame(frame.clone()).await,
            Err(AddressedAdmissionError::Expiry(
                airc_protocol::addressed_deadline::DeadlineError::Elapsed
            ))
        ));
        airc_protocol::addressed_deadline::stamp(
            &mut frame.envelope.headers,
            std::time::SystemTime::now(),
            airc_ipc::client::DEFAULT_RPC_TIMEOUT,
        )
        .unwrap();
        // Extending the signed header without the sender's key is not authority.
        assert!(matches!(
            state.receive_addressed_frame(frame).await,
            Err(AddressedAdmissionError::Signature(_))
        ));
        assert!(matches!(
            receiver.rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[derive(Default)]
    struct RecordingForwarder(std::sync::Mutex<Vec<Frame>>);

    #[async_trait::async_trait]
    impl crate::addressed_switch::AddressedForwarder for RecordingForwarder {
        async fn forward(
            &self,
            recipient: PeerId,
            frame: Frame,
            _deadline: tokio::time::Instant,
        ) -> Result<(), crate::addressed_switch::ForwardError> {
            assert_eq!(frame.envelope.target, MentionTarget::Peer(recipient));
            self.0.lock().unwrap().push(frame);
            Ok(())
        }
    }

    #[tokio::test]
    async fn ipc_prefers_local_owner_and_never_forwards_pressure_or_transport_ingress() {
        let (_home, state, peer, key) = fixture().await;
        let forwarder = Arc::new(RecordingForwarder::default());
        state.addressed.set_forwarder(forwarder.clone()).unwrap();
        let mut frame = test_frame(peer);
        airc_protocol::addressed_deadline::stamp(
            &mut frame.envelope.headers,
            std::time::SystemTime::now(),
            airc_ipc::client::DEFAULT_RPC_TIMEOUT,
        )
        .unwrap();
        frame.envelope.signature = key.sign_envelope(&frame.envelope, peer, 0).unwrap();
        let deadline = tokio::time::Instant::now() + airc_ipc::client::DEFAULT_RPC_TIMEOUT;
        let mut owner = state.addressed.register(peer).unwrap();
        admit_ipc(&state, frame.clone(), deadline).await.unwrap();
        assert_eq!(owner.rx.recv().await.unwrap().frame, frame);
        let mut permits = Vec::new();
        while let Ok(permit) = state.addressed.reserve_frame() {
            permits.push(permit);
        }
        assert!(matches!(
            admit_ipc(&state, frame.clone(), deadline).await,
            Err(AddressedAdmissionError::Queue(
                crate::addressed_switch::RouteError::Backpressure
            ))
        ));
        assert!(forwarder.0.lock().unwrap().is_empty());
        drop(permits);
        drop(owner);
        assert!(matches!(
            state.receive_addressed_frame(frame.clone()).await,
            Err(AddressedAdmissionError::Queue(
                crate::addressed_switch::RouteError::NoReceiver
            ))
        ));
        assert!(forwarder.0.lock().unwrap().is_empty());
        admit_ipc(&state, frame.clone(), deadline).await.unwrap();
        assert_eq!(*forwarder.0.lock().unwrap(), vec![frame]);
    }

    async fn fixture() -> (tempfile::TempDir, Arc<DaemonState>, PeerId, PeerKeypair) {
        let home = tempfile::tempdir().unwrap();
        let peer = PeerId::new();
        let key = PeerKeypair::generate();
        let registry = Arc::new(PeerKeyRegistry::new());
        airc_trust::add(home.path(), peer, key.public_bytes())
            .await
            .unwrap();
        let state = DaemonState::build(
            PeerId::new(),
            PeerKeypair::generate(),
            registry,
            VerificationPolicy::Strict,
            home.path().to_owned(),
            &home.path().join("events.sqlite"),
            Arc::new(InMemoryEventStore::new()),
            crate::DaemonRuntimeInfo::unknown(),
        )
        .await
        .unwrap();
        (home, Arc::new(state), peer, key)
    }

    fn test_frame(peer: PeerId) -> Frame {
        Frame {
            kind: FrameKind::Addressed,
            envelope: airc_protocol::Envelope {
                event_id: airc_core::EventId::new(),
                sender: peer,
                sender_client: airc_core::ClientId::new(),
                channel: addressed_channel(),
                target: MentionTarget::Peer(peer),
                lamport: 1,
                occurred_at_ms: 1,
                reply_to: None,
                headers: Default::default(),
                body: None,
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    #[tokio::test]
    async fn queued_write_expiry_releases_budget_without_late_output() {
        use crate::addressed_switch::{AddressedSwitch, QueuedFrame};
        use std::num::NonZeroUsize;
        let switch = AddressedSwitch::new(NonZeroUsize::MIN, NonZeroUsize::MIN, NonZeroUsize::MIN);
        let peer = PeerId::new();
        let (mut writer, mut reader) = tokio::io::duplex(1);
        let expired = QueuedFrame {
            frame: test_frame(peer),
            deadline: tokio::time::Instant::now(),
            _permit: switch.reserve_frame().unwrap(),
        };
        write_queued(&mut writer, expired).await.unwrap();
        let permit = switch
            .reserve_frame()
            .expect("expired frame released budget");
        let mut byte = [0];
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), reader.read(&mut byte))
                .await
                .is_err()
        );
        let blocked = QueuedFrame {
            frame: test_frame(peer),
            deadline: tokio::time::Instant::now() + std::time::Duration::from_millis(100),
            _permit: permit,
        };
        let writing = write_queued(&mut writer, blocked);
        tokio::pin!(writing);
        tokio::select! {
            result = &mut writing => panic!("write finished before backpressure: {result:?}"),
            result = reader.read_exact(&mut byte) => { result.unwrap(); }
        }
        assert!(matches!(
            switch.reserve_frame(),
            Err(crate::addressed_switch::RouteError::Backpressure)
        ));
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut writing)
            .await
            .unwrap();
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        assert!(
            switch.reserve_frame().is_ok(),
            "timed out write released budget"
        );
        // This stream has a partial frame and is dropped without reuse.
    }

    #[tokio::test]
    async fn queued_write_timeout_closes_authenticated_stream() {
        let (_home, state, peer, key) = fixture().await;
        let (mut client, server) = tokio::io::duplex(64);
        let (reader, writer) = tokio::io::split(server);
        let task = tokio::spawn(serve(reader, writer, state.clone(), peer));
        let Some(Response::AddressedChallenge(challenge)) =
            read_frame::<_, Response>(&mut client).await.unwrap()
        else {
            panic!("no challenge");
        };
        write_frame(
            &mut client,
            &Request::ProveAddressed(challenge.sign(&key, 0)),
        )
        .await
        .unwrap();
        assert_eq!(
            read_frame::<_, Response>(&mut client).await.unwrap(),
            Some(Response::Ok)
        );
        let mut frame = test_frame(peer);
        frame.envelope.body = Some(airc_core::Body::Binary(vec![0; 1024]));
        state
            .addressed
            .route(
                peer,
                frame,
                tokio::time::Instant::now() + std::time::Duration::from_millis(100),
            )
            .unwrap();
        let result = tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert!(
            result.is_err(),
            "partial frame timeout must close the connection"
        );
        let mut partial = Vec::new();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.read_to_end(&mut partial),
        )
        .await
        .unwrap()
        .unwrap();
        assert!(!partial.is_empty(), "fixture must exercise a partial frame");
        assert!(state
            .addressed
            .register(peer)
            .unwrap()
            .is_current()
            .unwrap());
    }

    #[tokio::test]
    async fn expired_ipc_frame_is_refused_before_admission() {
        let (_home, state, peer, _key) = fixture().await;
        let mut receiver = state.addressed.register(peer).unwrap();
        let frame = test_frame(peer);
        let response = route_request(
            &state,
            airc_ipc::AddressedRequest {
                frame,
                deadline_unix_ms: 1,
            },
        )
        .await;
        assert_eq!(response, refusal("addressed deadline elapsed"));
        assert!(matches!(
            receiver.rx.try_recv(),
            Err(tokio::sync::mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn stale_shared_cache_cannot_authenticate_after_durable_rotation() {
        let (_home, state, peer, old) = fixture().await;
        let next = PeerKeypair::generate();
        let rotation =
            airc_protocol::trust_rotation::sign_rotation(&old, peer, next.public_bytes(), 1, 1)
                .unwrap();
        airc_trust::rotate(&state.home, &rotation).await.unwrap();
        crate::trust_refresh::refresh_peer(&state.registry, &state.home, peer, None)
            .await
            .unwrap();
        // Deterministically finish a delayed old cache write after the new one.
        state.registry.enrol(peer, 0, old.public_bytes()).unwrap();
        state.registry.enrol(peer, 7, old.public_bytes()).unwrap();
        let registry = crate::trust_refresh::admission_registry(&state.home, peer)
            .await
            .unwrap();
        let challenge = AttachChallenge {
            daemon: state.peer_id,
            peer,
            nonce: uuid::Uuid::new_v4(),
        };
        assert!(challenge
            .verify(&challenge.sign(&old, 0), &registry)
            .is_err());
        assert!(challenge
            .verify(&challenge.sign(&old, 7), &registry)
            .is_err());
        assert!(challenge
            .verify(&challenge.sign(&next, 0), &registry)
            .is_ok());
        airc_trust::remove(&state.home, peer).await.unwrap();
        assert!(crate::trust_refresh::admission_registry(&state.home, peer)
            .await
            .is_err());
    }

    #[tokio::test]
    async fn add_peer_cannot_bootstrap_or_replace_durable_identity() {
        use base64::Engine;
        let (_home, state, peer, key) = fixture().await;
        let impostor = PeerKeypair::generate();
        let request = |peer_id, public_key| {
            Request::AddPeer(airc_ipc::AddPeerRequest {
                peer_id,
                pubkey_b64: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(public_key),
            })
        };
        assert_eq!(
            crate::handlers::dispatch(state.clone(), request(peer, key.public_bytes())).await,
            Response::Ok
        );
        assert!(matches!(
            crate::handlers::dispatch(state.clone(), request(peer, impostor.public_bytes())).await,
            Response::Error { .. }
        ));
        assert_eq!(
            state.registry.lookup(peer, 0).unwrap().to_bytes(),
            key.public_bytes()
        );
        let unknown = PeerId::new();
        assert!(matches!(
            crate::handlers::dispatch(state.clone(), request(unknown, impostor.public_bytes()))
                .await,
            Response::Error { .. }
        ));
        assert!(state.registry.lookup(unknown, 0).is_none());
        // A fresh verifier (daemon restart) recovers admission from durable trust.
        let restarted = PeerKeyRegistry::new();
        crate::trust_refresh::refresh_peer(&restarted, &state.home, peer, None)
            .await
            .unwrap();
        assert_eq!(
            restarted.lookup(peer, 0).unwrap().to_bytes(),
            key.public_bytes()
        );
    }

    #[tokio::test]
    async fn shutdown_interrupts_authentication_before_proof() {
        let (_home, state, peer, _key) = fixture().await;
        let (mut client, server) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(server);
        let task = tokio::spawn(serve(reader, writer, state.clone(), peer));
        assert!(matches!(
            read_frame::<_, Response>(&mut client).await.unwrap(),
            Some(Response::AddressedChallenge(_))
        ));
        state.shutdown.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn attached_stream_refuses_first_unexpected_byte_without_reading_a_frame() {
        let (_home, state, peer, key) = fixture().await;
        let (mut client, server) = tokio::io::duplex(4096);
        let (reader, writer) = tokio::io::split(server);
        let task = tokio::spawn(serve(reader, writer, state.clone(), peer));
        let Some(Response::AddressedChallenge(challenge)) =
            read_frame::<_, Response>(&mut client).await.unwrap()
        else {
            panic!("no challenge");
        };
        write_frame(
            &mut client,
            &Request::ProveAddressed(challenge.sign(&key, 0)),
        )
        .await
        .unwrap();
        assert_eq!(
            read_frame::<_, Response>(&mut client).await.unwrap(),
            Some(Response::Ok)
        );
        // A single prefix byte used to leave read_frame waiting for the rest.
        client.write_all(&[1]).await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(state
            .addressed
            .register(peer)
            .unwrap()
            .is_current()
            .unwrap());
    }
}
