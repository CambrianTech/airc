//! Subscriber fan-out — receive-side dispatch from connection read
//! loops to attached `subscribe()` callers.
//!
//! Per Codex's #671 review finding: do NOT hold the subscribers mutex
//! across `.send().await`. We snapshot matching senders under the
//! lock, drop the lock, then await dispatches outside. Slow
//! consumers no longer block other subscribers or new registrations.

use std::sync::Arc;

use tokio::sync::mpsc;

use airc_protocol::{Frame, FrameKind, Subscription};

use crate::lan_tcp::adapter::error::LanTcpError;
use crate::lan_tcp::adapter::inner::Inner;

pub(super) async fn dispatch_to_subscribers(inner: &Arc<Inner>, frame: Frame) {
    // 1. Snapshot under lock — drop the lock as soon as we have the
    //    sender handles we need.
    type Target = (u64, mpsc::Sender<Result<Frame, LanTcpError>>);
    let targets: Vec<Target> = {
        let subs = inner.subscribers.lock().await;
        subs.iter()
            .filter(|sub| subscription_matches_with_cursor(&sub.subscription, &frame))
            .map(|sub| (sub.id, sub.tx.clone()))
            .collect()
    };

    // 2. Dispatch outside the lock — slow consumers no longer block
    //    other subscribers or new registrations.
    let mut dead_ids: Vec<u64> = Vec::new();
    for (id, tx) in targets {
        let send_result = match frame.kind {
            FrameKind::Message | FrameKind::Control => {
                // Backpressure-bearing — block on slow consumer.
                tx.send(Ok(frame.clone())).await.map_err(|_| ())
            }
            FrameKind::Event => {
                // Lossy — drop on full subscriber buffer.
                match tx.try_send(Ok(frame.clone())) {
                    Ok(()) => Ok(()),
                    Err(mpsc::error::TrySendError::Full(_)) => Ok(()),
                    Err(mpsc::error::TrySendError::Closed(_)) => Err(()),
                }
            }
        };
        if send_result.is_err() {
            dead_ids.push(id);
        }
    }

    // 3. Reap dead subscribers under a brief lock.
    if !dead_ids.is_empty() {
        let mut subs = inner.subscribers.lock().await;
        subs.retain(|sub| !dead_ids.contains(&sub.id));
    }
}

/// Cursor predicate (mirrors local-fs's check) — frames at or before
/// the cursor are skipped for replay subscribers.
fn subscription_matches_with_cursor(sub: &Subscription, frame: &Frame) -> bool {
    if !sub.matches(frame) {
        return false;
    }
    if let Some(cursor) = &sub.from_cursor {
        let envelope = &frame.envelope;
        let after = envelope.lamport > cursor.lamport
            || (envelope.lamport == cursor.lamport
                && envelope.event_id.as_uuid() > cursor.event_id.as_uuid());
        if !after {
            return false;
        }
    }
    true
}
