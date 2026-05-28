//! Execution of resolved transport routes.
//!
//! Message construction, route selection, and adapter execution are
//! deliberately separate concerns. This layer is the bridge from a
//! selected `TransportKind` to the concrete adapter that can carry a
//! signed frame.

use airc_protocol::Frame;
use airc_transport::Transport;

use crate::error::AircError;
use crate::route::policy::TransportKind;
use crate::{Airc, Room};

impl Airc {
    pub(crate) async fn execute_send_route(
        &self,
        route: TransportKind,
        _room: &Room,
        frame: Frame,
    ) -> Result<(), AircError> {
        match route {
            TransportKind::LanTcp | TransportKind::Tailscale => {
                self.ensure_lan_subscriber().await?;
                self.lan_adapter()
                    .await?
                    .send(frame)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))
            }
            TransportKind::Udp => {
                self.ensure_udp_subscriber().await?;
                self.udp_adapter()
                    .await?
                    .send(frame)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))
            }
            TransportKind::WebRtcDataChannel => {
                let target_peer = match frame.envelope.target {
                    airc_core::transcript::MentionTarget::Peer(peer) => peer,
                    airc_core::transcript::MentionTarget::All
                    | airc_core::transcript::MentionTarget::Room(_) => {
                        return Err(AircError::Route(
                            "WebRtcDataChannel requires a Peer-directed target; \
                             rooms/broadcasts must go over LAN-TCP or Relay"
                                .into(),
                        ));
                    }
                };
                self.ensure_webrtc_subscriber(target_peer).await?;
                self.webrtc_adapter_for(target_peer)
                    .await?
                    .send(frame)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))
            }
            TransportKind::Reticulum => Err(unwired_transport_error(route)),
            TransportKind::Relay => {
                self.ensure_relay_subscriber().await?;
                self.relay_adapter()
                    .await?
                    .send(frame)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))
            }
            TransportKind::Ssh => Err(unwired_transport_error(route)),
            TransportKind::GhGist => Err(unwired_transport_error(route)),
        }
    }
}

fn unwired_transport_error(kind: TransportKind) -> AircError {
    AircError::Route(format!(
        "{kind:?} route selected but no executable adapter is wired"
    ))
}
