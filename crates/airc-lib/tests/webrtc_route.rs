//! Integration: full WebRTC orchestration round-trip without GitHub.
//!
//! Two `Airc` instances on separate homes share a local-fs wire so the
//! signaling messages can travel. Bob runs the responder via
//! [`Airc::accept_webrtc_offers`]; Alice calls
//! [`Airc::open_webrtc_to(bob)`] which drives the offer/answer
//! handshake over the AIRC mesh. Once the DataChannel is open on both
//! sides, both `replace_transport_health` with WebRTC-only so the
//! route resolver has no other choice, then Alice sends a control
//! event whose only viable route is `TransportKind::WebRtcDataChannel`.
//!
//! Uses the same gather-complete pattern as the existing
//! `webrtc_datachannel/tests.rs` adapter tests, and the
//! `send_frame_to_for_test` doc-hidden alias from #955 because UDP /
//! WebRTC are only admissible for non-`DataInteractive` route
//! classes.

use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use airc_core::PeerId;
use airc_lib::{
    Airc, Body, Headers, IncomingTrack, MentionTarget, OpenedWebRtcConnection, OutgoingAudioTrack,
    OutgoingVideoTrack, PeerSpec, TransportHealthSample, TransportHealthState, TransportKind,
    TransportRole,
};
use airc_protocol::FrameKind;
use bytes::Bytes;
use futures::stream::StreamExt;
use rtc::rtp_transceiver::rtp_sender::RtpCodecKind;
use rtc_media::Sample;
use tempfile::TempDir;
use webrtc::media_stream::Track;

static CRYPTO_INIT: LazyLock<Mutex<bool>> = LazyLock::new(|| Mutex::new(false));

fn ensure_crypto_provider() {
    let mut guard = CRYPTO_INIT.lock().unwrap();
    if !*guard {
        let _ = rustls::crypto::ring::default_provider().install_default();
        *guard = true;
    }
}

struct PairedAircFixture {
    _alice_home: TempDir,
    _bob_home: TempDir,
    _wire_dir: TempDir,
    alice: Airc,
    bob: Airc,
    alice_spec: PeerSpec,
    bob_spec: PeerSpec,
}

async fn paired_airc(room: &str) -> PairedAircFixture {
    let alice_home = TempDir::new().expect("alice home");
    let bob_home = TempDir::new().expect("bob home");
    let wire_dir = TempDir::new().expect("shared wire dir");
    let wire_path = wire_dir.path().join("wire.jsonl");

    let alice = Airc::open(alice_home.path()).await.expect("alice opens");
    let bob = Airc::open(bob_home.path()).await.expect("bob opens");

    let alice_spec: PeerSpec = alice.peer_spec().parse().expect("alice peer spec");
    let bob_spec: PeerSpec = bob.peer_spec().parse().expect("bob peer spec");
    alice
        .add_peer(bob_spec.clone())
        .await
        .expect("alice trusts bob");
    bob.add_peer(alice_spec.clone())
        .await
        .expect("bob trusts alice");

    alice
        .join_with_wire(room, wire_path.clone())
        .await
        .expect("alice joins room");
    bob.join_with_wire(room, wire_path)
        .await
        .expect("bob joins room");

    PairedAircFixture {
        _alice_home: alice_home,
        _bob_home: bob_home,
        _wire_dir: wire_dir,
        alice,
        bob,
        alice_spec,
        bob_spec,
    }
}

async fn write_fixture_media_samples(opened: &OpenedWebRtcConnection) {
    let audio_ssrcs = opened.outgoing_audio[0].ssrcs().await;
    let audio_ssrc = audio_ssrcs
        .first()
        .copied()
        .expect("outgoing audio track has an ssrc");
    opened.outgoing_audio[0]
        .write_sample(
            audio_ssrc,
            &Sample {
                data: Bytes::from_static(&[0xf8, 0xff, 0xfe]),
                duration: Duration::from_millis(20),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice writes audio sample");

    let video_ssrcs = opened.outgoing_video[0].ssrcs().await;
    let video_ssrc = video_ssrcs
        .first()
        .copied()
        .expect("outgoing video track has an ssrc");
    opened.outgoing_video[0]
        .write_sample(
            video_ssrc,
            &Sample {
                data: Bytes::from_static(&[
                    0x10, 0x00, 0x00, 0x9d, 0x01, 0x2a, 0x01, 0x00, 0x01, 0x00,
                ]),
                duration: Duration::from_millis(33),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("alice writes video sample");
}

async fn collect_track_kinds(
    track_rx: std::sync::mpsc::Receiver<(PeerId, IncomingTrack)>,
    expected_peer: PeerId,
) -> Vec<RtpCodecKind> {
    let tracks = tokio::time::timeout(Duration::from_secs(10), async move {
        let mut tracks = Vec::new();
        loop {
            if let Ok(track) = track_rx.try_recv() {
                tracks.push(track);
                if tracks.len() == 2 {
                    return tracks;
                }
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .expect("bob receives inbound media tracks");

    let mut kinds = Vec::new();
    for (remote_peer, track) in tracks {
        assert_eq!(remote_peer, expected_peer);
        assert!(
            !track.label().await.is_empty(),
            "remote track should expose a label"
        );
        kinds.push(track.kind().await);
    }
    kinds.sort_by_key(|kind| match kind {
        RtpCodecKind::Unspecified => 0,
        RtpCodecKind::Audio => 1,
        RtpCodecKind::Video => 2,
    });
    kinds
}

fn force_webrtc_route(airc: &Airc) {
    let webrtc_only = [TransportHealthSample {
        kind: TransportKind::WebRtcDataChannel,
        role: TransportRole::Direct,
        state: TransportHealthState::Healthy,
        rtt_ms: None,
        success_ppm: None,
    }];
    airc.replace_transport_health(webrtc_only).unwrap();
}

async fn wait_for_webrtc_channel(airc: Airc, peer_id: PeerId) {
    tokio::time::timeout(Duration::from_secs(20), async move {
        loop {
            if airc.has_webrtc_channel(peer_id).await {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("webrtc adapter registered within 20s");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webrtc_builder_negotiates_outgoing_media_tracks() {
    ensure_crypto_provider();

    let fixture = paired_airc("webrtc-media-test").await;

    let (track_tx, track_rx) = std::sync::mpsc::channel();
    fixture
        .bob
        .set_incoming_track_handler(move |peer_id, track| {
            let _ = track_tx.send((peer_id, track));
        })
        .await
        .expect("handler registered");

    fixture
        .bob
        .accept_webrtc_offers()
        .await
        .expect("bob spawns webrtc responder");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let opened = fixture
        .alice
        .webrtc_connection(fixture.bob_spec.peer_id)
        .with_audio_track(OutgoingAudioTrack::new("avatar-voice", "avatar-stream"))
        .with_video_track(OutgoingVideoTrack::new("avatar-video", "avatar-stream"))
        .open()
        .await
        .expect("alice opens media webrtc to bob");
    assert_eq!(opened.outgoing_audio.len(), 1);
    assert_eq!(opened.outgoing_video.len(), 1);
    write_fixture_media_samples(&opened).await;

    let kinds = collect_track_kinds(track_rx, fixture.alice_spec.peer_id).await;
    assert_eq!(kinds, vec![RtpCodecKind::Audio, RtpCodecKind::Video]);

    let registered = fixture
        .bob
        .incoming_tracks_for_peer(fixture.alice_spec.peer_id)
        .await;
    assert_eq!(registered.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webrtc_session_carries_media_tracks_and_control_events() {
    ensure_crypto_provider();

    let fixture = paired_airc("continuum-avatar-session-fixture").await;
    let (track_tx, track_rx) = std::sync::mpsc::channel();
    fixture
        .bob
        .set_incoming_track_handler(move |peer_id, track| {
            let _ = track_tx.send((peer_id, track));
        })
        .await
        .expect("handler registered");
    fixture
        .bob
        .accept_webrtc_offers()
        .await
        .expect("bob spawns webrtc responder");
    tokio::time::sleep(Duration::from_millis(100)).await;

    let opened = fixture
        .alice
        .webrtc_connection(fixture.bob_spec.peer_id)
        .with_audio_track(OutgoingAudioTrack::new("avatar-voice", "avatar-stream"))
        .with_video_track(OutgoingVideoTrack::new("avatar-video", "avatar-stream"))
        .open()
        .await
        .expect("alice opens avatar webrtc session");
    wait_for_webrtc_channel(fixture.bob.clone(), fixture.alice_spec.peer_id).await;
    write_fixture_media_samples(&opened).await;
    let kinds = collect_track_kinds(track_rx, fixture.alice_spec.peer_id).await;
    assert_eq!(kinds, vec![RtpCodecKind::Audio, RtpCodecKind::Video]);

    force_webrtc_route(&fixture.alice);
    force_webrtc_route(&fixture.bob);

    let bob_handle = fixture.bob.clone();
    let bob_peer_id = fixture.bob.peer_id();
    let alice_peer_id = fixture.alice.peer_id();
    let receiver = tokio::spawn(async move {
        let mut stream = bob_handle.subscribe().await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                Ok(Some(Ok(event))) => {
                    if event.peer_id == bob_peer_id || event.peer_id != alice_peer_id {
                        continue;
                    }
                    if event
                        .headers
                        .get("continuum.activity")
                        .is_some_and(|activity| activity == "avatar-room-fixture")
                    {
                        return Some(event);
                    }
                }
                Ok(Some(Err(_))) => continue,
                Ok(None) => return None,
                Err(_) => continue,
            }
        }
        None
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut headers = Headers::new();
    headers.insert(
        "airc.command_kind".into(),
        "continuum.avatar.control".into(),
    );
    headers.insert("continuum.activity".into(), "avatar-room-fixture".into());
    fixture
        .alice
        .send_frame_to_for_test(
            FrameKind::Event,
            MentionTarget::Peer(fixture.bob_spec.peer_id),
            Body::text("avatar control ready"),
            headers,
        )
        .await
        .expect("alice sends control event over webrtc route");

    let event = receiver
        .await
        .expect("receiver task joined")
        .expect("bob received control event within deadline");
    assert_eq!(
        event.body.as_ref().and_then(Body::as_text),
        Some("avatar control ready")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn webrtc_orchestration_round_trip_over_mesh_signaling() {
    ensure_crypto_provider();

    let fixture = paired_airc("webrtc-orchestration-test").await;

    // Bob runs the responder before Alice initiates so the offer
    // handshake doesn't race the accept loop.
    fixture
        .bob
        .accept_webrtc_offers()
        .await
        .expect("bob spawns webrtc responder");

    // Brief settle so Bob's subscriber attaches.
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Alice initiates. Drives offer → gather → send → receive answer →
    // connected → DataChannel open → adapter registered.
    fixture
        .alice
        .open_webrtc_to(fixture.bob_spec.peer_id)
        .await
        .expect("alice opens webrtc to bob");

    // Wait until Bob has the adapter registered too — the accept loop
    // runs in a spawned task and completes asynchronously.
    wait_for_webrtc_channel(fixture.bob.clone(), fixture.alice_spec.peer_id).await;

    // Force route resolver to pick WebRTC — no other healthy route.
    force_webrtc_route(&fixture.alice);
    force_webrtc_route(&fixture.bob);

    let bob_handle = fixture.bob.clone();
    let bob_peer_id = fixture.bob.peer_id();
    let alice_peer_id = fixture.alice.peer_id();
    let receiver = tokio::spawn(async move {
        let mut stream = bob_handle.subscribe().await.unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while std::time::Instant::now() < deadline {
            match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
                Ok(Some(Ok(event))) => {
                    if event.peer_id == bob_peer_id {
                        continue;
                    }
                    if event.peer_id != alice_peer_id {
                        continue;
                    }
                    if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                        if text == "webrtc-control-ping" {
                            return Some(event);
                        }
                    }
                }
                Ok(Some(Err(_))) => continue,
                Ok(None) => return None,
                Err(_) => continue,
            }
        }
        None
    });

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut headers = Headers::new();
    headers.insert("airc.command_kind".into(), "test.webrtc.control".into());
    fixture
        .alice
        .send_frame_to_for_test(
            FrameKind::Event,
            MentionTarget::Peer(fixture.bob_spec.peer_id),
            Body::text("webrtc-control-ping"),
            headers,
        )
        .await
        .expect("alice sends event over webrtc route");

    let event = receiver
        .await
        .expect("receiver task joined")
        .expect("bob received webrtc-routed event within deadline");
    assert_eq!(
        event.body.as_ref().and_then(Body::as_text),
        Some("webrtc-control-ping")
    );
}
