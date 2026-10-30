//! Addressed delivery: a command or its result between two identities,
//! readable by those two only and stored by no one.
//!
//! The wire form is `airc_protocol::addressed` (one `FrameKind::Addressed`
//! frame per handshake step or sealed payload, under a `SessionManager`
//! session). This module owns the per-identity state and the two ends:
//! [`Airc::send_addressed`](crate::Airc::send_addressed) seals and sends, and
//! the inbound hook opens what is addressed to this identity and hands it to
//! [`AddressedInbox`]. No addressed frame is ever appended to a store or the
//! live ring, on the sender, a third party, or the addressee: the inbox is the
//! only place an opened payload goes.
//!
//! `from`, `kind` and `correlation` in a [`VerifiedAddressed`] are
//! authenticated by the session (the handshake pins `from` to its enrolled
//! identity key; the AEAD covers the rest), never read from a header.

mod daemon;
mod receive;
mod send;
mod sink;
pub use sink::AddressedFrameSink;

use std::sync::Arc;

use tokio::sync::{broadcast, watch};
use uuid::Uuid;

use airc_core::{Body, Headers, PeerId};
use airc_identity::LocalIdentity;
use airc_protocol::{PeerKeyRegistry, SessionManager, SessionManagerError};

pub use airc_protocol::addressed::AddressedKind;

/// One addressed payload to send.
#[derive(Debug, Clone)]
pub struct AddressedSend {
    pub to: PeerId,
    pub kind: AddressedKind,
    pub correlation: Uuid,
    pub headers: Headers,
    pub body: Body,
}

/// One addressed payload received and opened. Only the receive path builds
/// one, so holding it means the session authenticated every field.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedAddressed {
    from: PeerId,
    to: PeerId,
    kind: AddressedKind,
    correlation: Uuid,
    headers: Headers,
    body: Body,
}

impl VerifiedAddressed {
    fn opened(from: PeerId, to: PeerId, inner: airc_protocol::addressed::SealedInner) -> Self {
        Self {
            from,
            to,
            kind: inner.kind,
            correlation: inner.correlation,
            headers: inner.headers,
            body: inner.body,
        }
    }

    /// The session's handshake-pinned identity, never a header.
    pub fn from(&self) -> PeerId {
        self.from
    }

    /// This scope's identity.
    pub fn to(&self) -> PeerId {
        self.to
    }

    pub fn kind(&self) -> AddressedKind {
        self.kind
    }

    pub fn correlation(&self) -> Uuid {
        self.correlation
    }

    pub fn headers(&self) -> &Headers {
        &self.headers
    }

    pub fn body(&self) -> &Body {
        &self.body
    }
}

/// Why an addressed send did not go out.
#[derive(Debug)]
pub enum AddressedError {
    /// No transport route to the peer, or the wire write failed.
    Transport(String),
    /// Opening a session failed (unknown peer, bad signature, ...).
    Handshake(SessionManagerError),
    /// The payload did not encode.
    Codec(String),
    /// No session formed before the caller's deadline.
    DeadlineExceeded,
}

impl std::fmt::Display for AddressedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AddressedError::Transport(error) => write!(f, "addressed transport: {error}"),
            AddressedError::Handshake(error) => write!(f, "addressed handshake: {error}"),
            AddressedError::Codec(error) => write!(f, "addressed codec: {error}"),
            AddressedError::DeadlineExceeded => {
                write!(f, "addressed: no session with the peer before the deadline")
            }
        }
    }
}

impl std::error::Error for AddressedError {}

/// Every [`VerifiedAddressed`] for this identity, from any peer.
pub struct AddressedInbox {
    rx: broadcast::Receiver<Arc<VerifiedAddressed>>,
    owner: Option<Arc<daemon::DaemonOwner>>,
}

/// Why [`AddressedInbox::next`] returned no payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxError {
    /// This consumer fell a full buffer behind and `lost` payloads were
    /// dropped for it. The inbox continues with the next payload; what was
    /// lost is the caller's to account for (its senders see their deadlines).
    Lagged { lost: u64 },
    /// The handle is gone: no payload will arrive again.
    Closed,
}

impl AddressedInbox {
    fn drain_closed(&mut self) -> Result<Arc<VerifiedAddressed>, InboxError> {
        match self.rx.try_recv() {
            Ok(item) => Ok(item),
            Err(broadcast::error::TryRecvError::Lagged(lost)) => Err(InboxError::Lagged { lost }),
            Err(broadcast::error::TryRecvError::Empty | broadcast::error::TryRecvError::Closed) => {
                Err(InboxError::Closed)
            }
        }
    }

    /// The next payload, or why there is none.
    pub async fn next(&mut self) -> Result<Arc<VerifiedAddressed>, InboxError> {
        let received = if let Some(owner) = &self.owner {
            let mut closed = owner.closed.clone();
            if *closed.borrow() {
                return self.drain_closed();
            }
            tokio::select! {
                biased;
                result = self.rx.recv() => result,
                _ = closed.changed() => return self.drain_closed(),
            }
        } else {
            self.rx.recv().await
        };
        match received {
            Ok(item) => Ok(item),
            Err(broadcast::error::RecvError::Lagged(lost)) => Err(InboxError::Lagged { lost }),
            Err(broadcast::error::RecvError::Closed) => Err(InboxError::Closed),
        }
    }
}

/// Per-identity addressed state, shared by every handle on the identity.
pub(crate) struct AddressedState {
    sink: std::sync::OnceLock<Arc<dyn AddressedFrameSink>>,
    sessions: SessionManager,
    daemon_owner: tokio::sync::Mutex<std::sync::Weak<daemon::DaemonOwner>>,
    inbox_tx: broadcast::Sender<Arc<VerifiedAddressed>>,
    /// Bumped whenever a session is installed, so senders waiting on a
    /// handshake wake and re-check.
    established: watch::Sender<u64>,
}

impl AddressedState {
    pub(crate) fn new(
        identity: &LocalIdentity,
        registry: Arc<PeerKeyRegistry>,
        inbox_capacity: usize,
    ) -> Self {
        let (inbox_tx, _) = broadcast::channel(inbox_capacity);
        Self {
            sink: std::sync::OnceLock::new(),
            daemon_owner: tokio::sync::Mutex::new(std::sync::Weak::new()),
            sessions: SessionManager::new(identity.keypair.clone(), identity.peer_id, registry),
            inbox_tx,
            established: watch::Sender::new(0),
        }
    }

    fn session_installed(&self) {
        self.established.send_modify(|generation| *generation += 1);
    }
}

impl crate::Airc {
    /// Lose every addressed session, as a restart does, while the transport
    /// stays up. Dropping a handle does not close its LAN connections, so an
    /// in-process reopen cannot stand in for a restart in tests.
    #[doc(hidden)]
    pub fn forget_addressed_sessions_for_test(&self) {
        self.inner.addressed.sessions.reset();
    }

    /// Every payload addressed to this identity, from any peer, in any room.
    pub async fn addressed_inbox(
        &self,
        deadline: tokio::time::Instant,
    ) -> Result<AddressedInbox, AddressedError> {
        let rx = self.inner.addressed.inbox_tx.subscribe();
        let owner = self.addressed_daemon_owner(deadline).await?;
        Ok(AddressedInbox { rx, owner })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn result_then_eof_is_drained_before_closed() {
        let (tx, rx) = broadcast::channel(1);
        let (closed_tx, closed) = watch::channel(false);
        let owner = Arc::new(daemon::DaemonOwner {
            task: tokio::spawn(async {}),
            closed,
        });
        let mut inbox = AddressedInbox {
            rx,
            owner: Some(owner),
        };
        let item = Arc::new(VerifiedAddressed {
            from: PeerId::new(),
            to: PeerId::new(),
            kind: AddressedKind::CommandResult,
            correlation: Uuid::new_v4(),
            headers: Headers::new(),
            body: Body::text("completed"),
        });
        tx.send(item.clone()).unwrap();
        closed_tx.send_replace(true);
        assert_eq!(
            inbox.next().await.unwrap().correlation(),
            item.correlation()
        );
        assert_eq!(inbox.next().await.err(), Some(InboxError::Closed));
    }

    // what this catches: a consumer that falls behind is told how many
    // payloads it lost, as a typed outcome, instead of the loss being logged
    // and the next payload handed over as if nothing happened.
    #[tokio::test]
    async fn a_lagging_inbox_reports_its_loss_then_continues() {
        let (tx, rx) = broadcast::channel(1);
        let mut inbox = AddressedInbox { rx, owner: None };
        let item = |n: u128| {
            Arc::new(VerifiedAddressed {
                from: PeerId::from_u128(n),
                to: PeerId::from_u128(0),
                kind: AddressedKind::Command,
                correlation: Uuid::from_u128(n),
                headers: Headers::new(),
                body: Body::text("x"),
            })
        };
        for n in 1..=3 {
            tx.send(item(n)).unwrap();
        }
        assert_eq!(
            inbox.next().await.err(),
            Some(InboxError::Lagged { lost: 2 })
        );
        assert_eq!(
            inbox.next().await.unwrap().correlation(),
            Uuid::from_u128(3)
        );
        drop(tx);
        assert_eq!(inbox.next().await.err(), Some(InboxError::Closed));
    }
}
