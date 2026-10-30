//! The daemon's single transport handle feeds its bounded addressed switch.
use airc_daemon::DaemonState;
use airc_lib::addressed::{AddressedError, AddressedFrameSink};
use airc_protocol::Frame;
use std::sync::Arc;

pub(crate) struct DaemonAddressedSink(pub Arc<DaemonState>);

#[async_trait::async_trait]
impl AddressedFrameSink for DaemonAddressedSink {
    async fn deliver(&self, frame: Frame) -> Result<(), AddressedError> {
        self.0
            .receive_addressed_frame(frame)
            .await
            .map_err(|error| AddressedError::Transport(error.to_string()))
    }
}
