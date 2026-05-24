//! WebRTC media-track consumer surface.
//!
//! DataChannel routing lives in [`crate::webrtc`]. This module owns
//! the generic media-track surface above that connection lifecycle:
//! inbound track delivery and per-peer inspection. Outbound track
//! construction lives in a separate implementation lane.

use std::collections::HashMap;
use std::sync::Arc;

use airc_core::PeerId;
use tokio::sync::RwLock;
use webrtc::media_stream::track_remote::TrackRemote;

/// Remote media track delivered by WebRTC.
pub type IncomingTrack = Arc<dyn TrackRemote>;

/// Global inbound-track callback.
///
/// AIRC supplies the peer that negotiated the track and the
/// `webrtc-rs` remote track object. Consumers decide whether that
/// track feeds STT, avatar awareness, recording, or another domain
/// pipeline.
pub type IncomingTrackHandler = Arc<dyn Fn(PeerId, IncomingTrack) + Send + Sync + 'static>;

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
