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
