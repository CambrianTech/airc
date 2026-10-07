//! Bounded, allocation-identity reuse of immutable attach frames.
//!
//! The budgets cover CBOR buffer capacity and entry
//! bookkeeping, not the pre-existing Planus encoder's input-dependent workspace.
//! Slow writers retain their charges after eviction; exhausted budgets apply
//! asynchronous backpressure rather than an unaccounted encoding fallback.
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex, MutexGuard, Weak};

use airc_bus::Envelope;
use airc_ipc::codec::{encode_event_frame_payload, MAX_FRAME_BYTES};
#[cfg(test)]
use airc_ipc::Response;
use tokio::sync::{Notify, OnceCell, OwnedSemaphorePermit, Semaphore, TryAcquireError};

// Keep a conservative two-frame reservation before encoding; exact CBOR is
// allocated once, then charged at retained capacity. The prefix stays on stack.
const ENCODING_RESERVATION: usize = 2 * MAX_FRAME_BYTES as usize;
const FRAME_BUDGET: usize = 2 * ENCODING_RESERVATION;
const ENTRY_BUDGET: usize = 128;

struct Identity {
    envelope: Weak<Envelope>,
    _permit: OwnedSemaphorePermit,
}

struct Entry {
    identity: Arc<Identity>,
    frame: OnceCell<Arc<SharedFrame>>,
}

pub(crate) struct SharedFrame {
    pub(crate) payload: Vec<u8>,
    // Both charges survive eviction and remain until the final socket writer
    // releases the frame. There is no cycle back to Entry or the cache.
    _identity: Arc<Identity>,
    _bytes: OwnedSemaphorePermit,
}

pub(crate) struct SharedFrames {
    entries: Mutex<VecDeque<Arc<Entry>>>,
    entry_budget: Arc<Semaphore>,
    byte_budget: Arc<Semaphore>,
    changed: Notify,
    #[cfg(test)]
    pub(crate) admission_waiting: Notify,
}

impl Default for SharedFrames {
    fn default() -> Self {
        Self::new(ENTRY_BUDGET, FRAME_BUDGET)
    }
}

impl SharedFrames {
    pub(crate) fn new(entries: usize, bytes: usize) -> Self {
        assert!(entries > 0 && bytes >= ENCODING_RESERVATION);
        Self {
            entries: Mutex::new(VecDeque::new()),
            entry_budget: Arc::new(Semaphore::new(entries)),
            byte_budget: Arc::new(Semaphore::new(bytes)),
            changed: Notify::new(),
            #[cfg(test)]
            admission_waiting: Notify::new(),
        }
    }

    fn metadata(&self) -> io::Result<MutexGuard<'_, VecDeque<Arc<Entry>>>> {
        self.entries
            .lock()
            .map_err(|_| io::Error::other("shared frame cache metadata poisoned"))
    }

    fn closed_budget(name: &str) -> io::Error {
        io::Error::new(
            io::ErrorKind::BrokenPipe,
            format!("shared frame {name} budget closed"),
        )
    }

    fn lookup(&self, envelope: &Arc<Envelope>) -> io::Result<Option<Arc<Entry>>> {
        let identity = Arc::downgrade(envelope);
        Ok(self
            .metadata()?
            .iter()
            .find(|entry| Weak::ptr_eq(&entry.identity.envelope, &identity))
            .cloned())
    }

    // Only metadata is locked. Dropping a cache reference cannot release a
    // frame's permits while a socket writer still holds that frame.
    fn evict(&self, protected: Option<&Arc<Entry>>) -> io::Result<bool> {
        let mut entries = self.metadata()?;
        let index = entries
            .iter()
            .position(|entry| protected.is_none_or(|protected| !Arc::ptr_eq(entry, protected)));
        Ok(index.and_then(|index| entries.remove(index)).is_some())
    }

    async fn entry(&self, envelope: &Arc<Envelope>) -> io::Result<Arc<Entry>> {
        if let Some(entry) = self.lookup(envelope)? {
            return Ok(entry);
        }
        let permit = loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if let Some(entry) = self.lookup(envelope)? {
                return Ok(entry);
            }
            match self.entry_budget.clone().try_acquire_owned() {
                Ok(permit) => break permit,
                Err(TryAcquireError::Closed) => return Err(Self::closed_budget("entry")),
                Err(TryAcquireError::NoPermits) => {}
            }
            if !self.evict(None)? {
                #[cfg(test)]
                self.admission_waiting.notify_waiters();
                tokio::select! {
                    permit = self.entry_budget.clone().acquire_owned() => break permit.map_err(|_| Self::closed_budget("entry"))?,
                    _ = changed => {},
                }
            }
        };
        self.insert(envelope, permit)
    }

    fn insert(
        &self,
        envelope: &Arc<Envelope>,
        permit: OwnedSemaphorePermit,
    ) -> io::Result<Arc<Entry>> {
        let mut entries = self.metadata()?;
        // A different task may have installed this identity while we waited.
        let identity = Arc::downgrade(envelope);
        if let Some(entry) = entries
            .iter()
            .find(|entry| Weak::ptr_eq(&entry.identity.envelope, &identity))
        {
            return Ok(entry.clone());
        }
        let entry = Arc::new(Entry {
            identity: Arc::new(Identity {
                envelope: identity,
                _permit: permit,
            }),
            frame: OnceCell::new(),
        });
        entries.push_back(entry.clone());
        self.changed.notify_waiters();
        Ok(entry)
    }

    async fn reserve_encoding(&self, entry: &Arc<Entry>) -> io::Result<OwnedSemaphorePermit> {
        loop {
            let changed = self.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            match self
                .byte_budget
                .clone()
                .try_acquire_many_owned(ENCODING_RESERVATION as u32)
            {
                Ok(permit) => return Ok(permit),
                Err(TryAcquireError::Closed) => return Err(Self::closed_budget("byte")),
                Err(TryAcquireError::NoPermits) => {}
            }
            if !self.evict(Some(entry))? {
                #[cfg(test)]
                self.admission_waiting.notify_waiters();
                tokio::select! {
                    permit = self.byte_budget.clone().acquire_many_owned(ENCODING_RESERVATION as u32) => return permit.map_err(|_| Self::closed_budget("byte")),
                    _ = changed => {},
                }
            }
        }
    }

    pub(crate) async fn get(&self, envelope: &Arc<Envelope>) -> io::Result<Arc<SharedFrame>> {
        let entry = self.entry(envelope).await?;
        let result = entry
            .frame
            .get_or_try_init(|| async {
                // Reserve BEFORE either encoding allocation. No cache lock spans
                // this wait, serialization, or the caller's socket writes.
                let mut reservation = self.reserve_encoding(&entry).await?;
                let wire = airc_wire::encode(envelope);
                let payload = encode_event_frame_payload(&wire)?;
                drop(wire);
                let retained = payload.capacity();
                assert!(retained <= MAX_FRAME_BYTES as usize);
                drop(reservation.split(ENCODING_RESERVATION - retained));
                Ok(Arc::new(SharedFrame {
                    payload,
                    _identity: entry.identity.clone(),
                    _bytes: reservation,
                }))
            })
            .await
            .cloned();
        // A permit may now be retained by a newly initialized cache entry,
        // rather than released to semaphore waiters. Wake them to reconsider
        // eviction; register-before-inspect above prevents a lost change.
        self.changed.notify_waiters();
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_bus::{DeliveryClass, Kind};
    use airc_core::{ClientId, PeerId, RoomId};
    use bytes::Bytes;
    use futures::FutureExt;

    fn envelope(payload: impl Into<Bytes>) -> Arc<Envelope> {
        Arc::new(Envelope::new(
            RoomId::new(),
            (PeerId::new(), ClientId::new()),
            Kind::StreamChunk,
            DeliveryClass::StreamChunk,
            payload.into(),
        ))
    }

    #[tokio::test]
    async fn poisoned_metadata_returns_error_and_releases_uninserted_permit() {
        let cache = Arc::new(SharedFrames::new(1, ENCODING_RESERVATION));
        let poison = cache.clone();
        assert!(std::thread::spawn(move || {
            let _guard = poison.entries.lock().unwrap();
            panic!("controlled cache metadata poison");
        })
        .join()
        .is_err());
        let event = envelope(Bytes::from_static(b"poison"));
        let error = cache.get(&event).await.err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert!(error.to_string().contains("metadata poisoned"));
        let permit = cache.entry_budget.clone().acquire_owned().await.unwrap();
        assert!(cache.insert(&event, permit).is_err());
        assert_eq!(cache.entry_budget.available_permits(), 1);
        assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
        assert!(cache.evict(None).is_err());
    }

    #[tokio::test]
    async fn closed_budgets_fail_immediate_and_waiting_admission_without_leaks() {
        for byte_budget in [false, true] {
            for close_while_waiting in [false, true] {
                let cache = SharedFrames::new(1, ENCODING_RESERVATION);
                let budget = if byte_budget {
                    cache.byte_budget.clone()
                } else {
                    cache.entry_budget.clone()
                };
                let permits = budget.available_permits();
                let held = if close_while_waiting {
                    Some(
                        budget
                            .clone()
                            .acquire_many_owned(permits as u32)
                            .await
                            .unwrap(),
                    )
                } else {
                    None
                };
                let event = envelope(Bytes::from_static(b"closed"));
                let mut pending = Box::pin(cache.get(&event));
                if close_while_waiting {
                    assert!(pending.as_mut().now_or_never().is_none());
                }
                budget.close();
                let error = tokio::time::timeout(std::time::Duration::from_secs(1), pending)
                    .await
                    .unwrap()
                    .err()
                    .unwrap();
                assert_eq!(error.kind(), io::ErrorKind::BrokenPipe);
                assert!(error.to_string().contains(if byte_budget {
                    "byte budget closed"
                } else {
                    "entry budget closed"
                }));
                drop(held);
                while cache.evict(None).unwrap() {}
                assert_eq!(cache.entry_budget.available_permits(), 1);
                assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
            }
        }
    }

    #[tokio::test]
    async fn concurrent_identity_reuse_preserves_exact_wire_and_distinguishes_same_id() {
        let cache = Arc::new(SharedFrames::default());
        let original = envelope(Bytes::from_static(b"first"));
        let reservation = cache
            .byte_budget
            .clone()
            .acquire_many_owned(FRAME_BUDGET as u32)
            .await
            .unwrap();
        let mut pending = Box::pin(futures::future::join_all(
            (0..8).map(|_| cache.get(&original)),
        ));
        assert!(pending.as_mut().now_or_never().is_none());
        drop(reservation);
        let frames: Vec<_> = pending.await.into_iter().map(Result::unwrap).collect();
        let first = &frames[0];
        for frame in &frames {
            assert!(Arc::ptr_eq(first, frame));
        }
        let mut distinct = (*original).clone();
        distinct.payload = Bytes::from_static(b"different but same event id");
        let distinct = Arc::new(distinct);
        let second = cache.get(&distinct).await.unwrap();
        assert!(!Arc::ptr_eq(first, &second));
        assert_ne!(first.payload, second.payload);
        let mut expected = Vec::new();
        airc_ipc::codec::write_frame(
            &mut expected,
            &Response::event_ref(&airc_wire::encode(&original)),
        )
        .await
        .unwrap();
        let mut actual = Vec::new();
        airc_ipc::codec::write_encoded_frame(&mut actual, &first.payload)
            .await
            .unwrap();
        assert_eq!(actual, expected);
    }

    #[tokio::test]
    async fn eviction_keeps_writer_entry_and_byte_charges_and_cancellation_releases_waiter() {
        let cache = SharedFrames::new(1, ENCODING_RESERVATION);
        let first = envelope(Bytes::from_static(b"held by socket writer"));
        let second = envelope(Bytes::from_static(b"next"));
        let held = cache.get(&first).await.unwrap();
        let charged = held.payload.capacity();
        assert!(cache.evict(None).unwrap());
        assert_eq!(cache.entry_budget.available_permits(), 0);
        assert_eq!(
            cache.byte_budget.available_permits(),
            ENCODING_RESERVATION - charged
        );
        let mut waiting = Box::pin(cache.get(&second));
        assert!(waiting.as_mut().now_or_never().is_none());
        drop(waiting);
        drop(held);
        assert_eq!(cache.entry_budget.available_permits(), 1);
        assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
        cache.get(&second).await.unwrap();
    }

    #[tokio::test]
    async fn cancelled_initializer_can_retry_without_leaking_reservation() {
        let cache = SharedFrames::new(2, ENCODING_RESERVATION);
        let first = envelope(Bytes::from_static(b"held"));
        let second = envelope(Bytes::from_static(b"waiting"));
        let held = cache.get(&first).await.unwrap();
        let mut waiting = Box::pin(cache.get(&second));
        assert!(waiting.as_mut().now_or_never().is_none());
        drop(waiting);
        drop(held);
        assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
        let next = cache.get(&second).await.unwrap();
        assert!(!next.payload.is_empty());
    }

    #[tokio::test]
    async fn admission_rechecks_entry_published_after_it_started_waiting() {
        let cache = SharedFrames::new(1, ENCODING_RESERVATION);
        let first = envelope(Bytes::from_static(b"entry not yet published"));
        let second = envelope(Bytes::from_static(b"waiting"));
        // Reproduce preemption between production's permit acquisition and
        // insertion. No cache entry exists for the waiter to evict yet.
        let permit = cache.entry_budget.clone().acquire_owned().await.unwrap();
        let mut waiting = Box::pin(cache.get(&second));
        assert!(waiting.as_mut().now_or_never().is_none());
        drop(cache.insert(&first, permit).unwrap());
        tokio::time::timeout(std::time::Duration::from_secs(1), waiting)
            .await
            .expect("insertion must wake admission to evict")
            .unwrap();
    }

    #[tokio::test]
    async fn byte_admission_rechecks_a_newly_cached_charge() {
        let cache = SharedFrames::new(2, ENCODING_RESERVATION);
        let first = envelope(Bytes::from_static(b"publishing"));
        let second = envelope(Bytes::from_static(b"waiting"));
        let entry_permit = cache.entry_budget.clone().acquire_owned().await.unwrap();
        let mut bytes = cache
            .byte_budget
            .clone()
            .acquire_many_owned(ENCODING_RESERVATION as u32)
            .await
            .unwrap();
        let mut pending = Box::pin(cache.get(&second));
        assert!(pending.as_mut().now_or_never().is_none());
        let payload = vec![0];
        drop(bytes.split(ENCODING_RESERVATION - payload.capacity()));
        let entry = cache.insert(&first, entry_permit).unwrap();
        assert!(entry
            .frame
            .set(Arc::new(SharedFrame {
                payload,
                _identity: entry.identity.clone(),
                _bytes: bytes
            }))
            .is_ok());
        drop(entry);
        tokio::time::timeout(std::time::Duration::from_secs(1), pending)
            .await
            .expect("new cache ownership must wake byte admission")
            .unwrap();
    }

    #[tokio::test]
    async fn cancelled_saturated_socket_write_releases_evicted_frame() {
        let cache = SharedFrames::new(1, ENCODING_RESERVATION);
        let event = envelope(Bytes::from_static(b"larger than duplex capacity"));
        let frame = cache.get(&event).await.unwrap();
        let (mut writer, _reader) = tokio::io::duplex(1);
        let mut writing = Box::pin(async move {
            airc_ipc::codec::write_encoded_frame(&mut writer, &frame.payload).await
        });
        assert!(writing.as_mut().now_or_never().is_none());
        assert!(cache.evict(None).unwrap());
        assert_eq!(cache.entry_budget.available_permits(), 0);
        drop(writing);
        assert_eq!(cache.entry_budget.available_permits(), 1);
        assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
    }

    #[tokio::test]
    async fn serialization_and_socket_errors_release_charges() {
        let cache = SharedFrames::new(2, ENCODING_RESERVATION);
        let oversized = envelope(Bytes::from(vec![255; MAX_FRAME_BYTES as usize / 2 + 1024]));
        let error = cache
            .get(&oversized)
            .await
            .err()
            .expect("CBOR sequence exceeds frame maximum");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("ipc frame too large"));
        assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
        let small = envelope(Bytes::from_static(b"small"));
        let frame = cache.get(&small).await.unwrap();
        let (mut writer, reader) = tokio::io::duplex(1);
        drop(reader);
        assert!(
            airc_ipc::codec::write_encoded_frame(&mut writer, &frame.payload)
                .await
                .is_err()
        );
        while cache.evict(None).unwrap() {}
        drop(frame);
        assert_eq!(cache.entry_budget.available_permits(), 2);
        assert_eq!(cache.byte_budget.available_permits(), ENCODING_RESERVATION);
    }
}
