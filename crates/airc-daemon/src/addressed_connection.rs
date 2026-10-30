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

pub(crate) async fn route_local(state: &DaemonState, frame: Frame) -> Response {
    if frame.kind != FrameKind::Addressed || frame.envelope.channel != addressed_channel() {
        return refusal("addressed route refuses room or plaintext frames");
    }
    let MentionTarget::Peer(peer) = &frame.envelope.target else {
        return refusal("addressed route requires one peer");
    };
    if !matches!(&frame.envelope.signature, Signature::Ed25519 { signer, .. } if *signer == frame.envelope.sender)
    {
        return refusal("local addressed sender must sign its own envelope");
    }
    let registry =
        match crate::trust_refresh::admission_registry(&state.home, frame.envelope.sender).await {
            Ok(registry) => registry,
            Err(error) => return refusal(format!("addressed trust refused: {error}")),
        };
    if let Err(error) = airc_protocol::verify(&frame, VerificationPolicy::Strict, &registry) {
        return refusal(format!("addressed signature refused: {error}"));
    }
    match state.addressed.route(*peer, frame) {
        Ok(()) => Response::Ok,
        Err(error) => refusal(format!("addressed local admission refused: {error:?}")),
    }
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
                let crate::addressed_switch::QueuedFrame { frame, _permit: permit } = queued;
                let response = Response::Addressed { frame };
                tokio::select! {
                    biased;
                    _ = lease.superseded.changed() => return Ok(()),
                                result = write_frame(&mut writer, &response) => result?,
                }
                drop(permit);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_protocol::{PeerKeyRegistry, PeerKeypair};
    use airc_store::InMemoryEventStore;
    use tokio::io::AsyncWriteExt;

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
