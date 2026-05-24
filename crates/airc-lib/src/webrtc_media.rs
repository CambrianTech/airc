//! WebRTC media-track consumer surface.
//!
//! DataChannel routing lives in [`crate::webrtc`]. This module owns
//! the generic media-track surface above that connection lifecycle:
//! inbound track delivery and per-peer inspection. Outbound track
//! construction lives in a separate implementation lane.

use std::collections::HashMap;
use std::sync::Arc;

use airc_core::PeerId;
use rtc::media_stream::MediaStreamTrack;
use rtc::peer_connection::configuration::media_engine::{MIME_TYPE_OPUS, MIME_TYPE_VP8};
use rtc::rtp_transceiver::rtp_sender::{
    RTCRtpCodec, RTCRtpCodingParameters, RTCRtpEncodingParameters, RtpCodecKind,
};
use tokio::sync::RwLock;
use uuid::Uuid;
use webrtc::media_stream::track_local::static_sample::TrackLocalStaticSample;
use webrtc::media_stream::track_remote::TrackRemote;

use crate::{Airc, AircError};

/// Remote media track delivered by WebRTC.
pub type IncomingTrack = Arc<dyn TrackRemote>;

/// Local media track created by AIRC and written by the consumer.
pub type OutgoingSampleTrack = Arc<TrackLocalStaticSample>;

/// Global inbound-track callback.
///
/// AIRC supplies the peer that negotiated the track and the
/// `webrtc-rs` remote track object. Consumers decide whether that
/// track feeds STT, avatar awareness, recording, or another domain
/// pipeline.
pub type IncomingTrackHandler = Arc<dyn Fn(PeerId, IncomingTrack) + Send + Sync + 'static>;

/// WebRTC media codec choices exposed by AIRC v1.
///
/// This is intentionally closed-set. New codecs should be explicit
/// enum variants so route policy, diagnostics, and tests can stay
/// exhaustive instead of accepting arbitrary MIME strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WebRtcMediaCodec {
    Opus,
    Vp8,
}

impl WebRtcMediaCodec {
    pub fn kind(self) -> RtpCodecKind {
        match self {
            Self::Opus => RtpCodecKind::Audio,
            Self::Vp8 => RtpCodecKind::Video,
        }
    }

    pub fn mime_type(self) -> &'static str {
        match self {
            Self::Opus => MIME_TYPE_OPUS,
            Self::Vp8 => MIME_TYPE_VP8,
        }
    }

    pub(crate) fn rtp_codec(self) -> RTCRtpCodec {
        match self {
            Self::Opus => RTCRtpCodec {
                mime_type: MIME_TYPE_OPUS.to_string(),
                clock_rate: 48_000,
                channels: 2,
                sdp_fmtp_line: "minptime=10;useinbandfec=1;stereo=1".to_string(),
                rtcp_feedback: Vec::new(),
            },
            Self::Vp8 => RTCRtpCodec {
                mime_type: MIME_TYPE_VP8.to_string(),
                clock_rate: 90_000,
                channels: 0,
                sdp_fmtp_line: String::new(),
                rtcp_feedback: Vec::new(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingAudioTrack {
    pub label: String,
    pub stream_id: String,
}

impl OutgoingAudioTrack {
    pub fn new(label: impl Into<String>, stream_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            stream_id: stream_id.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingVideoTrack {
    pub label: String,
    pub stream_id: String,
}

impl OutgoingVideoTrack {
    pub fn new(label: impl Into<String>, stream_id: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            stream_id: stream_id.into(),
        }
    }
}

/// Opened WebRTC connection with writable local media tracks.
#[derive(Default, Clone)]
pub struct OpenedWebRtcConnection {
    pub outgoing_audio: Vec<OutgoingSampleTrack>,
    pub outgoing_video: Vec<OutgoingSampleTrack>,
}

/// Builder for a peer WebRTC connection with optional pre-negotiated
/// media tracks.
#[derive(Clone)]
pub struct WebRtcConnectionBuilder {
    airc: Airc,
    target: PeerId,
    audio_tracks: Vec<OutgoingAudioTrack>,
    video_tracks: Vec<OutgoingVideoTrack>,
}

impl WebRtcConnectionBuilder {
    pub(crate) fn new(airc: Airc, target: PeerId) -> Self {
        Self {
            airc,
            target,
            audio_tracks: Vec::new(),
            video_tracks: Vec::new(),
        }
    }

    pub fn with_audio_track(mut self, track: OutgoingAudioTrack) -> Self {
        self.audio_tracks.push(track);
        self
    }

    pub fn with_video_track(mut self, track: OutgoingVideoTrack) -> Self {
        self.video_tracks.push(track);
        self
    }

    pub async fn open(self) -> Result<OpenedWebRtcConnection, AircError> {
        self.airc
            .open_webrtc_with_media(self.target, self.audio_tracks, self.video_tracks)
            .await
    }
}

#[derive(Default)]
pub(crate) struct PreparedOutgoingTracks {
    pub(crate) audio: Vec<OutgoingSampleTrack>,
    pub(crate) video: Vec<OutgoingSampleTrack>,
}

impl PreparedOutgoingTracks {
    pub(crate) fn from_configs(
        audio: Vec<OutgoingAudioTrack>,
        video: Vec<OutgoingVideoTrack>,
    ) -> Result<Self, AircError> {
        let mut prepared = Self::default();
        for track in audio {
            prepared.audio.push(build_track(
                track.stream_id,
                track.label,
                WebRtcMediaCodec::Opus,
            )?);
        }
        for track in video {
            prepared.video.push(build_track(
                track.stream_id,
                track.label,
                WebRtcMediaCodec::Vp8,
            )?);
        }
        Ok(prepared)
    }

    pub(crate) fn opened(&self) -> OpenedWebRtcConnection {
        OpenedWebRtcConnection {
            outgoing_audio: self.audio.clone(),
            outgoing_video: self.video.clone(),
        }
    }

    pub(crate) fn all_tracks(&self) -> impl Iterator<Item = OutgoingSampleTrack> + '_ {
        self.audio.iter().chain(self.video.iter()).cloned()
    }
}

fn build_track(
    stream_id: String,
    label: String,
    codec: WebRtcMediaCodec,
) -> Result<OutgoingSampleTrack, AircError> {
    let track_id = format!(
        "airc-{}-{}",
        codec.mime_type().replace('/', "-"),
        Uuid::new_v4()
    );
    let encoding = RTCRtpEncodingParameters {
        rtp_coding_parameters: RTCRtpCodingParameters {
            ssrc: Some(uuid_to_ssrc(Uuid::new_v4())),
            ..Default::default()
        },
        active: true,
        codec: codec.rtp_codec(),
        ..Default::default()
    };
    let media_track =
        MediaStreamTrack::new(stream_id, track_id, label, codec.kind(), vec![encoding]);
    TrackLocalStaticSample::new(media_track)
        .map(Arc::new)
        .map_err(|error| AircError::Transport(format!("webrtc media track: {error}")))
}

fn uuid_to_ssrc(uuid: Uuid) -> u32 {
    let bytes = uuid.as_bytes();
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// In-memory per-peer inbound media-track registry.
///
/// This is intentionally runtime state rather than event-store state:
/// WebRTC media tracks are live handles, not replayable transcript
/// events. Durable metadata can be added later as explicit typed
/// events if consumers need it.
#[derive(Clone, Default)]
pub(crate) struct IncomingTrackRegistry {
    tracks: Arc<RwLock<HashMap<PeerId, Vec<IncomingTrack>>>>,
}

impl IncomingTrackRegistry {
    pub(crate) async fn record(&self, peer_id: PeerId, track: IncomingTrack) {
        let mut guard = self.tracks.write().await;
        guard.entry(peer_id).or_default().push(track);
    }

    pub(crate) async fn tracks_for_peer(&self, peer_id: PeerId) -> Vec<IncomingTrack> {
        let guard = self.tracks.read().await;
        guard.get(&peer_id).cloned().unwrap_or_default()
    }
}
