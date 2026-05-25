use std::net::SocketAddr;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use airc_core::{Body, Headers, MentionTarget, PeerId};
use airc_lib::{
    Airc, PeerSpec, TransportHealthSample, TransportHealthState, TransportKind, TransportRole,
};
use airc_protocol::{
    PeerKeyRegistry, PeerKeypair, HEADER_AIRC_CORRELATION_ID, HEADER_AIRC_REPLY_TO,
};
use airc_relay::{RelayServer, RelayServerConfig};
use futures::StreamExt;
use serde::Serialize;
use uuid::Uuid;

use crate::route_cli::{RouteProofArgs, RouteProofKind};

pub async fn run(args: RouteProofArgs) -> Result<(), Box<dyn std::error::Error>> {
    let timeout = Duration::from_millis(args.timeout_ms);
    let report = match args.kind {
        RouteProofKind::LanLoopback => proof_lan_loopback(timeout).await?,
        RouteProofKind::RelayLoopback => proof_relay_loopback(timeout).await?,
    };
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(())
}

#[derive(Debug, Serialize)]
struct RouteProofReport {
    proof: &'static str,
    transport: &'static str,
    status: &'static str,
    github_routine_traffic: bool,
    alice_peer_id: String,
    bob_peer_id: String,
    relay_peer_id: Option<String>,
    relay_addr: Option<String>,
    correlation_id: String,
    reply_body: String,
    elapsed_ms: u128,
}

async fn proof_lan_loopback(
    timeout: Duration,
) -> Result<RouteProofReport, Box<dyn std::error::Error>> {
    let proof = ProofHomes::new("lan-loopback")?;
    let alice = Airc::open(&proof.alice_home).await?;
    let bob = Airc::open(&proof.bob_home).await?;
    enrol_pair(&alice, &bob).await?;

    alice.join("route-proof-lan").await?;
    bob.join("route-proof-lan").await?;

    let bob_addr = bob
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await?;
    alice.connect_lan(bob_addr, bob.peer_id()).await?;
    let handler = spawn_reply_handler(bob.clone(), "route.proof.lan.result");

    tokio::time::sleep(Duration::from_millis(50)).await;
    let start = Instant::now();
    let reply = issue_ping(&alice, timeout, "route.proof.lan.ping", "lan-ping").await?;
    join_reply_handler(handler).await?;

    Ok(RouteProofReport {
        proof: "lan-loopback",
        transport: "lan-tcp",
        status: "ok",
        github_routine_traffic: false,
        alice_peer_id: alice.peer_id().to_string(),
        bob_peer_id: bob.peer_id().to_string(),
        relay_peer_id: None,
        relay_addr: Some(bob_addr.to_string()),
        correlation_id: reply.correlation_id.to_string(),
        reply_body: reply.body,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

async fn proof_relay_loopback(
    timeout: Duration,
) -> Result<RouteProofReport, Box<dyn std::error::Error>> {
    ensure_crypto_provider();

    let proof = ProofHomes::new("relay-loopback")?;
    let alice = Airc::open(&proof.alice_home).await?;
    let bob = Airc::open(&proof.bob_home).await?;
    let (alice_spec, bob_spec) = enrol_pair(&alice, &bob).await?;

    let relay_peer = PeerId::new();
    let relay_keypair = PeerKeypair::generate();
    let relay_spec = PeerSpec {
        peer_id: relay_peer,
        pubkey: relay_keypair.public_bytes(),
    };
    alice.add_peer(relay_spec.clone()).await?;
    bob.add_peer(relay_spec).await?;

    alice.join("route-proof-relay").await?;
    bob.join("route-proof-relay").await?;

    let server_registry = std::sync::Arc::new(PeerKeyRegistry::new());
    server_registry.enrol(alice_spec.peer_id, 0, alice_spec.pubkey)?;
    server_registry.enrol(bob_spec.peer_id, 0, bob_spec.pubkey)?;
    let relay = RelayServer::start(RelayServerConfig {
        peer_id: relay_peer,
        keypair: relay_keypair,
        registry: server_registry,
        bind: SocketAddr::from(([127, 0, 0, 1], 0)),
    })
    .await?;
    let relay_addr = relay.local_addr();

    alice.connect_relay(relay_addr, relay_peer).await?;
    bob.connect_relay(relay_addr, relay_peer).await?;
    wait_for_relay_peers(&relay, &[alice.peer_id(), bob.peer_id()]).await?;

    let relay_only = [TransportHealthSample {
        kind: TransportKind::Relay,
        role: TransportRole::Relay,
        state: TransportHealthState::Healthy,
        rtt_ms: None,
        success_ppm: None,
    }];
    alice.replace_transport_health(relay_only)?;
    bob.replace_transport_health(relay_only)?;

    let handler = spawn_reply_handler(bob.clone(), "route.proof.relay.result");
    tokio::time::sleep(Duration::from_millis(50)).await;

    let start = Instant::now();
    let reply = issue_ping(&alice, timeout, "route.proof.relay.ping", "relay-ping").await?;
    join_reply_handler(handler).await?;
    relay.shutdown();

    Ok(RouteProofReport {
        proof: "relay-loopback",
        transport: "relay",
        status: "ok",
        github_routine_traffic: false,
        alice_peer_id: alice.peer_id().to_string(),
        bob_peer_id: bob.peer_id().to_string(),
        relay_peer_id: Some(relay_peer.to_string()),
        relay_addr: Some(relay_addr.to_string()),
        correlation_id: reply.correlation_id.to_string(),
        reply_body: reply.body,
        elapsed_ms: start.elapsed().as_millis(),
    })
}

struct ProofHomes {
    root: PathBuf,
    alice_home: PathBuf,
    bob_home: PathBuf,
}

impl ProofHomes {
    fn new(name: &str) -> Result<Self, std::io::Error> {
        let root = std::env::temp_dir().join(format!("airc-route-proof-{name}-{}", Uuid::new_v4()));
        let alice_home = root.join("alice/.airc");
        let bob_home = root.join("bob/.airc");
        std::fs::create_dir_all(&alice_home)?;
        std::fs::create_dir_all(&bob_home)?;
        Ok(Self {
            root,
            alice_home,
            bob_home,
        })
    }
}

impl Drop for ProofHomes {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct ProofReply {
    correlation_id: Uuid,
    body: String,
}

async fn enrol_pair(
    alice: &Airc,
    bob: &Airc,
) -> Result<(PeerSpec, PeerSpec), Box<dyn std::error::Error>> {
    let alice_spec = alice.peer_spec().parse::<PeerSpec>()?;
    let bob_spec = bob.peer_spec().parse::<PeerSpec>()?;
    alice.add_peer(bob_spec.clone()).await?;
    bob.add_peer(alice_spec.clone()).await?;
    Ok((alice_spec, bob_spec))
}

fn spawn_reply_handler(
    responder: Airc,
    result_hint: &'static str,
) -> tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>> {
    tokio::spawn(async move {
        let mut stream = responder.subscribe().await?;
        while let Some(next) = stream.next().await {
            let event = next?;
            if event.peer_id == responder.peer_id() {
                continue;
            }
            let Some(correlation) = event.headers.get(HEADER_AIRC_CORRELATION_ID) else {
                continue;
            };
            let Some(reply_to) = event.headers.get(HEADER_AIRC_REPLY_TO) else {
                continue;
            };
            let correlation_id = Uuid::parse_str(correlation)?;
            let reply_to_peer = PeerId::from_uuid(Uuid::parse_str(reply_to)?);
            let mut headers = Headers::new();
            headers.insert("forge.body_hint".into(), result_hint.into());
            responder
                .reply(
                    reply_to_peer,
                    correlation_id,
                    headers,
                    Body::text("route-proof-pong"),
                )
                .await?;
            return Ok(());
        }
        Err("reply handler stream ended before request arrived".into())
    })
}

async fn join_reply_handler(
    handler: tokio::task::JoinHandle<Result<(), Box<dyn std::error::Error + Send + Sync>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    handler
        .await
        .map_err(|error| format!("reply handler join failed: {error}"))?
        .map_err(|error| error.to_string().into())
}

async fn issue_ping(
    requester: &Airc,
    timeout: Duration,
    command_kind: &str,
    body: &str,
) -> Result<ProofReply, Box<dyn std::error::Error>> {
    let mut headers = Headers::new();
    headers.insert("airc.command_kind".into(), command_kind.into());
    let pending = requester
        .request(MentionTarget::All, headers, Body::text(body), timeout)
        .await?;
    let correlation_id = pending.correlation_id;
    let reply = requester.await_reply(pending).await?;
    let body = reply
        .body
        .as_ref()
        .and_then(Body::as_text)
        .unwrap_or("")
        .to_string();
    Ok(ProofReply {
        correlation_id,
        body,
    })
}

async fn wait_for_relay_peers(
    relay: &RelayServer,
    expected: &[PeerId],
) -> Result<(), Box<dyn std::error::Error>> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let connected = relay.connected_peers().await;
        if expected.iter().all(|peer| connected.contains(peer)) {
            return Ok(());
        }
        if Instant::now() > deadline {
            return Err(format!(
                "timeout waiting for relay peers; expected {expected:?}, connected {connected:?}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn ensure_crypto_provider() {
    let _ = rustls::crypto::ring::default_provider().install_default();
}
