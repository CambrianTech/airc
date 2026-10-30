//! Connection-bound addressed authentication and local opaque delivery.
use std::sync::Arc;

use airc_core::{MentionTarget, PeerId};
use airc_ipc::codec::{read_frame, write_frame};
use airc_ipc::{Request, Response};
use airc_protocol::addressed::addressed_channel;
use airc_protocol::addressed_attach::AttachChallenge;
use airc_protocol::{Frame, FrameKind, Signature, VerificationPolicy};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{DaemonError, DaemonState};

fn refusal(message: impl Into<String>) -> Response {
    Response::Error {
        message: message.into(),
    }
}

pub(crate) fn route_local(state: &DaemonState, frame: Frame) -> Response {
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
    if let Err(error) = airc_protocol::verify(&frame, VerificationPolicy::Strict, &state.registry) {
        return refusal(format!("addressed signature refused: {error}"));
    }
    match state.addressed.route(*peer, frame) {
        Ok(()) => Response::Ok,
        Err(error) => refusal(format!("addressed local admission refused: {error:?}")),
    }
}

pub(crate) async fn serve<R, W>(
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
                challenge.verify(&proof, &state.registry).is_ok()
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
        _ = state.shutdown.notified() => return Ok(()),
        result = write_frame(&mut writer, &Response::Ok) => result?,
    }
    loop {
        tokio::select! {
            biased;
            _ = lease.superseded.changed() => return Ok(()),
            _ = state.shutdown.notified() => return Ok(()),
            _ = read_frame::<_, Request>(&mut reader) => return Ok(()),
            next = lease.rx.recv() => {
                let Some(queued) = next else { return Ok(()); };
                let crate::addressed_switch::QueuedFrame { frame, _permit: permit } = queued;
                let response = Response::Addressed { frame };
                tokio::select! {
                    biased;
                    _ = lease.superseded.changed() => return Ok(()),
                    _ = state.shutdown.notified() => return Ok(()),
                    result = write_frame(&mut writer, &response) => result?,
                }
                drop(permit);
            }
        }
    }
}
