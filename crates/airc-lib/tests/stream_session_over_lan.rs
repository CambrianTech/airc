//! Stream-plane crypto, end-to-end over REAL airc LAN transport.
//!
//! "See it work." The slice-1/2/3b crypto (`airc_protocol::session` +
//! `::handshake` + `::session_manager`) is unit-tested in isolation; this proves
//! it works over the actual wire: two airc peers on a real LAN TCP+TLS
//! connection run the Ed25519-authenticated X25519 handshake and then exchange
//! ChaCha20-Poly1305-sealed frames, BOTH directions. The two-machine M5⇄BigMama
//! test is this exact shape across boxes.
//!
//! airc is the byte pipe here (the SessionManager identities are independent of
//! the airc peer identities); slice 3c folds this into a `SessionTransport<T>`
//! so callers don't hand-carry the messages. This test is the dogfood proof
//! that the crypto round-trips on real transport.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use airc_core::{Body, Headers, PeerId};
use airc_lib::{Airc, EventStream};
use airc_protocol::{
    HandshakeInit, HandshakeResp, PeerKeyRegistry, PeerKeypair, SealedFrame, SessionManager,
};
use futures::stream::StreamExt;
use serde::{Deserialize, Serialize};
use tempfile::TempDir;

/// The stream-plane messages we carry as airc frame bodies. (Slice 3c moves this
/// framing into the transport; here the test hand-carries it.)
#[derive(Serialize, Deserialize)]
enum SessionMsg {
    Init(HandshakeInit),
    Resp(HandshakeResp),
    Data { from: PeerId, sealed: SealedFrame },
}

/// Marker header so the receiver picks our frames out of ambient airc traffic.
const STREAM_HEADER: &str = "x.stream";

async fn send_msg(airc: &Airc, msg: &SessionMsg) {
    let mut headers = Headers::new();
    headers.insert(STREAM_HEADER.to_string(), "1".to_string());
    airc.send(Body::text(serde_json::to_string(msg).unwrap()), headers)
        .await
        .expect("send session msg over LAN");
}

/// Next stream-plane message off `stream` (skipping our own echoes + non-stream
/// frames), with a generous timeout so a wire hang fails loudly.
async fn next_msg(stream: &mut EventStream, self_peer: PeerId) -> SessionMsg {
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), stream.next())
            .await
            .expect("timed out awaiting a stream-plane frame over LAN")
            .expect("event stream ended")
            .expect("event stream error");
        if event.peer_id == self_peer {
            continue; // our own broadcast echo
        }
        if event.headers.get(STREAM_HEADER).is_none() {
            continue; // ambient airc traffic (presence/lifecycle)
        }
        let text = event
            .body
            .as_ref()
            .and_then(Body::as_text)
            .expect("stream frame has a text body");
        return serde_json::from_str(text).expect("decode SessionMsg");
    }
}

#[test]
fn stream_plane_handshake_and_sealed_frames_over_real_lan() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        // --- real airc LAN transport: two peers, mutually trusted, connected ---
        let alice_home = TempDir::new().unwrap();
        let bob_home = TempDir::new().unwrap();
        let alice = Airc::open(alice_home.path()).await.expect("alice opens");
        let bob = Airc::open(bob_home.path()).await.expect("bob opens");
        let alice_spec = alice.peer_spec().parse().expect("alice spec");
        let bob_spec = bob.peer_spec().parse().expect("bob spec");
        alice.add_peer(bob_spec).await.expect("alice trusts bob");
        bob.add_peer(alice_spec).await.expect("bob trusts alice");
        alice.join("stream-proof").await.unwrap();
        bob.join("stream-proof").await.unwrap();
        let bob_addr = bob
            .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bob listens");
        alice
            .connect_lan(bob_addr, bob.peer_id())
            .await
            .expect("alice dials bob over LAN");

        // --- stream-plane identities (independent of the airc peer ids; airc is
        //     just the wire). Both enrolled so each can verify the other. ---
        let reg = Arc::new(PeerKeyRegistry::new());
        let kp_a = PeerKeypair::generate();
        let id_a = PeerId::new();
        let kp_b = PeerKeypair::generate();
        let id_b = PeerId::new();
        reg.enrol(id_a, 0, kp_a.public_bytes()).unwrap();
        reg.enrol(id_b, 0, kp_b.public_bytes()).unwrap();
        let sm_a = SessionManager::new(kp_a, id_a, Arc::clone(&reg));
        let sm_b = SessionManager::new(kp_b, id_b, Arc::clone(&reg));

        let mut sub_a = alice.subscribe().await.expect("alice subscribe");
        let mut sub_b = bob.subscribe().await.expect("bob subscribe");
        tokio::time::sleep(Duration::from_millis(100)).await; // connection + subs arm

        // 1. A → Init (over the wire)
        send_msg(&alice, &SessionMsg::Init(sm_a.begin_handshake(id_b))).await;
        // 2. B receives Init → installs its session → Resp (over the wire)
        let SessionMsg::Init(init) = next_msg(&mut sub_b, bob.peer_id()).await else {
            panic!("expected Init");
        };
        send_msg(
            &bob,
            &SessionMsg::Resp(sm_b.on_init(&init).expect("on_init")),
        )
        .await;
        // 3. A receives Resp → completes its session
        let SessionMsg::Resp(resp) = next_msg(&mut sub_a, alice.peer_id()).await else {
            panic!("expected Resp");
        };
        sm_a.on_resp(&resp).expect("on_resp");
        assert!(
            sm_a.has_session(id_b) && sm_b.has_session(id_a),
            "both sessions established via a handshake over REAL LAN"
        );

        let aad = b"airc-stream-routing-headers";
        // 4. A seals → over the wire → B opens
        let sealed = sm_a
            .seal_for(id_b, aad, b"mesh hello from A")
            .expect("seal a");
        send_msg(&alice, &SessionMsg::Data { from: id_a, sealed }).await;
        let SessionMsg::Data { from, sealed } = next_msg(&mut sub_b, bob.peer_id()).await else {
            panic!("expected Data A->B");
        };
        assert_eq!(
            sm_b.open_from(from, aad, &sealed).expect("open a->b"),
            b"mesh hello from A"
        );
        // 5. B seals → over the wire → A opens (both directions proven)
        let sealed = sm_b
            .seal_for(id_a, aad, b"mesh hello from B")
            .expect("seal b");
        send_msg(&bob, &SessionMsg::Data { from: id_b, sealed }).await;
        let SessionMsg::Data { from, sealed } = next_msg(&mut sub_a, alice.peer_id()).await else {
            panic!("expected Data B->A");
        };
        assert_eq!(
            sm_a.open_from(from, aad, &sealed).expect("open b->a"),
            b"mesh hello from B"
        );

        println!(
            "\n✅ stream-plane crypto over REAL airc LAN (TLS+TCP): \
             authenticated handshake + symmetric AEAD frames, both directions"
        );
    });
}
