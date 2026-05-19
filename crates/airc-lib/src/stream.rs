use std::collections::BTreeSet;
use std::pin::Pin;
use std::task::{Context, Poll};

use airc_core::transcript::TranscriptKind;
use airc_core::{HeaderFilter, RoomId, TranscriptEvent};
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

/// Consumer-facing filter over persisted transcript events.
///
/// This intentionally mirrors the wire-level subscription shape but
/// operates on `TranscriptEvent`, which is what hooks, monitors, and
/// Continuum consume after signatures have been verified and frames
/// have crossed into the durable store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventFilter {
    pub channel: Option<RoomId>,
    pub kinds: BTreeSet<TranscriptKind>,
    pub headers_filter: HeaderFilter,
}

impl Default for EventFilter {
    fn default() -> Self {
        Self {
            channel: None,
            kinds: BTreeSet::new(),
            headers_filter: HeaderFilter::Any,
        }
    }
}

impl EventFilter {
    pub fn current_room() -> Self {
        Self::default()
    }

    pub fn matches(&self, event: &TranscriptEvent) -> bool {
        if let Some(channel) = self.channel {
            if event.room_id != channel {
                return false;
            }
        }
        if !self.kinds.is_empty() && !self.kinds.contains(&event.kind) {
            return false;
        }
        self.headers_filter.matches(&event.headers)
    }
}

pub struct FilteredEventStream {
    pub(crate) inner: EventStream,
    pub(crate) filter: EventFilter,
}

impl Stream for FilteredEventStream {
    type Item = Result<TranscriptEvent, LiveLag>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        loop {
            match Pin::new(&mut this.inner).poll_next(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Some(Ok(event))) => {
                    if this.filter.matches(&event) {
                        return Poll::Ready(Some(Ok(event)));
                    }
                }
                Poll::Ready(Some(Err(lag))) => return Poll::Ready(Some(Err(lag))),
                Poll::Ready(None) => return Poll::Ready(None),
            }
        }
    }
}
