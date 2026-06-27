use std::collections::HashMap;
use std::net::SocketAddr;

use airc_core::PeerId;

/// UDP adapter configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdpConfig {
    /// Local bind address. Use port 0 in tests or embedded callers
    /// that want the OS to allocate an ephemeral port.
    pub bind_addr: SocketAddr,
    /// Known peer endpoints. Broadcast sends go to all endpoints;
    /// direct-peer sends require the target peer to be present here.
    pub peer_endpoints: HashMap<PeerId, SocketAddr>,
}

impl UdpConfig {
    pub fn new(bind_addr: SocketAddr) -> Self {
        Self {
            bind_addr,
            peer_endpoints: HashMap::new(),
        }
    }

    pub fn with_peer(mut self, peer_id: PeerId, addr: SocketAddr) -> Self {
        self.peer_endpoints.insert(peer_id, addr);
        self
    }
}
