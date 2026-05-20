//! Smoke-test runner for the relay server.
//!
//! Generates an ephemeral identity and runs with an empty registry —
//! useful only for confirming the crate compiles + binds. Real
//! deployments embed [`airc_relay::RelayServer`] from a parent process
//! that owns persistent identity and a populated `PeerKeyRegistry`.

use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

use airc_core::PeerId;
use airc_protocol::{PeerKeyRegistry, PeerKeypair};
use airc_relay::{RelayServer, RelayServerConfig};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let bind: SocketAddr = std::env::var("AIRC_RELAY_BIND")
        .unwrap_or_else(|_| "127.0.0.1:0".to_string())
        .parse()?;

    let peer_id = PeerId::new();
    let keypair = PeerKeypair::generate();
    let registry = Arc::new(RwLock::new(PeerKeyRegistry::new()));

    let server = RelayServer::start(RelayServerConfig {
        peer_id,
        keypair,
        registry,
        bind,
    })
    .await?;

    eprintln!("airc-relay listening on {}", server.local_addr());
    eprintln!("(ephemeral identity, empty registry — embed the library for real use)");

    tokio::signal::ctrl_c().await?;
    server.shutdown();
    Ok(())
}
