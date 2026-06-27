//! WebRTC datachannel transport adapter.
//!
//! WebRTC signaling travels as signed AIRC events using
//! `airc_protocol::WebRtcSignal`. Once the selected signaling route
//! has established a datachannel, this adapter carries realtime AIRC
//! event frames over that channel. Audio/video media is out of scope.

mod adapter;
mod error;

pub use adapter::WebRtcDataChannelAdapter;
pub use error::WebRtcDataChannelError;

#[cfg(test)]
mod tests;
