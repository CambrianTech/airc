//! Embedding smoke test — proves a small consumer can link the lib
//! and exercise the Gate-4 minimum: open identity, join a room,
//! send, observe in store via `page_recent`, subscribe to live
//! events, fetch replay via `resume_from`, peer-spec/peers handling.
//!
//! No daemon involvement; the lib's in-process embedding owns the
//! background subscriber, the store, and the broadcast fan-out.
//! This is the slice-6b proof point.

use std::{net::SocketAddr, time::Duration};

use airc_daemon::{DaemonState, LocalIdentity};
use airc_lib::{
    coordinator_snapshot, resolve_mesh_identity_with, subscriptions, Airc, Body, ChannelName,
    CoordinatorConfig, EventFilter, HeaderFilter, Headers, MeshIdentity, MeshIdentitySource,
    PeerSpec, RouteEndpoint, SubscriptionSet, TranscriptKind, TransportHealthSample, TransportKind,
    TransportRole,
};
use airc_protocol::{PeerKeyRegistry, VerificationPolicy};
use airc_store::{EventStore, SqliteEventStore};
use futures::stream::StreamExt;
use tempfile::TempDir;

static HOME_ENV_LOCK: std::sync::LazyLock<std::sync::Mutex<()>> =
    std::sync::LazyLock::new(|| std::sync::Mutex::new(()));

/// Poll `page_recent` until it sees at least `expected` events or
/// the deadline fires. The wire-side tail loop runs in a background
/// task; first attaches replay from the start of the wire, but the
/// store append happens asynchronously after the local-fs adapter
/// observes the new line. A handful of polls keeps the test
/// deterministic without flaking on slow CI runners.
async fn wait_for_events(
    airc: &Airc,
    expected: usize,
    timeout: Duration,
) -> Vec<airc_lib::TranscriptEvent> {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let page = airc.page_recent(64).await.unwrap();
        if page.len() >= expected {
            return page;
        }
        if std::time::Instant::now() >= deadline {
            panic!(
                "wait_for_events: expected {expected} got {} in {:?}",
                page.len(),
                timeout
            );
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn wait_for_text(airc: &Airc, text: &str, timeout: Duration) -> airc_lib::TranscriptEvent {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        let page = airc.page_recent(64).await.unwrap();
        if let Some(event) = page
            .into_iter()
            .find(|event| event.body.as_ref().and_then(Body::as_text) == Some(text))
        {
            return event;
        }
        if std::time::Instant::now() >= deadline {
            panic!("wait_for_text: did not see {text:?} in {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn unique_socket() -> std::path::PathBuf {
    let suffix = uuid::Uuid::new_v4().as_u128() as u32;
    std::path::PathBuf::from(format!("/tmp/airclib{:x}.sock", suffix))
}

async fn spawn_daemon(home: &std::path::Path) -> (tokio::task::JoinHandle<()>, std::path::PathBuf) {
    let identity = LocalIdentity::load_or_generate(home).unwrap();
    let mut registry = PeerKeyRegistry::new();
    registry
        .enrol(identity.peer_id, 0, identity.keypair.public_bytes())
        .unwrap();
    let store: std::sync::Arc<dyn EventStore> = std::sync::Arc::new(
        SqliteEventStore::open_path(&home.join("events.sqlite"))
            .await
            .unwrap(),
    );
    let state = std::sync::Arc::new(DaemonState::new(
        identity.peer_id,
        identity.keypair,
        std::sync::Arc::new(std::sync::RwLock::new(registry)),
        VerificationPolicy::Strict,
        home.to_path_buf(),
        store,
    ));
    let socket = unique_socket();
    let server_socket = socket.clone();
    let handle = tokio::spawn(async move {
        airc_daemon::run(state, server_socket).await.unwrap();
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    (handle, socket)
}

#[tokio::test]
async fn open_join_say_and_replay_round_trips_in_process() {
    // Gate-4 minimum: a consumer can link airc-lib, open the substrate,
    // join a room, send, and observe the event via the store — without
    // manually feeding the store. Before slice 6b this test had to
    // call `airc.append_event(...)` because `say` only wrote to the
    // wire; the background subscriber now closes that loop.
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    let room = airc.join("project-x").await.unwrap();
    assert_eq!(room.name, "project-x");
    let current = airc.current_room().await.unwrap();
    assert_eq!(
        current.channel, room.channel,
        "join persisted to the subscription set"
    );

    let _event_id = airc.say("hello, consumer").await.unwrap();

    let page = wait_for_events(&airc, 1, Duration::from_secs(2)).await;
    assert_eq!(page.len(), 1);
    let bodies: Vec<&str> = page
        .iter()
        .filter_map(|e| e.body.as_ref().and_then(Body::as_text))
        .collect();
    assert_eq!(bodies, vec!["hello, consumer"]);

    let cursor = airc.latest_cursor().await.unwrap().unwrap();
    let after = airc.resume_from(&cursor, 10).await.unwrap();
    assert!(after.is_empty(), "nothing strictly after the latest cursor");
}

#[tokio::test]
async fn attached_sdk_sends_and_pages_through_daemon() {
    let home = TempDir::new().unwrap();
    let setup = Airc::open(home.path()).await.unwrap();
    setup.join("daemon-sdk").await.unwrap();
    drop(setup);
    let (daemon, socket) = spawn_daemon(home.path()).await;
    let airc = Airc::attach(home.path(), &socket).await.unwrap();

    assert!(airc.is_daemon_attached());
    airc.say("hello through attached sdk").await.unwrap();

    let event = wait_for_text(&airc, "hello through attached sdk", Duration::from_secs(3)).await;
    let after = airc.resume_from(&event.cursor(), 10).await.unwrap();
    assert!(after.is_empty());

    airc_daemon::DaemonClient::new(socket).stop().await.unwrap();
    tokio::time::timeout(Duration::from_secs(2), daemon)
        .await
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn subscribe_yields_live_events_in_order() {
    // The live subscription contract: subscribers see every event
    // the substrate appends to the store, in transcript order, while
    // the consumer is connected.
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("live-test").await.unwrap();

    let mut stream = airc.subscribe().await.unwrap();

    // Drive three sends from a spawned task. Cloning the Airc
    // handle is critical: a fresh `Airc::open` on the same home
    // would have its OWN broadcast channel, and the stream we're
    // holding wouldn't see fan-outs from that handle's subscriber.
    let airc_send = airc.clone();
    let send_task = tokio::spawn(async move {
        for i in 0..3 {
            airc_send.say(&format!("hi-{i}")).await.unwrap();
        }
    });

    let mut received = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    while received.len() < 3 && std::time::Instant::now() < deadline {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(event))) => {
                if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                    if text.starts_with("hi-") {
                        received.push(text.to_string());
                    }
                }
            }
            Ok(Some(Err(lag))) => panic!("unexpected live-stream lag: {lag}"),
            Ok(None) => panic!("stream closed unexpectedly"),
            Err(_) => continue,
        }
    }
    send_task.await.unwrap();

    assert_eq!(
        received,
        vec!["hi-0", "hi-1", "hi-2"],
        "subscriber must observe all three sends in send order"
    );
}

#[tokio::test]
async fn peer_spec_round_trips_via_add_peer() {
    let home_a = TempDir::new().unwrap();
    let home_b = TempDir::new().unwrap();
    let alice = Airc::open(home_a.path()).await.unwrap();
    let bob = Airc::open(home_b.path()).await.unwrap();

    // Alice prints her spec; Bob enrols it; Bob's peers list now
    // includes Alice.
    let alice_spec_str = alice.peer_spec();
    let alice_spec: PeerSpec = alice_spec_str.parse().unwrap();
    bob.add_peer(alice_spec).await.unwrap();

    let peers = bob.peers().await.unwrap();
    let alice_in_bobs_book = peers.iter().any(|p| p.peer_id == alice.peer_id());
    assert!(
        alice_in_bobs_book,
        "alice's peer_id must appear in bob's enrolled peers"
    );
}

#[test]
fn same_machine_scopes_share_account_wire_and_registry() {
    let machine = TempDir::new().unwrap();
    let _home_env_guard = HOME_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());

    temp_env::with_var("HOME", Some(machine.path()), || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let alice_home = machine.path().join("repo-a").join(".airc");
            let bob_home = machine.path().join("repo-b").join(".airc");
            std::fs::create_dir_all(alice_home.parent().unwrap()).unwrap();
            std::fs::create_dir_all(bob_home.parent().unwrap()).unwrap();

            let alice = Airc::open(&alice_home).await.unwrap();
            alice.join("general").await.unwrap();

            let bob = Airc::open(&bob_home).await.unwrap();
            bob.join("general").await.unwrap();

            let alice_room = alice.current_room().await.unwrap();
            let bob_room = bob.current_room().await.unwrap();
            assert_eq!(alice_room.channel, bob_room.channel);
            assert_eq!(alice_room.wire, bob_room.wire);
            assert_eq!(
                alice_room.wire,
                machine
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join(".airc/wires/general")
            );

            let peers = airc_daemon::peers_store::load(&machine.path().join(".airc"))
                .await
                .unwrap();
            assert_eq!(
                peers.len(),
                2,
                "opening two local scopes must publish both identities into the machine registry"
            );

            alice.say("same-machine account wire").await.unwrap();
            let event =
                wait_for_text(&bob, "same-machine account wire", Duration::from_secs(3)).await;
            assert_eq!(event.peer_id, alice.peer_id());
        });
    });
}

/// Regression: cross-instance (= cross-process in production) sends
/// must reach the receiver's LIVE BROADCAST stream, not only its
/// persistent store.
///
/// Previously, when two Airc instances shared a HOME (the
/// account-mesh convention), the receiver's wire subscriber re-read
/// the frame from disk, tried to persist it, got `DuplicateEventId`
/// (the sender had already persisted it via the shared SQLite store),
/// and silently dropped it without firing `live_tx`. Result: `inbox`
/// / `page_recent` showed the message but `subscribe` / `listen` /
/// `attach` / Monitor never narrated it. Cross-AI chat over the
/// public surface didn't work, even with the substrate fully wired.
///
/// Fix: `append_received_frame` fans out to `live_tx` on
/// `DuplicateEventId` too, with a `recently_broadcast` ring to avoid
/// double-delivery of locally-sent events.
///
/// This test models the production scenario via two distinct
/// `Airc::open` calls on the same home (each open allocates its own
/// `recently_broadcast` ring, mirroring two separate processes).
#[test]
fn cross_instance_send_reaches_receiver_subscribe_stream() {
    let machine = TempDir::new().unwrap();
    let _home_env_guard = HOME_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());

    temp_env::with_var("HOME", Some(machine.path()), || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            // Two scopes pointing at separate home dirs that share
            // the same machine-account wire root (mirrors how two
            // Claude/Codex tabs on Joel's machine end up using
            // `~/.airc/wires/<channel>/` even when they were
            // launched from different repos).
            let alice_home = machine.path().join("repo-a").join(".airc");
            let bob_home = machine.path().join("repo-b").join(".airc");
            std::fs::create_dir_all(alice_home.parent().unwrap()).unwrap();
            std::fs::create_dir_all(bob_home.parent().unwrap()).unwrap();

            let alice = Airc::open(&alice_home).await.unwrap();
            alice.join("general").await.unwrap();

            let bob = Airc::open(&bob_home).await.unwrap();
            bob.join("general").await.unwrap();

            // Subscribe BEFORE alice sends so the live stream is
            // armed and ready to receive. Bob is the "listener" /
            // "monitor attach" role.
            let mut bob_stream = bob.subscribe().await.unwrap();

            alice.say("live chat across instances").await.unwrap();

            // Wait up to 3s for the event to flow through the wire
            // subscriber → bob's live_tx → bob_stream.
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            let mut got_text: Option<String> = None;
            while std::time::Instant::now() < deadline {
                let next = tokio::time::timeout(Duration::from_millis(100), bob_stream.next())
                    .await
                    .ok()
                    .flatten();
                if let Some(Ok(event)) = next {
                    if let Some(text) = event.body.as_ref().and_then(Body::as_text) {
                        if text == "live chat across instances" {
                            got_text = Some(text.to_string());
                            break;
                        }
                    }
                }
            }

            assert_eq!(
                got_text.as_deref(),
                Some("live chat across instances"),
                "bob's subscribe stream must see alice's send live — \
                 NOT just via store/inbox. This is the cross-process \
                 live-broadcast path that the public `airc msg` / \
                 `airc attach` chat surface depends on."
            );

            // No duplicate delivery for alice's own send into her
            // own subscribe stream.
            let mut alice_stream = alice.subscribe().await.unwrap();
            alice.say("alice-only echo").await.unwrap();
            let mut count = 0;
            let deadline = std::time::Instant::now() + Duration::from_secs(1);
            while std::time::Instant::now() < deadline {
                let next = tokio::time::timeout(Duration::from_millis(100), alice_stream.next())
                    .await
                    .ok()
                    .flatten();
                if let Some(Ok(event)) = next {
                    if event.body.as_ref().and_then(Body::as_text) == Some("alice-only echo") {
                        count += 1;
                    }
                }
            }
            assert_eq!(
                count, 1,
                "alice's own send must reach her subscribe stream EXACTLY ONCE — \
                 not twice (once via send-side fan-out, once via wire-subscriber \
                 re-read). recently_broadcast ring de-dupes this."
            );
        });
    });
}

#[test]
fn default_join_context_subscribes_general_and_repo_owner_on_shared_account_wire() {
    let machine = TempDir::new().unwrap();
    let _home_env_guard = HOME_ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner());

    temp_env::with_var("HOME", Some(machine.path()), || {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        runtime.block_on(async {
            let alice_repo = repo_with_origin(
                &machine.path().join("alice-continuum"),
                "https://github.com/CambrianTech/continuum.git",
            );
            let bob_repo = repo_with_origin(
                &machine.path().join("bob-airc"),
                "git@github.com:CambrianTech/airc.git",
            );
            let alice_home = alice_repo.join(".airc");
            let bob_home = bob_repo.join(".airc");

            seed_mesh_identity(&alice_home, "joelteply");
            seed_mesh_identity(&bob_home, "joelteply");

            let alice = Airc::open(&alice_home).await.unwrap();
            let bob = Airc::open(&bob_home).await.unwrap();

            let alice_rooms = alice.join_default_context(&alice_repo).await.unwrap();
            let bob_rooms = bob.join_default_context(&bob_repo).await.unwrap();

            assert_eq!(room_names(&alice_rooms), vec!["cambriantech", "general"]);
            assert_eq!(room_names(&bob_rooms), vec!["cambriantech", "general"]);
            assert_eq!(alice.current_room().await.unwrap().name, "cambriantech");
            assert_eq!(bob.current_room().await.unwrap().name, "cambriantech");
            assert_eq!(
                alice.current_room().await.unwrap().wire,
                machine
                    .path()
                    .canonicalize()
                    .unwrap()
                    .join(".airc/wires/cambriantech")
            );
            assert_eq!(
                alice.current_room().await.unwrap().channel,
                bob.current_room().await.unwrap().channel
            );

            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let snapshot = coordinator_snapshot(
                &machine.path().join(".airc"),
                &MeshIdentity::new("joelteply"),
                &CoordinatorConfig::default(),
                now_ms,
            )
            .unwrap();
            assert_eq!(snapshot.live.len(), 2);
            assert_eq!(
                snapshot
                    .live_channels
                    .iter()
                    .map(ChannelName::as_str)
                    .collect::<Vec<_>>(),
                vec!["cambriantech", "general"]
            );
            assert!(
                snapshot
                    .live
                    .iter()
                    .any(|beacon| beacon.scope_home == alice_home),
                "join_default_context must publish Alice's scope beacon"
            );
            assert!(
                snapshot
                    .live
                    .iter()
                    .any(|beacon| beacon.scope_home == bob_home),
                "join_default_context must publish Bob's scope beacon"
            );

            alice.say("default account context works").await.unwrap();
            let event = wait_for_text(
                &bob,
                "default account context works",
                Duration::from_secs(3),
            )
            .await;
            assert_eq!(event.peer_id, alice.peer_id());
        });
    });
}

#[tokio::test]
async fn open_is_idempotent_across_handles() {
    // Two Airc::open calls on the same home recover the same
    // identity and event store.
    let home = TempDir::new().unwrap();
    let first = Airc::open(home.path()).await.unwrap();
    let first_peer = first.peer_id();
    drop(first);
    let second = Airc::open(home.path()).await.unwrap();
    assert_eq!(second.peer_id(), first_peer);
}

fn repo_with_origin(path: &std::path::Path, origin: &str) -> std::path::PathBuf {
    std::fs::create_dir_all(path.join(".git")).unwrap();
    std::fs::write(
        path.join(".git/config"),
        format!(
            r#"[core]
    repositoryformatversion = 0
[remote "origin"]
    url = {origin}
"#
        ),
    )
    .unwrap();
    path.to_path_buf()
}

fn seed_mesh_identity(home: &std::path::Path, identity: &str) {
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;
    resolve_mesh_identity_with(
        home,
        || Some((identity.to_string(), MeshIdentitySource::Operator)),
        now_ms,
    )
    .unwrap();
}

fn room_names(rooms: &[airc_lib::Room]) -> Vec<&str> {
    rooms.iter().map(|room| room.name.as_str()).collect()
}

#[tokio::test]
async fn send_typed_body_with_headers_round_trips() {
    // Gate-4 bullet: "send typed body with headers". The headers
    // survive the wire boundary and land in the persisted event.
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("typed-test").await.unwrap();

    let mut headers = Headers::new();
    headers.insert(
        "forge.body_hint".to_string(),
        "application/json".to_string(),
    );
    headers.insert("x-test-marker".to_string(), "round-trip".to_string());
    let _event_id = airc
        .send(Body::text(r#"{"k":"v"}"#), headers.clone())
        .await
        .unwrap();

    let page = wait_for_events(&airc, 1, Duration::from_secs(2)).await;
    assert_eq!(page.len(), 1);
    assert_eq!(
        page[0].headers.get("x-test-marker").map(String::as_str),
        Some("round-trip")
    );
    assert_eq!(
        page[0].headers.get("forge.body_hint").map(String::as_str),
        Some("application/json")
    );
}

#[tokio::test]
async fn send_refuses_github_invite_only_route() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("route-gate").await.unwrap();
    airc.replace_transport_health([TransportHealthSample {
        kind: TransportKind::GhGist,
        role: TransportRole::InviteBeacon,
        state: airc_lib::TransportHealthState::Healthy,
        rtt_ms: None,
        success_ppm: None,
    }])
    .unwrap();

    let err = airc.say("must not go through gist").await.unwrap_err();

    assert!(
        err.to_string()
            .contains("DataInteractive has no admissible live route"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn two_embedded_handles_chat_over_lan_without_cli() {
    let alice_home = TempDir::new().unwrap();
    let bob_home = TempDir::new().unwrap();
    let alice = Airc::open(alice_home.path()).await.unwrap();
    let bob = Airc::open(bob_home.path()).await.unwrap();

    let alice_spec: PeerSpec = alice.peer_spec().parse().unwrap();
    let bob_spec: PeerSpec = bob.peer_spec().parse().unwrap();
    alice.add_peer(bob_spec).await.unwrap();
    bob.add_peer(alice_spec).await.unwrap();

    alice.join("lan-room").await.unwrap();
    bob.join("lan-room").await.unwrap();

    let bind = SocketAddr::from(([127, 0, 0, 1], 0));
    let alice_addr = alice.listen_lan(bind).await.unwrap();
    bob.connect_lan(alice_addr, alice.peer_id()).await.unwrap();

    bob.say("hello over sdk lan").await.unwrap();

    let page = wait_for_events(&alice, 1, Duration::from_secs(3)).await;
    let bodies: Vec<&str> = page
        .iter()
        .filter_map(|event| event.body.as_ref().and_then(Body::as_text))
        .collect();
    assert_eq!(bodies, vec!["hello over sdk lan"]);
}

#[tokio::test]
async fn lan_listen_feeds_health_and_invite_endpoint_without_tailscale() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    let bound = airc
        .listen_lan(SocketAddr::from(([127, 0, 0, 1], 0)))
        .await
        .unwrap();

    let health = airc.transport_health().unwrap();
    assert!(
        health
            .iter()
            .any(|sample| sample.kind == TransportKind::LocalFs),
        "local route remains present"
    );
    assert!(
        health.iter().any(|sample| {
            sample.kind == TransportKind::LanTcp
                && sample.state == airc_lib::TransportHealthState::Healthy
        }),
        "LAN listen must feed route health"
    );

    let beacon = airc.invite_beacon().unwrap();
    assert_eq!(beacon.peer_id, airc.peer_id());
    assert!(
        beacon
            .endpoints
            .contains(&RouteEndpoint::LanTcp { addr: bound }),
        "invite beacon publishes connection metadata"
    );
    assert!(
        beacon
            .endpoints
            .iter()
            .all(|endpoint| !matches!(endpoint, RouteEndpoint::TailscaleTcp { .. })),
        "Tailscale must not be invented for local LAN operation"
    );
}

#[tokio::test]
async fn discovery_refresh_is_local_first_without_github_or_tailscale() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    airc.replace_transport_health([]).unwrap();
    let snapshot = airc.refresh_route_discovery().await.unwrap();

    assert!(
        snapshot
            .health
            .iter()
            .any(|sample| sample.kind == TransportKind::LocalFs),
        "local-fs must be discovered without GitHub"
    );
    assert!(
        snapshot
            .health
            .iter()
            .all(|sample| sample.kind != TransportKind::GhGist),
        "GitHub must not appear in local discovery"
    );
    assert!(
        snapshot
            .health
            .iter()
            .all(|sample| sample.kind != TransportKind::Tailscale),
        "Tailscale is optional reachability, not a local dependency"
    );
}

#[tokio::test]
async fn filtered_event_queries_match_kind_and_headers_without_body_parse() {
    let home = TempDir::new().unwrap();
    let airc = Airc::open(home.path()).await.unwrap();
    airc.join("filtered-events").await.unwrap();

    let mut headers = Headers::new();
    headers.insert(
        "forge.body_hint".to_string(),
        "forge.persona.turn".to_string(),
    );
    headers.insert("continuum.activity".to_string(), "general".to_string());
    airc.send(Body::text("persona turn payload"), headers)
        .await
        .unwrap();
    airc.say("plain chat").await.unwrap();

    let mut filter = EventFilter::current_room();
    filter.kinds.insert(TranscriptKind::Message);
    filter.headers_filter = HeaderFilter::Prefix {
        key: "forge.body_hint".to_string(),
        value_prefix: "forge.persona.".to_string(),
    };

    let matches = airc.page_recent_filtered(filter, 32).await.unwrap();
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0].body.as_ref().and_then(Body::as_text),
        Some("persona turn payload")
    );
}

#[tokio::test]
async fn subscribed_filter_reads_all_configured_channels_not_current_room_only() {
    let home = TempDir::new().unwrap();
    let identity = MeshIdentity::unset();
    let mut subscription_set = SubscriptionSet::empty();
    subscription_set
        .subscribe(home.path(), &identity, ChannelName::new("general").unwrap())
        .unwrap();
    subscription_set
        .subscribe(
            home.path(),
            &identity,
            ChannelName::new("cambriantech").unwrap(),
        )
        .unwrap();
    subscriptions::save(home.path(), &subscription_set).unwrap();
    let airc = Airc::open(home.path()).await.unwrap();

    airc.join("general").await.unwrap();
    airc.say("general lobby message").await.unwrap();
    wait_for_text(&airc, "general lobby message", Duration::from_secs(2)).await;

    airc.join("cambriantech").await.unwrap();
    airc.say("project room message").await.unwrap();
    wait_for_text(&airc, "project room message", Duration::from_secs(2)).await;

    let filter = EventFilter {
        kinds: std::collections::BTreeSet::from([TranscriptKind::Message]),
        ..EventFilter::default()
    };
    let events = airc
        .page_recent_subscribed_filtered(filter, 32)
        .await
        .unwrap();
    let texts = events
        .iter()
        .filter_map(|event| event.body.as_ref().and_then(Body::as_text))
        .collect::<Vec<_>>();

    assert!(texts.contains(&"general lobby message"));
    assert!(texts.contains(&"project room message"));
}
