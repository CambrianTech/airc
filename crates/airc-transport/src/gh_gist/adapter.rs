use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::Stream;
use tokio::sync::mpsc;
use tokio::time::sleep;

use airc_core::TranscriptCursor;
use airc_protocol::{Frame, FrameKind, Subscription};

use crate::transport::{FrameStream, Transport};

use super::client::{GhCliClient, GistClient};
use super::error::GhGistError;

const DEFAULT_POLL_INTERVAL: Duration = Duration::from_secs(15);
const RECEIVER_CHANNEL_DEPTH: usize = 64;

pub struct GhGistAdapter<C = GhCliClient> {
    gist_id: String,
    client: Arc<C>,
    poll_interval: Duration,
}

impl GhGistAdapter<GhCliClient> {
    pub fn new(gist_id: impl Into<String>) -> Self {
        Self::with_client(gist_id, GhCliClient::new())
    }
}

impl<C> GhGistAdapter<C>
where
    C: GistClient + 'static,
{
    pub fn with_client(gist_id: impl Into<String>, client: C) -> Self {
        Self {
            gist_id: gist_id.into(),
            client: Arc::new(client),
            poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }
}

#[async_trait]
impl<C> Transport for GhGistAdapter<C>
where
    C: GistClient + 'static,
{
    type Error = GhGistError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        let mut content = self.client.get_messages(&self.gist_id).await?;
        let serialized = serde_json::to_string(&frame)?;
        if !content.is_empty() && !content.ends_with('\n') {
            content.push('\n');
        }
        content.push_str(&serialized);
        content.push('\n');
        self.client.put_messages(&self.gist_id, &content).await
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        let initial_seen = if subscription.from_cursor.is_some() {
            0
        } else {
            self.client
                .get_messages(&self.gist_id)
                .await?
                .lines()
                .count()
        };
        let gist_id = self.gist_id.clone();
        let client = Arc::clone(&self.client);
        let poll_interval = self.poll_interval;
        let (tx, rx) = mpsc::channel(RECEIVER_CHANNEL_DEPTH);

        tokio::spawn(async move {
            tail_loop(
                client,
                gist_id,
                subscription,
                initial_seen,
                poll_interval,
                tx.clone(),
            )
            .await;
        });

        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}

async fn tail_loop<C>(
    client: Arc<C>,
    gist_id: String,
    subscription: Subscription,
    mut seen_lines: usize,
    poll_interval: Duration,
    tx: mpsc::Sender<Result<Frame, GhGistError>>,
) where
    C: GistClient + 'static,
{
    loop {
        if tx.is_closed() {
            return;
        }

        match client.get_messages(&gist_id).await {
            Ok(content) => {
                let lines = content.lines().collect::<Vec<_>>();
                if seen_lines > lines.len() {
                    seen_lines = 0;
                }
                for line in &lines[seen_lines..] {
                    let frame = match serde_json::from_str::<Frame>(line) {
                        Ok(frame) => frame,
                        Err(error) => {
                            let _ = tx.send(Err(GhGistError::Json(error))).await;
                            return;
                        }
                    };
                    if !subscription.matches(&frame) {
                        continue;
                    }
                    if let Some(cursor) = &subscription.from_cursor {
                        if !frame_after_cursor(&frame, cursor) {
                            continue;
                        }
                    }
                    if dispatch_frame(&tx, frame).await.is_err() {
                        return;
                    }
                }
                seen_lines = lines.len();
            }
            Err(error) => {
                let _ = tx.send(Err(error)).await;
                return;
            }
        }

        sleep(poll_interval).await;
    }
}

async fn dispatch_frame(
    tx: &mpsc::Sender<Result<Frame, GhGistError>>,
    frame: Frame,
) -> Result<(), ()> {
    match frame.kind {
        FrameKind::Message | FrameKind::Control => tx.send(Ok(frame)).await.map_err(|_| ()),
        FrameKind::Event => match tx.try_send(Ok(frame)) {
            Ok(()) | Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
        },
    }
}

fn frame_after_cursor(frame: &Frame, cursor: &TranscriptCursor) -> bool {
    let envelope = &frame.envelope;
    envelope.lamport > cursor.lamport
        || (envelope.lamport == cursor.lamport
            && envelope.event_id.as_uuid() > cursor.event_id.as_uuid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::EventId;
    use airc_core::{headers::Headers, transcript::MentionTarget, Body, ClientId, PeerId, RoomId};
    use airc_protocol::{Envelope, Signature};
    use std::sync::Mutex;

    #[derive(Default)]
    struct MemoryGistClient {
        content: Mutex<String>,
    }

    #[async_trait]
    impl GistClient for MemoryGistClient {
        async fn get_messages(&self, _gist_id: &str) -> Result<String, GhGistError> {
            Ok(self.content.lock().unwrap().clone())
        }

        async fn put_messages(&self, _gist_id: &str, content: &str) -> Result<(), GhGistError> {
            *self.content.lock().unwrap() = content.to_string();
            Ok(())
        }
    }

    fn frame_at(lamport: u64, channel: RoomId, body: &str) -> Frame {
        Frame {
            kind: FrameKind::Message,
            envelope: Envelope {
                event_id: EventId::from_u128(lamport as u128),
                sender: PeerId::from_u128(0xa1),
                sender_client: ClientId::from_u128(0xc1),
                channel,
                target: MentionTarget::All,
                lamport,
                occurred_at_ms: 1_700_000_000_000 + lamport,
                reply_to: None,
                headers: Headers::new(),
                body: Some(Body::text(body)),
                media: Vec::new(),
                signature: Signature::Unsigned,
            },
        }
    }

    #[tokio::test]
    async fn send_appends_frame_to_messages_file_content() {
        let adapter = GhGistAdapter::with_client("gist-a", MemoryGistClient::default());
        let channel = RoomId::from_u128(0xc0ffee);

        adapter
            .send(frame_at(1, channel, "hello over gist"))
            .await
            .unwrap();

        let content = adapter.client.get_messages("gist-a").await.unwrap();
        let stored: Frame = serde_json::from_str(content.trim()).unwrap();
        assert_eq!(
            stored.envelope.body.as_ref().and_then(Body::as_text),
            Some("hello over gist")
        );
    }

    #[tokio::test]
    async fn subscribe_replays_from_cursor_and_filters_channel() {
        let client = MemoryGistClient::default();
        let adapter = GhGistAdapter::with_client("gist-a", client)
            .with_poll_interval(Duration::from_millis(5));
        let channel = RoomId::from_u128(0xc0ffee);
        let other = RoomId::from_u128(0xbad);
        adapter
            .send(frame_at(1, other, "wrong room"))
            .await
            .unwrap();
        adapter
            .send(frame_at(2, channel, "right room"))
            .await
            .unwrap();

        let mut stream = adapter
            .subscribe(Subscription {
                channel: Some(channel),
                from_cursor: Some(TranscriptCursor {
                    lamport: 0,
                    event_id: EventId::from_u128(0),
                }),
                ..Default::default()
            })
            .await
            .unwrap();

        let received = tokio::time::timeout(
            Duration::from_secs(1),
            futures::StreamExt::next(&mut stream),
        )
        .await
        .expect("must yield")
        .expect("stream open")
        .expect("frame ok");
        assert_eq!(received.envelope.lamport, 2);
    }
}
