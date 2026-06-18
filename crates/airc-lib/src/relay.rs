//! Relay transport binding for embedded AIRC handles.
//!
//! Relay is the cross-boundary live route for peers that cannot reach
//! each other directly. The SDK owns adapter lifecycle, route health,
//! and frame ingestion; consumers only provide the relay endpoint and
//! pinned relay identity.

use std::net::SocketAddr;

use airc_core::PeerId;
use airc_relay::{RelayServer, RelayServerConfig};
use airc_transport::relay::{RelayAdapter, RelayClientConfig};

use crate::error::AircError;
use crate::route::{
    RouteEndpoint, TransportHealthSample, TransportHealthState, TransportKind, TransportRole,
};
use crate::Airc;

impl Airc {
    /// Connect this handle to a pinned relay and make the relay route
    /// available for subsequent sends when route health selects it.
    pub async fn connect_relay(
        &self,
        relay_addr: SocketAddr,
        relay_peer: PeerId,
    ) -> Result<(), AircError> {
        {
            let guard = self.inner.relay.lock().await;
            if guard.is_some() {
                drop(guard);
                self.ensure_relay_subscriber().await?;
                self.upsert_relay_health(relay_addr, relay_peer)?;
                return Ok(());
            }
        }

        let adapter = RelayAdapter::new(RelayClientConfig {
            self_peer_id: self.inner.identity.peer_id,
            self_keypair: self.inner.identity.keypair.clone(),
            relay_peer_id: relay_peer,
            relay_addr,
            registry: self.inner.registry.clone(),
        });
        adapter
            .connect()
            .await
            .map_err(|error| AircError::Transport(error.to_string()))?;
        {
            let mut guard = self.inner.relay.lock().await;
            if guard.is_none() {
                *guard = Some(adapter.clone());
            }
        }
        self.ensure_relay_subscriber().await?;
        self.upsert_relay_health(relay_addr, relay_peer)?;
        Ok(())
    }

    /// Promote this node to ALSO be a relay: bind a relay listener on
    /// `bind_addr` (port 0 = OS-assigned) serving this node's enrolled
    /// peers, and advertise the relay endpoint (peer-id-bearing, so it's
    /// pinnable) on this node's route table — which flows into the gist
    /// directory, so peers that import this node's card connect through it
    /// (discovery → [`Airc::connect_relay`], #1247 slice 2).
    ///
    /// This is the "be a relay" half of self-election (#1247 slice 4): a
    /// node that can bind a reachable listener can host the relay the
    /// cross-subnet mesh needs. Reachability is EMPIRICAL — promoting
    /// yourself just binds + advertises; whether peers actually reach you
    /// decides whether your relay matters (an unreachable self-elected
    /// relay accumulates no connections and its gist entry goes stale).
    /// The election TRIGGER (when to call this) lives in the daemon's
    /// route-refresh loop; this is the mechanism it invokes.
    ///
    /// Idempotent: a node already relaying re-advertises and returns its
    /// existing bound address.
    pub async fn become_relay(&self, bind_addr: SocketAddr) -> Result<SocketAddr, AircError> {
        // Fast path: already relaying — re-advertise (idempotent) + return.
        {
            let guard = self.inner.relay_server.lock().await;
            if let Some(server) = guard.as_ref() {
                let addr = server.local_addr();
                drop(guard);
                self.advertise_self_relay(addr)?;
                return Ok(addr);
            }
        }
        // Bind a fresh relay server OUTSIDE the lock (no await under the
        // guard), serving exactly this node's enrolled peers.
        let server = RelayServer::start(RelayServerConfig {
            peer_id: self.inner.identity.peer_id,
            keypair: self.inner.identity.keypair.clone(),
            registry: self.inner.registry.clone(),
            bind: bind_addr,
        })
        .await
        .map_err(|error| AircError::Transport(error.to_string()))?;
        let addr = server.local_addr();
        // Install — unless a concurrent caller won the race, in which case
        // shut ours down and adopt the winner's address (one relay/node).
        let final_addr = {
            let mut guard = self.inner.relay_server.lock().await;
            match guard.as_ref() {
                Some(existing) => {
                    let existing_addr = existing.local_addr();
                    server.shutdown();
                    existing_addr
                }
                None => {
                    *guard = Some(server);
                    addr
                }
            }
        };
        self.advertise_self_relay(final_addr)?;
        Ok(final_addr)
    }

    /// Record THIS node's own relay endpoint (peer-id-bearing) on the
    /// route table so it propagates into the gist directory. Distinct from
    /// `upsert_relay_health`, which marks a relay this node is a CLIENT of.
    fn advertise_self_relay(&self, addr: SocketAddr) -> Result<(), AircError> {
        self.upsert_route_endpoint(RouteEndpoint::relay(self.inner.identity.peer_id, addr))?;
        Ok(())
    }

    fn upsert_relay_health(
        &self,
        relay_addr: SocketAddr,
        relay_peer: PeerId,
    ) -> Result<(), AircError> {
        self.upsert_transport_health(TransportHealthSample {
            kind: TransportKind::Relay,
            role: TransportRole::Relay,
            state: TransportHealthState::Healthy,
            rtt_ms: None,
            success_ppm: None,
        })?;
        // Record the relay endpoint with its peer id baked into the URL
        // (`airc-relay://<peer>@<addr>`) so when this is advertised through
        // the gist rendezvous, an importing peer can both dial AND pin the
        // relay (mTLS) with no out-of-band credential exchange — the
        // self-electing-relay model (#1247). A bare `airc-relay://<addr>`
        // would be advertised un-connectable.
        self.upsert_route_endpoint(RouteEndpoint::relay(relay_peer, relay_addr))?;
        Ok(())
    }

    pub(crate) async fn relay_adapter(&self) -> Result<RelayAdapter, AircError> {
        let guard = self.inner.relay.lock().await;
        guard
            .as_ref()
            .cloned()
            .ok_or_else(|| AircError::Transport("relay adapter is not connected".into()))
    }
}
