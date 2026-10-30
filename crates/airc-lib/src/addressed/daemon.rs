//! Lazy daemon transport ownership. Only inbox/send leases keep the worker alive.
use super::AddressedError;
use crate::Airc;
use std::sync::Arc;
use tokio::sync::watch;
use tokio::time::Instant;

pub(super) struct DaemonOwner {
    pub(super) task: tokio::task::JoinHandle<()>,
    pub closed: watch::Receiver<bool>,
}
impl Drop for DaemonOwner {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl Airc {
    pub(super) async fn addressed_daemon_owner(
        &self,
        deadline: Instant,
    ) -> Result<Option<Arc<DaemonOwner>>, AddressedError> {
        let Some(client) = &self.inner.daemon_client else {
            return Ok(None);
        };
        tokio::time::timeout_at(deadline, async {
            let mut slot = self.inner.addressed.daemon_owner.lock().await;
            if let Some(owner) = slot.upgrade() {
                if *owner.closed.borrow() {
                    return Err(AddressedError::Transport("addressed daemon connection closed; release old inboxes before reacquiring".into()));
                }
                return Ok(Some(owner));
            }
            let status = client.status().await.map_err(|e| AddressedError::Transport(e.to_string()))?;
            let daemon = uuid::Uuid::parse_str(&status.peer_id).map(airc_core::PeerId::from_uuid).map_err(|e| AddressedError::Transport(format!("invalid daemon identity: {e}")))?;
            let mut stream = client.attach_addressed(self.peer_id(), daemon, &self.inner.identity.keypair, 0)
                .await.map_err(|e| AddressedError::Transport(e.to_string()))?;
            let (closed_tx, closed) = watch::channel(false);
            let airc = self.clone();
            let task = tokio::spawn(async move {
                while let Ok(Some(airc_ipc::Response::Addressed { frame })) =
                    airc_ipc::codec::read_response_frame(&mut stream).await
                {
                    airc.receive_addressed(frame).await;
                }
                closed_tx.send_replace(true);
            });
            let owner = Arc::new(DaemonOwner { task, closed });
            *slot = Arc::downgrade(&owner);
            Ok(Some(owner))
        }).await.map_err(|_| AddressedError::DeadlineExceeded)?
    }
}
