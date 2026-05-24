//! Execution of resolved transport routes.
//!
//! Message construction, route selection, and adapter execution are
//! deliberately separate concerns. This layer is the bridge from a
//! selected `TransportKind` to the concrete adapter that can carry a
//! signed frame.

use airc_protocol::Frame;
use airc_transport::{LocalFsAdapter, Transport};

use crate::error::AircError;
use crate::route::policy::TransportKind;
use crate::{Airc, Room};

impl Airc {
    pub(crate) async fn execute_send_route(
        &self,
        route: TransportKind,
        room: &Room,
        frame: Frame,
    ) -> Result<(), AircError> {
        match route {
            TransportKind::LocalFs => {
                self.ensure_wire_subscriber(&room.wire).await?;
                LocalFsAdapter::new(&room.wire)
                    .send(frame)
                    .await
                    .map_err(|error| AircError::Transport(error.to_string()))
            }
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
            TransportKind::WebRtcDataChannel => Err(unwired_transport_error(route)),
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
