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
    NoReceiver,
    Backpressure,
    Closed,
    StatePoisoned,
}

struct Entry {
    generation: Uuid,
    tx: mpsc::Sender<QueuedFrame>,
    _alive: watch::Sender<()>,
}

pub(crate) struct QueuedFrame {
    pub frame: Frame,
    // Held through the socket write, so draining the queue cannot evade admission.
    pub(crate) _permit: OwnedSemaphorePermit,
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
        if !entries.contains_key(&peer) && entries.len() >= self.receiver_limit {
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
    pub(crate) fn route(&self, peer: PeerId, frame: Frame) -> Result<(), RouteError> {
        let entries = self.entries.lock().map_err(|_| RouteError::StatePoisoned)?;
        let entry = entries.get(&peer).ok_or(RouteError::NoReceiver)?;
        let permit = self
            .frames
            .clone()
            .try_acquire_owned()
            .map_err(|_| RouteError::Backpressure)?;
        entry
            .tx
            .try_send(QueuedFrame {
                frame,
                _permit: permit,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => RouteError::Backpressure,
                mpsc::error::TrySendError::Closed(_) => RouteError::Closed,
            })
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

    #[test]
    fn old_connection_cannot_unregister_replacement_and_receiver_limit_is_hard() {
        let one = NonZeroUsize::new(1).unwrap();
        let switch = AddressedSwitch::new(one, one, one);
        let peer = PeerId::new();
        let old = switch.register(peer).unwrap();
        let replacement = switch.register(peer).unwrap();
        assert!(!old.is_current().unwrap());
        drop(old);
        assert!(replacement.is_current().unwrap());
        assert!(matches!(
            switch.register(PeerId::new()),
            Err(RouteError::ReceiverLimit)
        ));
        drop(replacement);
        assert!(switch.register(PeerId::new()).is_ok());
    }
}
