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

mod receive;
mod send;

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

/// One addressed payload received and opened.
#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedAddressed {
    /// The session's handshake-pinned identity, never a header.
    pub from: PeerId,
    /// This scope's identity.
    pub to: PeerId,
    pub kind: AddressedKind,
    pub correlation: Uuid,
    pub headers: Headers,
    pub body: Body,
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
    /// This handle is attached to a machine daemon, whose routing of addressed
    /// frames is not built yet. Refused rather than sent any other way.
    DaemonPathNotBuilt,
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
            AddressedError::DaemonPathNotBuilt => write!(
                f,
                "addressed: a daemon-attached handle cannot send addressed traffic yet"
            ),
        }
    }
}

impl std::error::Error for AddressedError {}

/// Every [`VerifiedAddressed`] for this identity, from any peer.
pub struct AddressedInbox {
    rx: broadcast::Receiver<Arc<VerifiedAddressed>>,
}

impl AddressedInbox {
    /// The next payload, or `None` once the handle is gone. A consumer that
    /// falls a full buffer behind loses the overflow: the loss is logged, and
    /// a lost command surfaces to its sender as its own deadline.
    pub async fn next(&mut self) -> Option<Arc<VerifiedAddressed>> {
        loop {
            match self.rx.recv().await {
                Ok(item) => return Some(item),
                Err(broadcast::error::RecvError::Lagged(lost)) => {
                    tracing::error!(
                        target: "airc::addressed",
                        lost,
                        "addressed inbox consumer fell behind; payloads were dropped"
                    );
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }
}

/// Per-identity addressed state, shared by every handle on the identity.
pub(crate) struct AddressedState {
    sessions: SessionManager,
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
    /// Every payload addressed to this identity, from any peer, in any room.
    pub fn addressed_inbox(&self) -> AddressedInbox {
        AddressedInbox {
            rx: self.inner.addressed.inbox_tx.subscribe(),
        }
    }
}
