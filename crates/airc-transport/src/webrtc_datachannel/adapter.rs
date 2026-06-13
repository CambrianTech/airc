use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::Stream;
use tokio::sync::{mpsc, Mutex};
use webrtc::data_channel::{DataChannel, DataChannelEvent};

use airc_protocol::{Frame, FrameKind, Subscription};

use crate::transport::{FrameStream, Transport};
use crate::webrtc_datachannel::error::WebRtcDataChannelError;

const MAX_DATACHANNEL_FRAME_BYTES: usize = 16 * 1024;
const SUBSCRIBER_CHANNEL_DEPTH: usize = 64;

struct SubscriberHandle {
    #[allow(dead_code)]
    id: u64,
    subscription: Subscription,
    tx: mpsc::Sender<Result<Frame, WebRtcDataChannelError>>,
}

struct Inner {
    channel: Arc<dyn DataChannel>,
    is_open: AtomicBool,
    subscribers: Mutex<Vec<SubscriberHandle>>,
    next_sub_id: AtomicU64,
}

#[derive(Clone)]
pub struct WebRtcDataChannelAdapter {
    inner: Arc<Inner>,
}

impl WebRtcDataChannelAdapter {
    pub fn new(channel: Arc<dyn DataChannel>) -> Self {
        let adapter = Self {
            inner: Arc::new(Inner {
                channel,
                is_open: AtomicBool::new(false),
                subscribers: Mutex::new(Vec::new()),
                next_sub_id: AtomicU64::new(0),
            }),
        };
        adapter.spawn_poll_loop();
        adapter
    }

    pub fn is_open(&self) -> bool {
        self.inner.is_open.load(Ordering::Acquire)
    }

    fn spawn_poll_loop(&self) {
        let inner = Arc::clone(&self.inner);
        tokio::spawn(async move {
            while let Some(event) = inner.channel.poll().await {
                match event {
                    DataChannelEvent::OnOpen => {
                        inner.is_open.store(true, Ordering::Release);
                    }
                    DataChannelEvent::OnMessage(message) => {
                        let frame = serde_json::from_slice::<Frame>(&message.data);
                        match frame {
                            Ok(frame) => dispatch_frame(&inner, frame).await,
                            Err(error) => {
                                dispatch_error(&inner, WebRtcDataChannelError::Json(error)).await;
                            }
                        }
                    }
                    DataChannelEvent::OnClose => {
                        inner.is_open.store(false, Ordering::Release);
                        return;
                    }
                    DataChannelEvent::OnError => {
                        dispatch_error(
                            &inner,
                            WebRtcDataChannelError::WebRtc("datachannel error event".to_string()),
                        )
                        .await;
                    }
                    DataChannelEvent::OnClosing
                    | DataChannelEvent::OnBufferedAmountLow
                    | DataChannelEvent::OnBufferedAmountHigh => {}
                }
            }
            inner.is_open.store(false, Ordering::Release);
        });
    }
}

async fn dispatch_frame(inner: &Arc<Inner>, frame: Frame) {
    let subscribers = {
        let subscribers = inner.subscribers.lock().await;
        subscribers
            .iter()
            .filter(|subscriber| subscriber.subscription.matches(&frame))
            .map(|subscriber| subscriber.tx.clone())
            .collect::<Vec<_>>()
    };

    for tx in subscribers {
        let _ = tx.try_send(Ok(frame.clone()));
    }
}

async fn dispatch_error(inner: &Arc<Inner>, error: WebRtcDataChannelError) {
    let subscribers = {
        let subscribers = inner.subscribers.lock().await;
        subscribers
            .iter()
            .map(|subscriber| subscriber.tx.clone())
            .collect::<Vec<_>>()
    };

    for tx in subscribers {
        let _ = tx.try_send(Err(match &error {
            WebRtcDataChannelError::Json(json) => {
                WebRtcDataChannelError::Json(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    json.to_string(),
                )))
            }
            WebRtcDataChannelError::WebRtc(message) => {
                WebRtcDataChannelError::WebRtc(message.clone())
            }
            WebRtcDataChannelError::NotOpen => WebRtcDataChannelError::NotOpen,
            WebRtcDataChannelError::FrameTooLarge { actual, limit } => {
                WebRtcDataChannelError::FrameTooLarge {
                    actual: *actual,
                    limit: *limit,
                }
            }
            WebRtcDataChannelError::UnsupportedDurableKind(kind) => {
                WebRtcDataChannelError::UnsupportedDurableKind(*kind)
            }
        }));
    }
}

#[async_trait]
impl Transport for WebRtcDataChannelAdapter {
    type Error = WebRtcDataChannelError;

    async fn send(&self, frame: Frame) -> Result<(), Self::Error> {
        if !matches!(frame.kind, FrameKind::Event) {
            return Err(WebRtcDataChannelError::UnsupportedDurableKind(frame.kind));
        }
        if !self.is_open() {
            return Err(WebRtcDataChannelError::NotOpen);
        }
        let payload = serde_json::to_string(&frame)?;
        if payload.len() > MAX_DATACHANNEL_FRAME_BYTES {
            return Err(WebRtcDataChannelError::FrameTooLarge {
                actual: payload.len(),
                limit: MAX_DATACHANNEL_FRAME_BYTES,
            });
        }
        self.inner
            .channel
            .send_text(&payload)
            .await
            .map_err(|error| WebRtcDataChannelError::WebRtc(error.to_string()))?;
        Ok(())
    }

    async fn subscribe(
        &self,
        subscription: Subscription,
    ) -> Result<FrameStream<Self::Error>, Self::Error> {
        let (tx, rx) = mpsc::channel(SUBSCRIBER_CHANNEL_DEPTH);
        let id = self.inner.next_sub_id.fetch_add(1, Ordering::Relaxed);
        self.inner.subscribers.lock().await.push(SubscriberHandle {
            id,
            subscription,
            tx,
        });
        let stream = futures::stream::unfold(rx, |mut rx| async move {
            rx.recv().await.map(|item| (item, rx))
        });
        Ok(Box::pin(stream) as Pin<Box<dyn Stream<Item = _> + Send>>)
    }
}
