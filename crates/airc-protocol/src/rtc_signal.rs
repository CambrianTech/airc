//! WebRTC signaling payload contract.
//!
//! AIRC carries the control plane for WebRTC datachannel routes:
//! offer/answer/ICE metadata as signed AIRC events. Media bytes and
//! datachannel bytes stay on WebRTC. Consumers place this payload in a
//! frame body and set `forge.body_hint = WEBRTC_SIGNAL_BODY_HINT`.

use serde::{Deserialize, Serialize};

use airc_core::{EventId, PeerId, RoomId};

pub const WEBRTC_SIGNAL_BODY_HINT: &str = "forge.webrtc.signal";
pub const WEBRTC_SIGNAL_SCHEMA_VERSION: u16 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WebRtcSignalKind {
    Offer,
    Answer,
    IceCandidate,
    Teardown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebRtcSignal {
    pub schema_version: u16,
    pub session_id: EventId,
    pub room_id: RoomId,
    pub from_peer: PeerId,
    pub to_peer: PeerId,
    pub kind: WebRtcSignalKind,
    /// SDP text for offer/answer, ICE candidate JSON for
    /// `IceCandidate`, or a short machine-readable reason for
    /// `Teardown`.
    pub payload: String,
}

impl WebRtcSignal {
    pub fn new(
        session_id: EventId,
        room_id: RoomId,
        from_peer: PeerId,
        to_peer: PeerId,
        kind: WebRtcSignalKind,
        payload: impl Into<String>,
    ) -> Self {
        Self {
            schema_version: WEBRTC_SIGNAL_SCHEMA_VERSION,
            session_id,
            room_id,
            from_peer,
            to_peer,
            kind,
            payload: payload.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signal_contract_round_trips_without_transport_fields() {
        let signal = WebRtcSignal::new(
            EventId::from_u128(1),
            RoomId::from_u128(2),
            PeerId::from_u128(3),
            PeerId::from_u128(4),
            WebRtcSignalKind::Offer,
            "v=0\r\n",
        );

        let json = serde_json::to_string(&signal).unwrap();
        let decoded: WebRtcSignal = serde_json::from_str(&json).unwrap();

        assert_eq!(decoded, signal);
        assert!(!json.contains("udp_addr"));
        assert!(!json.contains("transport_kind"));
    }
}
