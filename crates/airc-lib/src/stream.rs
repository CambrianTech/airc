use std::pin::Pin;
use std::task::{Context, Poll};

use airc_core::TranscriptEvent;
use futures::stream::Stream;
use tokio_stream::wrappers::{errors::BroadcastStreamRecvError, BroadcastStream};

/// Live transcript-event stream returned by `Airc::subscribe`.
pub struct EventStream {
    pub(crate) inner: BroadcastStream<TranscriptEvent>,
}

impl Stream for EventStream {
    type Item = Result<TranscriptEvent, LiveLag>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        let inner = Pin::new(&mut this.inner);
        match inner.poll_next(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Ok(event))) => Poll::Ready(Some(Ok(event))),
            Poll::Ready(Some(Err(BroadcastStreamRecvError::Lagged(n)))) => {
                Poll::Ready(Some(Err(LiveLag { skipped: n })))
            }
            Poll::Ready(None) => Poll::Ready(None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveLag {
    pub skipped: u64,
}

impl std::fmt::Display for LiveLag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "live stream lagged {} events; resume from cursor",
            self.skipped
        )
    }
}

impl std::error::Error for LiveLag {}
