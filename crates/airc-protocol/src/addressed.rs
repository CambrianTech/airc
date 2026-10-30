//! Addressed delivery on the wire: a command or its result between two peers,
//! readable by those two peers only.
//!
//! A [`FrameKind::Addressed`](crate::envelope::FrameKind::Addressed) frame's body
//! is one [`AddressedWire`]: a handshake step, or a [`SealedFrame`] whose
//! plaintext is a [`SealedInner`]. The inner kind, correlation, headers and body
//! are inside the AEAD, so they are confidential and authenticated end to end;
//! the outer frame's kind sits outside the envelope signature and decides
//! nothing beyond "addressed". [`aad`] binds the routing fields a forwarder must
//! read (sender, addressee), so a sealed frame readdressed fails to open. A
//! gateway that re-signs the envelope keeps those fields, so the seal survives
//! the daemon path. Addressed traffic belongs to no room: its envelope channel
//! is always [`addressed_channel`].
//!
//! Nothing here is ever persisted: addressed frames never enter a transcript on
//! any machine, including the addressee's (see `airc-lib`'s `addressed` module).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use airc_core::{Body, Headers, PeerId};

use crate::envelope::ChannelId;
use crate::handshake::{HandshakeInit, HandshakeResp};
use crate::session::SealedFrame;

const DOMAIN_AAD: &[u8] = b"airc-addressed-v1-aad";

/// The envelope channel of every addressed frame. Addressed traffic is bound to
/// two identities, never to a room; a receiver refuses any other channel.
pub fn addressed_channel() -> ChannelId {
    ChannelId::from_u128(0)
}

/// What an addressed exchange carries. Inner-only: neither kind ever travels as
/// a plaintext frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AddressedKind {
    Command,
    CommandResult,
}

/// The body of an addressed frame.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AddressedWire {
    /// Opens (or, from a restarted peer, re-opens) the session.
    Init(HandshakeInit),
    /// Completes it.
    Resp(HandshakeResp),
    /// A [`SealedInner`] under the session.
    Sealed(SealedFrame),
}

/// The plaintext of a sealed frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SealedInner {
    pub kind: AddressedKind,
    pub correlation: Uuid,
    pub headers: Headers,
    pub body: Body,
}

/// Why addressed bytes did not decode.
#[derive(Debug)]
pub struct AddressedCodecError(String);

impl std::fmt::Display for AddressedCodecError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "addressed codec: {}", self.0)
    }
}

impl std::error::Error for AddressedCodecError {}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, AddressedCodecError> {
    let mut bytes = Vec::new();
    ciborium::into_writer(value, &mut bytes).map_err(|e| AddressedCodecError(e.to_string()))?;
    Ok(bytes)
}

fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, AddressedCodecError> {
    ciborium::from_reader(bytes).map_err(|e| AddressedCodecError(e.to_string()))
}

impl AddressedWire {
    pub fn to_body(&self) -> Result<Body, AddressedCodecError> {
        Ok(Body::Binary(encode(self)?))
    }

    pub fn from_body(body: &Body) -> Result<Self, AddressedCodecError> {
        match body {
            Body::Binary(bytes) => decode(bytes),
            Body::Json(_) => Err(AddressedCodecError("addressed body is not binary".into())),
        }
    }
}

impl SealedInner {
    pub fn to_plaintext(&self) -> Result<Vec<u8>, AddressedCodecError> {
        encode(self)
    }

    pub fn from_plaintext(bytes: &[u8]) -> Result<Self, AddressedCodecError> {
        decode(bytes)
    }
}

/// The associated data both ends compute for a sealed frame: the routing
/// fields, authenticated but left readable for forwarders.
pub fn aad(from: PeerId, to: PeerId) -> Vec<u8> {
    let mut aad = Vec::with_capacity(DOMAIN_AAD.len() + 32);
    aad.extend_from_slice(DOMAIN_AAD);
    aad.extend_from_slice(from.as_uuid().as_bytes());
    aad.extend_from_slice(to.as_uuid().as_bytes());
    aad
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keypair::PeerKeypair;
    use crate::session_manager::SessionManager;
    use crate::signature::PeerKeyRegistry;
    use std::sync::Arc;

    fn pair() -> (SessionManager, PeerId, SessionManager, PeerId) {
        let reg = Arc::new(PeerKeyRegistry::new());
        let (ka, kb) = (PeerKeypair::generate(), PeerKeypair::generate());
        let (a, b) = (PeerId::new(), PeerId::new());
        reg.enrol(a, 0, ka.public_bytes()).unwrap();
        reg.enrol(b, 0, kb.public_bytes()).unwrap();
        let (ma, mb) = (
            SessionManager::new(ka, a, Arc::clone(&reg)),
            SessionManager::new(kb, b, Arc::clone(&reg)),
        );
        let init = AddressedWire::from_body(
            &AddressedWire::Init(ma.begin_handshake(b, 1))
                .to_body()
                .unwrap(),
        )
        .unwrap();
        let AddressedWire::Init(init) = init else {
            panic!("init round-trip")
        };
        let resp = mb.on_init(&init).unwrap();
        ma.on_resp(&resp).unwrap();
        (ma, a, mb, b)
    }

    // what this catches: the whole addressed path in miniature: a command
    // sealed by A, carried as a Body, opens at B to the same kind, correlation
    // and body, and its bytes on the wire do not contain the plaintext.
    #[test]
    fn a_sealed_command_round_trips_and_is_not_readable_on_the_wire() {
        let (a, a_id, b, b_id) = pair();
        let inner = SealedInner {
            kind: AddressedKind::Command,
            correlation: Uuid::new_v4(),
            headers: Headers::new(),
            body: Body::text("resident prompt: private"),
        };
        let sealed = a
            .seal_for(b_id, &aad(a_id, b_id), &inner.to_plaintext().unwrap())
            .unwrap();
        let body = AddressedWire::Sealed(sealed).to_body().unwrap();
        let Body::Binary(wire) = &body else {
            panic!("binary body")
        };
        assert!(!wire.windows(7).any(|w| w == b"private"));

        let AddressedWire::Sealed(frame) = AddressedWire::from_body(&body).unwrap() else {
            panic!("sealed round-trip")
        };
        let opened = b.open_from(a_id, &aad(a_id, b_id), &frame).unwrap();
        assert_eq!(SealedInner::from_plaintext(&opened).unwrap(), inner);
    }

    // what this catches: the AAD binds the addressee, so a forwarder that
    // rewrites who a sealed frame is for gets a frame that does not open.
    #[test]
    fn a_readdressed_sealed_frame_does_not_open() {
        let (a, a_id, b, b_id) = pair();
        let frame = a.seal_for(b_id, &aad(a_id, b_id), b"x").unwrap();
        assert!(b
            .open_from(a_id, &aad(a_id, PeerId::new()), &frame)
            .is_err());
    }
}
