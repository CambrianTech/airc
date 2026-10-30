//! A transport host routes sealed frames to its local authenticated owner.
use super::AddressedError;
use airc_protocol::Frame;
use std::sync::Arc;

#[async_trait::async_trait]
pub trait AddressedFrameSink: Send + Sync {
    /// Admit the intact frame without opening it or publishing a room event.
    async fn deliver(&self, frame: Frame) -> Result<(), AddressedError>;
}

impl crate::Airc {
    /// Install once on the daemon's shared listener/dialer handle.
    pub fn set_addressed_frame_sink(
        &self,
        sink: Arc<dyn AddressedFrameSink>,
    ) -> Result<(), AddressedError> {
        self.inner
            .addressed
            .sink
            .set(sink)
            .map_err(|_| AddressedError::Transport("addressed frame sink already installed".into()))
    }

    /// Forward an intact addressed frame over an already connected host session.
    /// Host selection and durable admission belong to the daemon; this adapter
    /// never dials, broadcasts, changes the signature, or enters a room store.
    pub async fn forward_addressed_frame(
        &self,
        host: airc_core::PeerId,
        frame: Frame,
        deadline: tokio::time::Instant,
    ) -> Result<(), AddressedError> {
        if tokio::time::Instant::now() >= deadline {
            return Err(AddressedError::DeadlineExceeded);
        }
        if frame.kind != airc_protocol::FrameKind::Addressed
            || frame.envelope.channel != airc_protocol::addressed::addressed_channel()
            || !matches!(frame.envelope.target, airc_core::MentionTarget::Peer(_))
        {
            return Err(AddressedError::Transport(
                "opaque forwarding requires an addressed peer frame".into(),
            ));
        }
        tokio::time::timeout_at(deadline, async {
            let adapter = self.inner.lan_tcp.lock().await.clone().ok_or_else(|| {
                AddressedError::Transport("addressed host transport is not running".into())
            })?;
            adapter
                .send_to(host, frame)
                .await
                .map_err(|error| AddressedError::Transport(error.to_string()))
        })
        .await
        .unwrap_or(Err(AddressedError::DeadlineExceeded))
    }

    pub(crate) async fn dispatch_addressed_frame(&self, frame: Frame) {
        if let Some(sink) = self.inner.addressed.sink.get() {
            if let Err(error) = sink.deliver(frame).await {
                tracing::error!(target: "airc::addressed", %error, "opaque addressed admission refused");
            }
        } else {
            self.receive_addressed(frame).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::{ClientId, EventId, Headers, MentionTarget, PeerId};
    use airc_protocol::{Envelope, FrameKind, Signature};

    struct Capture(tokio::sync::Mutex<Vec<Frame>>);
    #[async_trait::async_trait]
    impl AddressedFrameSink for Capture {
        async fn deliver(&self, frame: Frame) -> Result<(), AddressedError> {
            self.0.lock().await.push(frame);
            Ok(())
        }
    }

    #[tokio::test]
    async fn host_intercepts_another_identity_before_endpoint_or_store() {
        let home = tempfile::tempdir().unwrap();
        let host = crate::Airc::open(home.path()).await.unwrap();
        let sink = Arc::new(Capture(tokio::sync::Mutex::new(Vec::new())));
        host.set_addressed_frame_sink(sink.clone()).unwrap();
        assert!(host.set_addressed_frame_sink(sink.clone()).is_err());
        let frame = Frame {
            kind: FrameKind::Addressed,
            envelope: Envelope {
                event_id: EventId::new(),
                sender: PeerId::new(),
                sender_client: ClientId::new(),
                channel: airc_protocol::addressed::addressed_channel(),
                target: MentionTarget::Peer(PeerId::new()),
                lamport: 1,
                occurred_at_ms: 1,
                reply_to: None,
                headers: Headers::new(),
                body: None,
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        };
        assert!(matches!(
            host.forward_addressed_frame(PeerId::new(), frame.clone(), tokio::time::Instant::now())
                .await,
            Err(AddressedError::DeadlineExceeded)
        ));
        assert!(host
            .forward_addressed_frame(
                PeerId::new(),
                frame.clone(),
                tokio::time::Instant::now() + airc_ipc::client::DEFAULT_RPC_TIMEOUT
            )
            .await
            .is_err());
        assert!(
            host.inner.lan_tcp.lock().await.is_none(),
            "forwarding must not create a transport"
        );
        host.append_received_frame(frame.clone()).await;
        assert_eq!(*sink.0.lock().await, vec![frame]);
        assert!(host
            .event_store()
            .page_recent(None, 100)
            .await
            .unwrap()
            .is_empty());
    }
}
