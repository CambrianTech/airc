use airc_protocol::FrameKind;

#[derive(Debug)]
pub enum WebRtcDataChannelError {
    Json(serde_json::Error),
    WebRtc(String),
    NotOpen,
    FrameTooLarge { actual: usize, limit: usize },
    UnsupportedDurableKind(FrameKind),
}

impl std::fmt::Display for WebRtcDataChannelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Json(error) => write!(f, "webrtc datachannel frame parse: {error}"),
            Self::WebRtc(error) => write!(f, "webrtc datachannel error: {error}"),
            Self::NotOpen => f.write_str("webrtc datachannel is not open"),
            Self::FrameTooLarge { actual, limit } => {
                write!(
                    f,
                    "webrtc datachannel frame size {actual} exceeds limit {limit}"
                )
            }
            Self::UnsupportedDurableKind(kind) => {
                write!(f, "webrtc datachannel route rejects durable kind {kind:?}")
            }
        }
    }
}

impl std::error::Error for WebRtcDataChannelError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Json(error) => Some(error),
            Self::WebRtc(_)
            | Self::NotOpen
            | Self::FrameTooLarge { .. }
            | Self::UnsupportedDurableKind(_) => None,
        }
    }
}

impl From<serde_json::Error> for WebRtcDataChannelError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}
