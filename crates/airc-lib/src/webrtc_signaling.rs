//! WebRTC offer/answer/ICE signaling carried over the AIRC substrate.
//!
//! WebRTC requires an out-of-band channel to exchange the SDP
//! offer/answer pair and the ICE candidates that let the two
//! endpoints find each other through NAT. AIRC already provides a
//! signed, peer-directed event channel, so we use the substrate
//! itself as the signaling carrier — no separate signaling server.
//!
//! Routing: signaling frames are `FrameKind::Event` with
//! [`MentionTarget::Peer`] addressed at the other endpoint. The route
//! policy admits UDP/WebRTC for `RouteClass::MediaSignaling`, but
//! signaling itself must travel over an already-established route
//! (LAN-TCP, Tailscale, or LocalFs on the same machine) — it can't
//! travel over the WebRTC connection it is trying to bootstrap.
//!
//! Headers:
//! - `airc.webrtc.signaling.kind`: `"offer" | "answer" | "ice_candidate"`
//! - `airc.webrtc.signaling.session_id`: uuid — distinguishes
//!   concurrent connection attempts so a stale offer can't be confused
//!   with a fresh one
//!
//! Body: JSON-encoded [`SignalingMessage`].

use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Header key carrying the signaling-message kind.
pub const HEADER_WEBRTC_SIGNALING_KIND: &str = "airc.webrtc.signaling.kind";
/// Header key carrying the signaling session id (UUID).
pub const HEADER_WEBRTC_SIGNALING_SESSION_ID: &str = "airc.webrtc.signaling.session_id";

/// One signaling message exchanged between two endpoints to negotiate
/// an RTCPeerConnection + DataChannel.
///
/// Carries SDP and ICE-candidate strings as `gh`-CLI-style opaque
/// payload — the substrate doesn't parse them, only routes them. The
/// receiving endpoint feeds them straight into its
/// `RTCPeerConnection` API.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalingMessage {
    /// Initiator → responder. Contains the SDP offer the responder
    /// must `set_remote_description` and answer.
    Offer { session_id: Uuid, sdp: String },
    /// Responder → initiator. SDP answer the initiator
    /// `set_remote_description`s.
    Answer { session_id: Uuid, sdp: String },
    /// Either side. A locally-gathered ICE candidate the peer should
    /// `add_ice_candidate`. Empty `candidate` signals end-of-candidates.
    IceCandidate {
        session_id: Uuid,
        candidate: String,
        sdp_mid: Option<String>,
        sdp_mline_index: Option<u16>,
    },
}

impl SignalingMessage {
    pub fn session_id(&self) -> Uuid {
        match self {
            SignalingMessage::Offer { session_id, .. }
            | SignalingMessage::Answer { session_id, .. }
            | SignalingMessage::IceCandidate { session_id, .. } => *session_id,
        }
    }

    pub fn kind_str(&self) -> &'static str {
        match self {
            SignalingMessage::Offer { .. } => "offer",
            SignalingMessage::Answer { .. } => "answer",
            SignalingMessage::IceCandidate { .. } => "ice_candidate",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_round_trips_through_json() {
        let msg = SignalingMessage::Offer {
            session_id: Uuid::nil(),
            sdp: "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n".to_string(),
        };
        let json = serde_json::to_string(&msg).expect("encode");
        let decoded: SignalingMessage = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn answer_round_trips_through_json() {
        let msg = SignalingMessage::Answer {
            session_id: Uuid::nil(),
            sdp: "v=0\r\no=- 0 0 IN IP4 0.0.0.0\r\n".to_string(),
        };
        let json = serde_json::to_string(&msg).expect("encode");
        let decoded: SignalingMessage = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn ice_candidate_round_trips_through_json() {
        let msg = SignalingMessage::IceCandidate {
            session_id: Uuid::nil(),
            candidate: "candidate:0 1 UDP 2122252543 192.0.2.1 50000 typ host".to_string(),
            sdp_mid: Some("0".to_string()),
            sdp_mline_index: Some(0),
        };
        let json = serde_json::to_string(&msg).expect("encode");
        let decoded: SignalingMessage = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn end_of_candidates_sentinel_round_trips() {
        let msg = SignalingMessage::IceCandidate {
            session_id: Uuid::nil(),
            candidate: String::new(),
            sdp_mid: None,
            sdp_mline_index: None,
        };
        let json = serde_json::to_string(&msg).expect("encode");
        let decoded: SignalingMessage = serde_json::from_str(&json).expect("decode");
        assert_eq!(decoded, msg);
    }

    #[test]
    fn kind_str_matches_variant() {
        let sid = Uuid::nil();
        assert_eq!(
            SignalingMessage::Offer {
                session_id: sid,
                sdp: String::new()
            }
            .kind_str(),
            "offer"
        );
        assert_eq!(
            SignalingMessage::Answer {
                session_id: sid,
                sdp: String::new()
            }
            .kind_str(),
            "answer"
        );
        assert_eq!(
            SignalingMessage::IceCandidate {
                session_id: sid,
                candidate: String::new(),
                sdp_mid: None,
                sdp_mline_index: None,
            }
            .kind_str(),
            "ice_candidate"
        );
    }
}
