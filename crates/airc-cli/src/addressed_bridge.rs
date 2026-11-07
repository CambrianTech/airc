//! The daemon's single transport handle feeds its bounded addressed switch.
use airc_daemon::addressed_switch::{ForwardError, RouteError};
use airc_daemon::AddressedAdmissionError;
use airc_daemon::DaemonState;
use airc_lib::addressed::{
    AddressedError, AddressedFrameSink, AdmissionRefusal, ForwardRefusal, QueueRefusal,
};
use airc_protocol::Frame;
use std::sync::Weak;

pub(crate) struct DaemonAddressedSink(pub Weak<DaemonState>);

#[async_trait::async_trait]
impl AddressedFrameSink for DaemonAddressedSink {
    async fn deliver(&self, frame: Frame) -> Result<(), AddressedError> {
        let state = self.0.upgrade().ok_or_else(|| AddressedError::Admission {
            cause: AdmissionRefusal::OwnerClosed,
            detail: "addressed daemon owner closed".into(),
        })?;
        state
            .receive_addressed_frame(frame)
            .await
            .map_err(|error| AddressedError::Admission {
                cause: admission_refusal(&error),
                detail: error.to_string(),
            })
    }
}

/// The daemon's admission error as the library's typed cause: the refusal counter keys on it, so a
/// cause is never erased into text before it is counted (Fable on #1651).
fn admission_refusal(error: &AddressedAdmissionError) -> AdmissionRefusal {
    match error {
        AddressedAdmissionError::WrongKindOrChannel => AdmissionRefusal::WrongKindOrChannel,
        AddressedAdmissionError::WrongTarget => AdmissionRefusal::WrongTarget,
        AddressedAdmissionError::SenderMustSign => AdmissionRefusal::SenderMustSign,
        AddressedAdmissionError::Trust(_) => AdmissionRefusal::Trust,
        AddressedAdmissionError::Signature(_) => AdmissionRefusal::Signature,
        AddressedAdmissionError::Queue(route) => AdmissionRefusal::Queue(queue_refusal(route)),
        AddressedAdmissionError::Forward(forward) => {
            AdmissionRefusal::Forward(forward_refusal(forward))
        }
        AddressedAdmissionError::Expiry(_) => AdmissionRefusal::Expiry,
    }
}

fn queue_refusal(error: &RouteError) -> QueueRefusal {
    match error {
        RouteError::ReceiverLimit => QueueRefusal::ReceiverLimit,
        RouteError::ReceiverBusy => QueueRefusal::ReceiverBusy,
        RouteError::NoReceiver => QueueRefusal::NoReceiver,
        RouteError::Backpressure => QueueRefusal::Backpressure,
        RouteError::Closed => QueueRefusal::Closed,
        RouteError::StatePoisoned => QueueRefusal::StatePoisoned,
        RouteError::DeadlineElapsed => QueueRefusal::DeadlineElapsed,
    }
}

fn forward_refusal(error: &ForwardError) -> ForwardRefusal {
    match error {
        ForwardError::NotInstalled => ForwardRefusal::NotInstalled,
        ForwardError::AlreadyInstalled => ForwardRefusal::AlreadyInstalled,
        ForwardError::UnknownRecipient => ForwardRefusal::UnknownRecipient,
        ForwardError::UnknownHost => ForwardRefusal::UnknownHost,
        ForwardError::LocalOwnerAbsent => ForwardRefusal::LocalOwnerAbsent,
        ForwardError::InvalidEndpoints => ForwardRefusal::InvalidEndpoints,
        ForwardError::Trust(_) => ForwardRefusal::Trust,
        ForwardError::Transport(_) => ForwardRefusal::Transport,
        ForwardError::Admission(route) => ForwardRefusal::Admission(queue_refusal(route)),
    }
}

pub(crate) struct DaemonAddressedForwarder {
    pub airc: airc_lib::Airc,
    pub home: std::path::PathBuf,
    pub daemon: airc_core::PeerId,
}

#[async_trait::async_trait]
impl airc_daemon::addressed_switch::AddressedForwarder for DaemonAddressedForwarder {
    async fn forward(
        &self,
        recipient: airc_core::PeerId,
        frame: Frame,
        deadline: tokio::time::Instant,
    ) -> Result<(), airc_daemon::addressed_switch::ForwardError> {
        use airc_daemon::addressed_switch::ForwardError;
        let peers = airc_trust::load(&self.home)
            .await
            .map_err(|e| ForwardError::Trust(e.to_string()))?;
        let host = resolve_host(&peers, recipient, self.daemon)?;
        self.airc
            .forward_addressed_frame(host, frame, deadline)
            .await
            .map_err(|e| ForwardError::Transport(e.to_string()))
    }
}

fn resolve_host(
    peers: &[airc_trust::StoredPeer],
    recipient: airc_core::PeerId,
    daemon: airc_core::PeerId,
) -> Result<airc_core::PeerId, airc_daemon::addressed_switch::ForwardError> {
    use airc_daemon::addressed_switch::ForwardError;
    let record = peers
        .iter()
        .find(|p| p.peer_id == recipient)
        .ok_or(ForwardError::UnknownRecipient)?;
    let host = match record.endpoints_peer_id {
        Some(host) => host,
        None => {
            let encoded = record
                .endpoints_json
                .as_deref()
                .ok_or(ForwardError::UnknownHost)?;
            let endpoints: Vec<airc_lib::RouteEndpoint> =
                serde_json::from_str(encoded).map_err(|_| ForwardError::InvalidEndpoints)?;
            if endpoints.is_empty() {
                return Err(ForwardError::UnknownHost);
            }
            recipient
        }
    };
    if host == daemon {
        return Err(ForwardError::LocalOwnerAbsent);
    }
    if !peers.iter().any(|p| p.peer_id == host) {
        return Err(ForwardError::UnknownHost);
    }
    Ok(host)
}

#[cfg(test)]
mod tests {
    use super::*;
    use airc_core::PeerId;
    use airc_daemon::addressed_switch::ForwardError;

    #[tokio::test]
    async fn host_resolution_requires_explicit_custody_and_enrolled_host() {
        let home = tempfile::tempdir().unwrap();
        let recipient = PeerId::new();
        let host = PeerId::new();
        let daemon = PeerId::new();
        let key = airc_protocol::PeerKeypair::generate();
        let mut record = airc_trust::add(home.path(), recipient, key.public_bytes())
            .await
            .unwrap();
        assert!(matches!(
            resolve_host(&[], recipient, daemon),
            Err(ForwardError::UnknownRecipient)
        ));
        assert!(matches!(
            resolve_host(&[record.clone()], recipient, daemon),
            Err(ForwardError::UnknownHost)
        ));
        record.endpoints_json = Some("[]".into());
        assert!(matches!(
            resolve_host(&[record.clone()], recipient, daemon),
            Err(ForwardError::UnknownHost)
        ));
        record.endpoints_json = Some("invalid".into());
        assert!(matches!(
            resolve_host(&[record.clone()], recipient, daemon),
            Err(ForwardError::InvalidEndpoints)
        ));
        record.endpoints_json = Some(r#"[{"kind":"lan_tcp","addr":"127.0.0.1:9999"}]"#.into());
        assert_eq!(
            resolve_host(&[record.clone()], recipient, daemon).unwrap(),
            recipient
        );
        record.endpoints_peer_id = Some(daemon);
        assert!(matches!(
            resolve_host(&[record.clone()], recipient, daemon),
            Err(ForwardError::LocalOwnerAbsent)
        ));
        record.endpoints_peer_id = Some(host);
        assert!(matches!(
            resolve_host(&[record.clone()], recipient, daemon),
            Err(ForwardError::UnknownHost)
        ));
        let host_record = airc_trust::add(
            home.path(),
            host,
            airc_protocol::PeerKeypair::generate().public_bytes(),
        )
        .await
        .unwrap();
        assert_eq!(
            resolve_host(&[record, host_record], recipient, daemon).unwrap(),
            host
        );
    }
}

#[cfg(test)]
#[path = "addressed_bridge_tests.rs"]
mod transport_tests;
