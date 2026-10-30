//! Bounded, generation-fenced inboxes for opaque addressed frames.
//! Registration is internal: the connection owner must authenticate first.

use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex};

use airc_core::PeerId;
use airc_protocol::Frame;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};
use uuid::Uuid;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RouteError {
    ReceiverLimit,
    ReceiverBusy,
    NoReceiver,
    Backpressure,
    Closed,
    StatePoisoned,
    DeadlineElapsed,
}

struct Entry {
    generation: Uuid,
    tx: mpsc::Sender<QueuedFrame>,
    _alive: watch::Sender<()>,
}

pub(crate) struct QueuedFrame {
    pub frame: Frame,
    pub deadline: tokio::time::Instant,
    // Held through the socket write, so draining the queue cannot evade admission.
    pub(crate) _permit: OwnedSemaphorePermit,
}

/// Only an absent owner permits transport-host resolution. Queue failures do not.
pub(crate) enum LocalAdmission {
    Delivered,
    Unowned(Box<Frame>),
}

pub struct AddressedSwitch {
    entries: Mutex<HashMap<PeerId, Entry>>,
    frames: Arc<Semaphore>,
    receiver_limit: usize,
    queue_capacity: usize,
}

impl AddressedSwitch {
    /// Limits are node-wide frame admission and receiver count, plus per-peer
    /// queue admission. The IPC codec independently bounds each frame's bytes.
    pub fn new(
        receiver_limit: NonZeroUsize,
        frame_limit: NonZeroUsize,
        queue_capacity: NonZeroUsize,
    ) -> Arc<Self> {
        Arc::new(Self {
            entries: Mutex::new(HashMap::new()),
            frames: Arc::new(Semaphore::new(frame_limit.get())),
            receiver_limit: receiver_limit.get(),
            queue_capacity: queue_capacity.get(),
        })
    }

    pub(crate) fn register(self: &Arc<Self>, peer: PeerId) -> Result<ReceiverLease, RouteError> {
        let mut entries = self.entries.lock().map_err(|_| RouteError::StatePoisoned)?;
        if entries.contains_key(&peer) {
            return Err(RouteError::ReceiverBusy);
        }
        if entries.len() >= self.receiver_limit {
            return Err(RouteError::ReceiverLimit);
        }
        let generation = Uuid::new_v4();
        let (tx, rx) = mpsc::channel(self.queue_capacity);
        let (alive, superseded) = watch::channel(());
        entries.insert(
            peer,
            Entry {
                generation,
                tx,
                _alive: alive,
            },
        );
        Ok(ReceiverLease {
            owner: self.clone(),
            peer,
            generation,
            rx,
            superseded,
        })
    }

    /// The caller admits only authenticated addressed envelopes, never room data.
    pub(crate) fn route(
        &self,
        peer: PeerId,
        frame: Frame,
        deadline: tokio::time::Instant,
    ) -> Result<(), RouteError> {
        match self.route_if_owned(peer, frame, deadline)? {
            LocalAdmission::Delivered => Ok(()),
            LocalAdmission::Unowned(frame) => {
                drop(frame);
                Err(RouteError::NoReceiver)
            }
        }
    }

    /// Select and enqueue under one lock. Return the intact frame only when no
    /// authenticated local owner exists; never turn queue pressure into rerouting.
    pub(crate) fn route_if_owned(
        &self,
        peer: PeerId,
        frame: Frame,
        deadline: tokio::time::Instant,
    ) -> Result<LocalAdmission, RouteError> {
        if tokio::time::Instant::now() >= deadline {
            return Err(RouteError::DeadlineElapsed);
        }
        let entries = self.entries.lock().map_err(|_| RouteError::StatePoisoned)?;
        let Some(entry) = entries.get(&peer) else {
            return Ok(LocalAdmission::Unowned(Box::new(frame)));
        };
        let permit = self.reserve_frame()?;
        entry
            .tx
            .try_send(QueuedFrame {
                frame,
                deadline,
                _permit: permit,
            })
            .map(|()| LocalAdmission::Delivered)
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RouteError::Backpressure,
                mpsc::error::TrySendError::Closed(_) => RouteError::Closed,
            })
    }

    /// Local queues and outbound writes share the same node admission budget.
    pub(crate) fn reserve_frame(&self) -> Result<OwnedSemaphorePermit, RouteError> {
        self.frames
            .clone()
            .try_acquire_owned()
            .map_err(|_| RouteError::Backpressure)
    }
}

pub(crate) struct ReceiverLease {
    owner: Arc<AddressedSwitch>,
    peer: PeerId,
    generation: Uuid,
    pub rx: mpsc::Receiver<QueuedFrame>,
    pub superseded: watch::Receiver<()>,
}

impl ReceiverLease {
    #[cfg(test)]
    pub fn is_current(&self) -> Result<bool, RouteError> {
        let entries = self
            .owner
            .entries
            .lock()
            .map_err(|_| RouteError::StatePoisoned)?;
        Ok(entries
            .get(&self.peer)
            .is_some_and(|entry| entry.generation == self.generation))
    }
}

impl Drop for ReceiverLease {
    fn drop(&mut self) {
        // A poisoned switch already refuses admission; drop must not panic.
        if let Ok(mut entries) = self.owner.entries.lock() {
            if entries
                .get(&self.peer)
                .is_some_and(|entry| entry.generation == self.generation)
            {
                entries.remove(&self.peer);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(peer: PeerId) -> Frame {
        Frame {
            kind: airc_protocol::FrameKind::Addressed,
            envelope: airc_protocol::Envelope {
                event_id: airc_core::EventId::new(),
                sender: PeerId::new(),
                sender_client: airc_core::ClientId::new(),
                channel: airc_protocol::addressed::addressed_channel(),
                target: airc_core::MentionTarget::Peer(peer),
                lamport: 1,
                occurred_at_ms: 1,
                reply_to: None,
                headers: Default::default(),
                body: None,
                media: Vec::new(),
                signature: airc_protocol::Signature::Unsigned,
            },
        }
    }

    #[tokio::test]
    async fn only_absent_owner_returns_frame_and_budget_covers_inflight_write() {
        let switch = AddressedSwitch::new(NonZeroUsize::MIN, NonZeroUsize::MIN, NonZeroUsize::MIN);
        let peer = PeerId::new();
        let deadline = tokio::time::Instant::now() + airc_ipc::client::DEFAULT_RPC_TIMEOUT;
        let original = frame(peer);
        let LocalAdmission::Unowned(returned) = switch
            .route_if_owned(peer, original.clone(), deadline)
            .unwrap()
        else {
            panic!("absent owner consumed the frame");
        };
        assert_eq!(*returned, original);
        let mut owner = switch.register(peer).unwrap();
        let outbound = switch.reserve_frame().unwrap();
        assert!(matches!(
            switch.route_if_owned(peer, frame(peer), deadline),
            Err(RouteError::Backpressure)
        ));
        drop(outbound);
        assert!(matches!(
            switch.route_if_owned(peer, frame(peer), deadline),
            Ok(LocalAdmission::Delivered)
        ));
        let writing = owner.rx.recv().await.unwrap();
        assert!(matches!(
            switch.reserve_frame(),
            Err(RouteError::Backpressure)
        ));
        assert!(matches!(
            switch.route_if_owned(peer, frame(peer), deadline),
            Err(RouteError::Backpressure)
        ));
        drop(writing);
        assert!(switch.reserve_frame().is_ok());
        owner.rx.close();
        assert!(matches!(
            switch.route_if_owned(peer, frame(peer), deadline),
            Err(RouteError::Closed)
        ));
    }

    #[test]
    fn live_receiver_cannot_be_replaced_and_receiver_limit_is_hard() {
        let one = NonZeroUsize::new(1).unwrap();
        let switch = AddressedSwitch::new(one, one, one);
        let peer = PeerId::new();
        let old = switch.register(peer).unwrap();
        assert!(matches!(
            switch.register(peer),
            Err(RouteError::ReceiverBusy)
        ));
        assert!(old.is_current().unwrap());
        assert!(matches!(
            switch.register(PeerId::new()),
            Err(RouteError::ReceiverLimit)
        ));
        drop(old);
        assert!(switch.register(PeerId::new()).is_ok());
    }
}
