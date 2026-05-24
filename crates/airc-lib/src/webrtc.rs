//! WebRTC DataChannel orchestration over the AIRC mesh.
//!
//! Wires the existing [`airc_transport::webrtc_datachannel::WebRtcDataChannelAdapter`]
//! into a complete per-peer DataChannel lifecycle:
//!
//! - [`Airc::open_webrtc_to`] is the initiator. Creates an
//!   `RTCPeerConnection`, opens a DataChannel, sends an SDP offer to
//!   `peer_id` over the AIRC mesh (as a [`SignalingMessage::Offer`]
//!   event), waits for the matching [`SignalingMessage::Answer`],
//!   completes the handshake, and registers the resulting adapter in
//!   the per-peer table so [`TransportKind::WebRtcDataChannel`]
//!   route execution can dispatch sends to it.
//! - [`Airc::accept_webrtc_offers`] is the responder side. Spawns a
//!   long-running task that subscribes to the substrate stream,
//!   filters for incoming `Offer` messages, and answers them with
//!   the same handshake flow in reverse.
//!
//! Uses the **gather-complete** approach (not trickle ICE): each side
//! waits for ICE candidate gathering to finish before sending its
//! local description, so the offer/answer SDPs already include all
//! candidates. This is exactly the pattern
//! `webrtc_datachannel/tests.rs::established_adapters` uses. Trickle
//! ICE, STUN/TURN configuration, reconnect-on-drop, and renegotiation
//! are explicit non-goals here.
//!
//! Loopback bind: PCs gather candidates on `127.0.0.1:0`, which means
//! this PR's path only works between two endpoints on the same
//! machine. Real-network NAT traversal needs configurable ICE servers
//! — flagged as a follow-up.

use std::sync::Arc;
use std::time::Duration;

use airc_core::headers::Headers;
use airc_core::{Body, PeerId, TranscriptEvent};
use airc_protocol::FrameKind;
use airc_transport::webrtc_datachannel::WebRtcDataChannelAdapter;
use futures::StreamExt;
use tokio::sync::mpsc;
use uuid::Uuid;
use webrtc::data_channel::DataChannel;
use webrtc::peer_connection::{
    PeerConnection, PeerConnectionBuilder, PeerConnectionEventHandler, RTCIceGatheringState,
    RTCPeerConnectionState, RTCSdpType, RTCSessionDescription,
};
use webrtc::runtime::{channel as rtc_channel, default_runtime, Sender as RtcSender};

use crate::error::AircError;
use crate::webrtc_signaling::{
    SignalingMessage, HEADER_WEBRTC_SIGNALING_KIND, HEADER_WEBRTC_SIGNALING_SESSION_ID,
};
use crate::Airc;

const HANDSHAKE_GATHER_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_CONNECT_TIMEOUT: Duration = Duration::from_secs(15);

impl Airc {
    /// Initiate a WebRTC DataChannel to `peer_id`. Drives the full
    /// offer/answer handshake over the AIRC mesh, registers the
    /// resulting adapter for [`TransportKind::WebRtcDataChannel`]
    /// route execution, and ensures the per-peer ingest subscriber.
    ///
    /// Blocks until the DataChannel is open or the handshake times
    /// out. Concurrent calls for the same `peer_id` short-circuit if
    /// an adapter is already registered.
    pub async fn open_webrtc_to(&self, peer_id: PeerId) -> Result<(), AircError> {
        {
            let guard = self.inner.webrtc_channels.lock().await;
            if guard.contains_key(&peer_id) {
                drop(guard);
                self.ensure_webrtc_subscriber(peer_id).await?;
                return Ok(());
            }
        }

        let session_id = Uuid::new_v4();
        let runtime = default_runtime()
            .ok_or_else(|| AircError::Transport("webrtc default runtime unavailable".into()))?;
        let (gather_tx, mut gather_rx) = rtc_channel::<()>(1);
        let (connected_tx, mut connected_rx) = rtc_channel::<()>(1);
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_handler(Arc::new(OffererHandler {
                    gather_tx,
                    connected_tx,
                }))
                .with_runtime(runtime)
                .with_udp_addrs(vec!["127.0.0.1:0"])
                .build()
                .await
                .map_err(|error| AircError::Transport(format!("webrtc build: {error}")))?,
        );

        let channel = pc
            .create_data_channel("airc", None)
            .await
            .map_err(|error| AircError::Transport(format!("webrtc data_channel: {error}")))?;

        let offer = pc
            .create_offer(None)
            .await
            .map_err(|error| AircError::Transport(format!("webrtc create_offer: {error}")))?;
        pc.set_local_description(offer).await.map_err(|error| {
            AircError::Transport(format!("webrtc set_local_description(offer): {error}"))
        })?;

        // Wait for ICE gathering to complete so the local description
        // includes all candidates (no trickle ICE in this skeleton).
        if webrtc::runtime::timeout(HANDSHAKE_GATHER_TIMEOUT, gather_rx.recv())
            .await
            .is_err()
        {
            return Err(AircError::Transport(
                "webrtc ICE gather timeout (offerer)".to_string(),
            ));
        }
        let local_desc = pc
            .local_description()
            .await
            .ok_or_else(|| AircError::Transport("webrtc local_description missing".into()))?;

        // Subscribe BEFORE sending the offer so we don't miss the
        // answer if it lands fast.
        let mut stream = self.subscribe().await?;
        self.send_signaling(
            peer_id,
            SignalingMessage::Offer {
                session_id,
                sdp: local_desc.sdp.clone(),
            },
        )
        .await?;

        // Wait for the answer matching this session_id.
        let answer_sdp = await_signaling_answer(&mut stream, session_id, peer_id).await?;
        let mut answer = RTCSessionDescription::default();
        answer.sdp_type = RTCSdpType::Answer;
        answer.sdp = answer_sdp;
        pc.set_remote_description(answer).await.map_err(|error| {
            AircError::Transport(format!("webrtc set_remote_description(answer): {error}"))
        })?;

        if webrtc::runtime::timeout(HANDSHAKE_CONNECT_TIMEOUT, connected_rx.recv())
            .await
            .is_err()
        {
            return Err(AircError::Transport(
                "webrtc connect timeout (offerer)".to_string(),
            ));
        }

        let adapter = WebRtcDataChannelAdapter::new(channel);
        wait_for_adapter_open(&adapter).await?;
        self.register_webrtc_adapter(peer_id, adapter, pc).await?;
        self.ensure_webrtc_subscriber(peer_id).await?;
        Ok(())
    }

    /// Spawn a long-running responder task that listens for incoming
    /// `SignalingMessage::Offer` events on the substrate and answers
    /// them with the matching `Answer`. Returns once the task is
    /// spawned — the task itself runs until the `Airc` handle is
    /// dropped or its subscriber stream ends.
    pub async fn accept_webrtc_offers(&self) -> Result<(), AircError> {
        let mut stream = self.subscribe().await?;
        let airc = self.clone();
        tokio::spawn(async move {
            while let Some(item) = stream.next().await {
                let Ok(event) = item else {
                    continue;
                };
                let Some(message) = parse_signaling_event(&event) else {
                    continue;
                };
                let SignalingMessage::Offer { session_id, sdp } = message else {
                    continue;
                };
                // Skip our own broadcast echoes.
                if event.peer_id == airc.peer_id() {
                    continue;
                }
                let initiator = event.peer_id;
                if let Err(error) = airc.answer_offer(initiator, session_id, sdp).await {
                    eprintln!("webrtc accept_offer failed for {initiator}: {error}");
                }
            }
        });
        Ok(())
    }

    async fn answer_offer(
        &self,
        initiator: PeerId,
        session_id: Uuid,
        offer_sdp: String,
    ) -> Result<(), AircError> {
        {
            let guard = self.inner.webrtc_channels.lock().await;
            if guard.contains_key(&initiator) {
                // Already have a channel — duplicate offer, ignore.
                return Ok(());
            }
        }

        let runtime = default_runtime()
            .ok_or_else(|| AircError::Transport("webrtc default runtime unavailable".into()))?;
        let (gather_tx, mut gather_rx) = rtc_channel::<()>(1);
        let (connected_tx, mut connected_rx) = rtc_channel::<()>(1);
        let (channel_tx, mut channel_rx) = mpsc::channel::<Arc<dyn DataChannel>>(1);
        let pc: Arc<dyn PeerConnection> = Arc::new(
            PeerConnectionBuilder::new()
                .with_handler(Arc::new(AnswererHandler {
                    gather_tx,
                    connected_tx,
                    channel_tx,
                }))
                .with_runtime(runtime)
                .with_udp_addrs(vec!["127.0.0.1:0"])
                .build()
                .await
                .map_err(|error| AircError::Transport(format!("webrtc build: {error}")))?,
        );

        let mut offer = RTCSessionDescription::default();
        offer.sdp_type = RTCSdpType::Offer;
        offer.sdp = offer_sdp;
        pc.set_remote_description(offer).await.map_err(|error| {
            AircError::Transport(format!("webrtc set_remote_description(offer): {error}"))
        })?;

        let answer = pc
            .create_answer(None)
            .await
            .map_err(|error| AircError::Transport(format!("webrtc create_answer: {error}")))?;
        pc.set_local_description(answer).await.map_err(|error| {
            AircError::Transport(format!("webrtc set_local_description(answer): {error}"))
        })?;

        if webrtc::runtime::timeout(HANDSHAKE_GATHER_TIMEOUT, gather_rx.recv())
            .await
            .is_err()
        {
            return Err(AircError::Transport(
                "webrtc ICE gather timeout (answerer)".to_string(),
            ));
        }
        let local_desc = pc
            .local_description()
            .await
            .ok_or_else(|| AircError::Transport("webrtc local_description missing".into()))?;

        self.send_signaling(
            initiator,
            SignalingMessage::Answer {
                session_id,
                sdp: local_desc.sdp.clone(),
            },
        )
        .await?;

        if webrtc::runtime::timeout(HANDSHAKE_CONNECT_TIMEOUT, connected_rx.recv())
            .await
            .is_err()
        {
            return Err(AircError::Transport(
                "webrtc connect timeout (answerer)".to_string(),
            ));
        }

        let channel = tokio::time::timeout(Duration::from_secs(5), channel_rx.recv())
            .await
            .map_err(|_| AircError::Transport("webrtc data_channel callback timeout".into()))?
            .ok_or_else(|| AircError::Transport("webrtc data_channel channel closed".into()))?;

        let adapter = WebRtcDataChannelAdapter::new(channel);
        wait_for_adapter_open(&adapter).await?;
        self.register_webrtc_adapter(initiator, adapter, pc).await?;
        self.ensure_webrtc_subscriber(initiator).await?;
        Ok(())
    }

    async fn send_signaling(
        &self,
        target: PeerId,
        message: SignalingMessage,
    ) -> Result<(), AircError> {
        let kind = message.kind_str().to_string();
        let session_id = message.session_id().to_string();
        let body = serde_json::to_value(&message)
            .map_err(|error| AircError::Transport(format!("webrtc signaling encode: {error}")))?;
        let mut headers = Headers::new();
        headers.insert(HEADER_WEBRTC_SIGNALING_KIND.into(), kind);
        headers.insert(HEADER_WEBRTC_SIGNALING_SESSION_ID.into(), session_id);
        self.send_frame_to(
            FrameKind::Event,
            airc_core::MentionTarget::Peer(target),
            Body::Json(body),
            headers,
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn webrtc_adapter_for(
        &self,
        peer_id: PeerId,
    ) -> Result<WebRtcDataChannelAdapter, AircError> {
        let guard = self.inner.webrtc_channels.lock().await;
        guard
            .get(&peer_id)
            .cloned()
            .ok_or_else(|| AircError::Transport(format!("no webrtc channel for peer {peer_id}")))
    }

    /// Whether a WebRTC DataChannel is currently registered for
    /// `peer_id`. Public so consumers (and integration tests) can
    /// gate operations on the channel being live without holding the
    /// internal lock.
    pub async fn has_webrtc_channel(&self, peer_id: PeerId) -> bool {
        let guard = self.inner.webrtc_channels.lock().await;
        guard.contains_key(&peer_id)
    }

    async fn register_webrtc_adapter(
        &self,
        peer_id: PeerId,
        adapter: WebRtcDataChannelAdapter,
        pc: Arc<dyn PeerConnection>,
    ) -> Result<(), AircError> {
        let mut guard = self.inner.webrtc_channels.lock().await;
        guard.insert(peer_id, adapter);
        let mut pc_guard = self.inner.webrtc_peer_connections.lock().await;
        pc_guard.insert(peer_id, pc);
        Ok(())
    }
}

async fn await_signaling_answer(
    stream: &mut crate::stream::EventStream,
    session_id: Uuid,
    expected_peer: PeerId,
) -> Result<String, AircError> {
    let deadline = std::time::Instant::now() + HANDSHAKE_CONNECT_TIMEOUT;
    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let next = tokio::time::timeout(remaining, stream.next()).await;
        let Ok(Some(Ok(event))) = next else {
            continue;
        };
        if event.peer_id != expected_peer {
            continue;
        }
        let Some(message) = parse_signaling_event(&event) else {
            continue;
        };
        if message.session_id() != session_id {
            continue;
        }
        if let SignalingMessage::Answer { sdp, .. } = message {
            return Ok(sdp);
        }
    }
    Err(AircError::Transport("webrtc answer timeout".to_string()))
}

fn parse_signaling_event(event: &TranscriptEvent) -> Option<SignalingMessage> {
    let _ = event.headers.get(HEADER_WEBRTC_SIGNALING_KIND)?;
    let body = event.body.as_ref()?;
    let value = match body {
        Body::Json(value) => value.clone(),
        _ => return None,
    };
    serde_json::from_value(value).ok()
}

async fn wait_for_adapter_open(adapter: &WebRtcDataChannelAdapter) -> Result<(), AircError> {
    let deadline = std::time::Instant::now() + HANDSHAKE_CONNECT_TIMEOUT;
    while std::time::Instant::now() < deadline {
        if adapter.is_open() {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    Err(AircError::Transport(
        "webrtc adapter did not open within deadline".to_string(),
    ))
}

struct OffererHandler {
    gather_tx: RtcSender<()>,
    connected_tx: RtcSender<()>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for OffererHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gather_tx.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if state == RTCPeerConnectionState::Connected {
            let _ = self.connected_tx.try_send(());
        }
    }
}

struct AnswererHandler {
    gather_tx: RtcSender<()>,
    connected_tx: RtcSender<()>,
    channel_tx: mpsc::Sender<Arc<dyn DataChannel>>,
}

#[async_trait::async_trait]
impl PeerConnectionEventHandler for AnswererHandler {
    async fn on_ice_gathering_state_change(&self, state: RTCIceGatheringState) {
        if state == RTCIceGatheringState::Complete {
            let _ = self.gather_tx.try_send(());
        }
    }

    async fn on_connection_state_change(&self, state: RTCPeerConnectionState) {
        if state == RTCPeerConnectionState::Connected {
            let _ = self.connected_tx.try_send(());
        }
    }

    async fn on_data_channel(&self, channel: Arc<dyn DataChannel>) {
        let _ = self.channel_tx.send(channel).await;
    }
}
