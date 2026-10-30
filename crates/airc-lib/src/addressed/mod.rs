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
    deadline: tokio::time::Instant,
    from: PeerId,
    to: PeerId,
    kind: AddressedKind,
    correlation: Uuid,
    headers: Headers,
    body: Body,
}

impl VerifiedAddressed {
    /// Retained local monotonic expiry; consumers must respect it after retrieval.
    pub fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }

    fn opened(
        from: PeerId,
        to: PeerId,
        inner: airc_protocol::addressed::SealedInner,
        deadline: tokio::time::Instant,
    ) -> Self {
        Self {
            deadline,
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
    /// A queued payload expired before this consumer retrieved it.
    Expired {
        from: PeerId,
        to: PeerId,
        kind: AddressedKind,
        correlation: Uuid,
    },
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for InboxError {}

impl AddressedInbox {
    fn admit(item: Arc<VerifiedAddressed>) -> Result<Arc<VerifiedAddressed>, InboxError> {
        if tokio::time::Instant::now() >= item.deadline {
            Err(InboxError::Expired {
                from: item.from(),
                to: item.to(),
                kind: item.kind(),
                correlation: item.correlation(),
            })
        } else {
            Ok(item)
        }
    }
    fn drain_closed(&mut self) -> Result<Arc<VerifiedAddressed>, InboxError> {
        match self.rx.try_recv() {
            Ok(item) => Self::admit(item),
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
            Ok(item) => Self::admit(item),
            Err(broadcast::error::RecvError::Lagged(lost)) => Err(InboxError::Lagged { lost }),
            Err(broadcast::error::RecvError::Closed) => Err(InboxError::Closed),
        }
    }
}

/// Per-identity addressed state, shared by every handle on the identity.
pub(crate) struct AddressedState {
    pub(super) handshake_waiters: send::HandshakeWaiters,
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
            handshake_waiters: Default::default(),
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
    async fn terminal_command_errors_release_the_inbox() {
        let home = tempfile::tempdir().unwrap();
        let airc = crate::Airc::open(home.path()).await.unwrap();
        for case in ["lagged", "deadline", "expired", "closed"] {
            let peer = PeerId::new();
            let correlation = Uuid::new_v4();
            let now = tokio::time::Instant::now();
            let deadline = if case == "deadline" {
                now
            } else {
                now + std::time::Duration::from_secs(5)
            };
            let (tx, rx) = broadcast::channel(1);
            let mut pending = crate::command_bus::PendingCommand::for_test(
                AddressedInbox { rx, owner: None },
                peer,
                airc.peer_id(),
                correlation,
                deadline,
            );
            let item = Arc::new(VerifiedAddressed {
                deadline: if case == "expired" { now } else { deadline },
                from: peer,
                to: airc.peer_id(),
                kind: AddressedKind::CommandProgress,
                correlation,
                headers: Headers::new(),
                body: Body::text("progress"),
            });
            if case == "expired" || case == "lagged" {
                tx.send(item.clone()).unwrap();
            }
            if case == "lagged" {
                tx.send(item).unwrap();
            }
            let tx = if case == "closed" {
                drop(tx);
                None
            } else {
                Some(tx)
            };
            let result = airc.next_command_event(&mut pending).await;
            match case {
                "lagged" => assert!(matches!(
                    result,
                    Err(crate::AircError::CommandInbox {
                        source: InboxError::Lagged { .. },
                        ..
                    })
                )),
                "closed" => assert!(matches!(
                    result,
                    Err(crate::AircError::CommandInbox {
                        source: InboxError::Closed,
                        ..
                    })
                )),
                _ => assert!(matches!(
                    result,
                    Err(crate::AircError::CommandDeadline { .. })
                )),
            }
            if let Some(tx) = tx {
                assert_eq!(tx.receiver_count(), 0);
            }
            assert!(matches!(
                airc.next_command_event(&mut pending).await,
                Err(crate::AircError::CommandFinished { .. })
            ));
        }
    }

    #[tokio::test]
    async fn progress_and_stream_end_cannot_settle_a_command() {
        let home = tempfile::tempdir().unwrap();
        let airc = crate::Airc::open(home.path()).await.unwrap();
        let peer = PeerId::new();
        let correlation = Uuid::new_v4();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let (tx, rx) = broadcast::channel(8);
        let make_pending = |rx| {
            crate::command_bus::PendingCommand::for_test(
                AddressedInbox { rx, owner: None },
                peer,
                airc.peer_id(),
                correlation,
                deadline,
            )
        };
        let mut events = make_pending(rx);
        let settled = make_pending(tx.subscribe());
        for (from, kind, body) in [
            (PeerId::new(), AddressedKind::CommandResult, "wrong peer"),
            (peer, AddressedKind::CommandProgress, "last chunk"),
            (peer, AddressedKind::CommandResult, "settled"),
        ] {
            let mut headers = Headers::new();
            headers.insert(crate::HEADER_STREAM_FINAL.into(), "true".into());
            tx.send(Arc::new(VerifiedAddressed {
                deadline,
                from,
                to: airc.peer_id(),
                kind,
                correlation,
                headers,
                body: Body::text(body),
            }))
            .unwrap();
        }
        assert!(
            matches!(airc.next_command_event(&mut events).await.unwrap(),
            crate::CommandEvent::Progress(frame) if frame.body().as_text() == Some("last chunk"))
        );
        assert!(
            matches!(airc.next_command_event(&mut events).await.unwrap(),
            crate::CommandEvent::Completed(frame) if frame.body().as_text() == Some("settled"))
        );
        assert!(matches!(
            airc.next_command_event(&mut events).await,
            Err(crate::AircError::CommandFinished { .. })
        ));
        assert_eq!(
            tx.receiver_count(),
            1,
            "completed exchange releases its inbox"
        );
        assert_eq!(
            airc.await_reply(settled).await.unwrap().body().as_text(),
            Some("settled")
        );
        assert_eq!(tx.receiver_count(), 0);
    }

    #[tokio::test]
    async fn unrelated_expired_result_does_not_abort_pending_exchange() {
        let home = tempfile::tempdir().unwrap();
        let airc = crate::Airc::open(home.path()).await.unwrap();
        let peer = PeerId::new();
        let correlation = Uuid::new_v4();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
        let (tx, rx) = broadcast::channel(4);
        let pending = crate::command_bus::PendingCommand::for_test(
            AddressedInbox { rx, owner: None },
            peer,
            airc.peer_id(),
            correlation,
            deadline,
        );
        for (id, expires) in [
            (Uuid::new_v4(), tokio::time::Instant::now()),
            (correlation, deadline),
        ] {
            tx.send(Arc::new(VerifiedAddressed {
                deadline: expires,
                from: peer,
                to: airc.peer_id(),
                kind: AddressedKind::CommandResult,
                correlation: id,
                headers: Headers::new(),
                body: Body::text("result"),
            }))
            .unwrap();
        }
        assert_eq!(
            airc.await_reply(pending).await.unwrap().correlation(),
            correlation
        );
        let pending = crate::command_bus::PendingCommand::for_test(
            AddressedInbox {
                rx: tx.subscribe(),
                owner: None,
            },
            peer,
            airc.peer_id(),
            correlation,
            deadline,
        );
        tx.send(Arc::new(VerifiedAddressed {
            deadline: tokio::time::Instant::now(),
            from: peer,
            to: airc.peer_id(),
            kind: AddressedKind::CommandResult,
            correlation,
            headers: Headers::new(),
            body: Body::text("expired matching result"),
        }))
        .unwrap();
        assert!(matches!(airc.await_reply(pending).await,
            Err(crate::AircError::CommandDeadline { correlation_id }) if correlation_id == correlation));
    }

    #[tokio::test]
    async fn reply_cannot_renew_expired_outer_budget_with_future_inner_deadline() {
        let home = tempfile::tempdir().unwrap();
        let airc = crate::Airc::open(home.path()).await.unwrap();
        let mut headers = Headers::new();
        headers.insert(
            airc_protocol::HEADER_AIRC_DEADLINE.into(),
            (crate::time::now_ms().unwrap() + 60_000).to_string(),
        );
        let request = VerifiedAddressed {
            deadline: tokio::time::Instant::now(),
            from: PeerId::new(),
            to: airc.peer_id(),
            kind: AddressedKind::Command,
            correlation: Uuid::new_v4(),
            headers,
            body: Body::text("late"),
        };
        assert!(matches!(
            airc.reply(&request, Headers::new(), Body::text("reply"))
                .await,
            Err(crate::AircError::CommandDeadline { .. })
        ));
        assert!(airc.inner.lan_tcp.lock().await.is_none());
    }

    #[tokio::test]
    async fn expired_payload_is_refused_even_when_draining_closed_owner() {
        let (tx, rx) = broadcast::channel(1);
        let mut inbox = AddressedInbox { rx, owner: None };
        let item = Arc::new(VerifiedAddressed {
            deadline: tokio::time::Instant::now(),
            from: PeerId::new(),
            to: PeerId::new(),
            kind: AddressedKind::Command,
            correlation: Uuid::new_v4(),
            headers: Headers::new(),
            body: Body::text("expired"),
        });
        tx.send(item.clone()).unwrap();
        assert!(matches!(
            inbox.next().await,
            Err(InboxError::Expired { .. })
        ));
        tx.send(item).unwrap();
        assert!(matches!(
            inbox.drain_closed(),
            Err(InboxError::Expired { .. })
        ));
    }

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
            deadline: tokio::time::Instant::now() + airc_ipc::client::DEFAULT_RPC_TIMEOUT,
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
                deadline: tokio::time::Instant::now() + airc_ipc::client::DEFAULT_RPC_TIMEOUT,
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
