use std::sync::Arc;
use std::time::{Duration, Instant};

use airc_core::{headers::Headers, transcript::MentionTarget, Body, ClientId, EventId, PeerId};
use airc_protocol::{ChannelId, Envelope, Frame, FrameKind, Signature, Subscription};
use futures::StreamExt;
use tokio::sync::mpsc;
use webrtc::data_channel::DataChannel;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState,
};
use webrtc::runtime::{channel, default_runtime, timeout as rtc_timeout, Sender};

use crate::transport::Transport;
use crate::webrtc_datachannel::{WebRtcDataChannelAdapter, WebRtcDataChannelError};

struct OffererHandler {
    gather_complete_tx: Sender<()>,
    connected_tx: Sender<()>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for OffererHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gather_complete_tx.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if state == RTCPeerConnectionState::Connected {
            let _ = self.connected_tx.try_send(());
        }
    }
}

struct AnswererHandler {
    gather_complete_tx: Sender<()>,
    connected_tx: Sender<()>,
    adapter_tx: mpsc::Sender<WebRtcDataChannelAdapter>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for AnswererHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gather_complete_tx.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if state == RTCPeerConnectionState::Connected {
            let _ = self.connected_tx.try_send(());
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let adapter = WebRtcDataChannelAdapter::new(channel);
        let _ = self.adapter_tx.send(adapter).await;
    }
}

fn event_frame(lamport: u64, body: &str) -> Frame {
    Frame {
        kind: FrameKind::Event,
        envelope: Envelope {
            event_id: EventId::from_u128(lamport as u128),
            sender: PeerId::from_u128(0xa1),
            sender_client: ClientId::from_u128(0xc1),
            channel: ChannelId::from_u128(0xc0ffee),
            target: MentionTarget::All,
            lamport,
            occurred_at_ms: 1_700_000_000_000 + lamport,
            reply_to: None,
            headers: Headers::new(),
            body: Some(Body::text(body)),
            media: Vec::new(),
            signature: Signature::Unsigned,
        },
    }
}

fn durable_frame() -> Frame {
    let mut frame = event_frame(99, "durable");
    frame.kind = FrameKind::Message;
    frame
}

async fn wait_open(adapter: &WebRtcDataChannelAdapter) {
    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if adapter.is_open() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("webrtc datachannel did not open within 10s");
}

async fn established_adapters() -> (
    WebRtcDataChannelAdapter,
    WebRtcDataChannelAdapter,
    Arc<dyn webrtc::peer_connection::PeerConnection>,
    Arc<dyn webrtc::peer_connection::PeerConnection>,
) {
    let runtime = default_runtime().expect("default webrtc runtime");

    let (offerer_gather_tx, mut offerer_gather_rx) = channel::<()>(1);
    let (offerer_connected_tx, mut offerer_connected_rx) = channel::<()>(1);
    let (answerer_gather_tx, mut answerer_gather_rx) = channel::<()>(1);
    let (answerer_connected_tx, mut answerer_connected_rx) = channel::<()>(1);
    let (answer_adapter_tx, mut answer_adapter_rx) = mpsc::channel(1);

    let offerer_pc = Arc::new(
        PeerConnectionBuilder::new()
            .with_handler(Arc::new(OffererHandler {
                gather_complete_tx: offerer_gather_tx,
                connected_tx: offerer_connected_tx,
            }))
            .with_runtime(runtime.clone())
            .with_udp_addrs(vec!["127.0.0.1:0"])
            .build()
            .await
            .expect("offerer peer connection"),
    );

    let offerer_channel = offerer_pc
        .create_data_channel("airc", None)
        .await
        .expect("offerer datachannel");
    let offerer_adapter = WebRtcDataChannelAdapter::new(offerer_channel);

    let offer = offerer_pc.create_offer(None).await.expect("offer");
    offerer_pc
        .set_local_description(offer)
        .await
        .expect("set offer local description");
    let _ = rtc_timeout(Duration::from_secs(5), offerer_gather_rx.recv()).await;
    let offer_sdp = offerer_pc
        .local_description()
        .await
        .expect("offerer local description");

    let answerer_pc = Arc::new(
        PeerConnectionBuilder::new()
            .with_handler(Arc::new(AnswererHandler {
                gather_complete_tx: answerer_gather_tx,
                connected_tx: answerer_connected_tx,
                adapter_tx: answer_adapter_tx,
            }))
            .with_runtime(runtime.clone())
            .with_udp_addrs(vec!["127.0.0.1:0"])
            .build()
            .await
            .expect("answerer peer connection"),
    );

    answerer_pc
        .set_remote_description(offer_sdp)
        .await
        .expect("answerer remote offer");
    let answer = answerer_pc.create_answer(None).await.expect("answer");
    answerer_pc
        .set_local_description(answer)
        .await
        .expect("answerer local description");
    let _ = rtc_timeout(Duration::from_secs(5), answerer_gather_rx.recv()).await;
    let answer_sdp = answerer_pc
        .local_description()
        .await
        .expect("answerer local description");

    offerer_pc
        .set_remote_description(answer_sdp)
        .await
        .expect("offerer remote answer");

    rtc_timeout(Duration::from_secs(15), offerer_connected_rx.recv())
        .await
        .expect("offerer connected timeout")
        .expect("offerer connected channel");
    rtc_timeout(Duration::from_secs(5), answerer_connected_rx.recv())
        .await
        .expect("answerer connected timeout")
        .expect("answerer connected channel");

    let answerer_adapter = tokio::time::timeout(Duration::from_secs(5), answer_adapter_rx.recv())
        .await
        .expect("answerer adapter timeout")
        .expect("answerer adapter");

    wait_open(&offerer_adapter).await;
    wait_open(&answerer_adapter).await;

    (offerer_adapter, answerer_adapter, offerer_pc, answerer_pc)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_rtc_datachannel_adapter_carries_airc_event_frames() {
    let (offerer_adapter, answerer_adapter, offerer_pc, answerer_pc) = established_adapters().await;
    let mut answerer_stream = answerer_adapter
        .subscribe(Subscription::default())
        .await
        .expect("subscribe answerer");

    let frame = event_frame(1, "hello through webrtc datachannel");
    offerer_adapter
        .send(frame.clone())
        .await
        .expect("send frame");

    let received = tokio::time::timeout(Duration::from_secs(5), answerer_stream.next())
        .await
        .expect("receive timeout")
        .expect("stream item")
        .expect("frame result");

    assert_eq!(received, frame);

    offerer_pc.close().await.expect("close offerer");
    answerer_pc.close().await.expect("close answerer");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_rtc_datachannel_rejects_durable_frames() {
    let (offerer_adapter, _answerer_adapter, offerer_pc, answerer_pc) =
        established_adapters().await;

    let error = offerer_adapter.send(durable_frame()).await.unwrap_err();
    assert!(matches!(
        error,
        WebRtcDataChannelError::UnsupportedDurableKind(FrameKind::Message)
    ));

    offerer_pc.close().await.expect("close offerer");
    answerer_pc.close().await.expect("close answerer");
}
