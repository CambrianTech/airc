//! Exercise the production bridge through real IPC and TLS, without serving workloads.
use super::*;
use airc_core::{MentionTarget, PeerId};
use airc_ipc::{DaemonClient, Response};
use airc_protocol::{PeerKeyRegistry, PeerKeypair, VerificationPolicy};
use airc_store::EventStore;
use std::sync::Arc;

struct Machine {
    gateway: airc_lib::Airc,
    state: Arc<DaemonState>,
    client: DaemonClient,
    task: tokio::task::JoinHandle<Result<(), airc_daemon::DaemonError>>,
    // Drop resources before directories, including on panic.
    _socket_home: tempfile::TempDir,
    home: tempfile::TempDir,
}

impl Drop for Machine {
    fn drop(&mut self) {
        self.state.shutdown.notify_waiters();
        if !self.task.is_finished() {
            self.task.abort();
        }
    }
}

impl Machine {
    async fn shutdown(mut self) {
        self.state.shutdown.notify_waiters();
        tokio::time::timeout(std::time::Duration::from_secs(5), &mut self.task)
            .await
            .expect("daemon shutdown deadline")
            .expect("daemon task joined")
            .expect("daemon shutdown succeeded");
    }

    async fn boot() -> Self {
        let home = tempfile::tempdir().unwrap();
        let gateway = airc_lib::Airc::open(home.path()).await.unwrap();
        let identity = airc_identity::LocalIdentity::load(home.path())
            .await
            .unwrap();
        let store = Arc::new(
            airc_store::SqliteEventStore::open_path(&home.path().join("events.sqlite"))
                .await
                .unwrap(),
        );
        let state = Arc::new(
            DaemonState::build(
                identity.peer_id,
                identity.keypair,
                Arc::new(PeerKeyRegistry::new()),
                VerificationPolicy::Strict,
                home.path().to_owned(),
                &home.path().join("events.sqlite"),
                store,
                airc_daemon::DaemonRuntimeInfo::unknown(),
            )
            .await
            .unwrap(),
        );
        gateway
            .set_addressed_frame_sink(Arc::new(DaemonAddressedSink(Arc::downgrade(&state))))
            .unwrap();
        state
            .addressed
            .set_forwarder(Arc::new(DaemonAddressedForwarder {
                airc: gateway.clone(),
                home: home.path().to_owned(),
                daemon: state.peer_id,
            }))
            .unwrap();
        let socket_home = tempfile::tempdir().unwrap();
        let socket = socket_home.path().join("a.sock");
        let client = DaemonClient::new(socket.clone());
        let (ready, bound) = tokio::sync::oneshot::channel();
        let serving = state.clone();
        let task = tokio::spawn(airc_daemon::server::run_with_startup_check(
            serving,
            socket,
            move || {
                ready
                    .send(())
                    .map_err(|_| std::io::Error::other("fixture receiver gone"))
            },
        ));
        tokio::time::timeout(std::time::Duration::from_secs(5), bound)
            .await
            .unwrap()
            .unwrap();
        Self {
            home,
            gateway,
            state,
            client,
            task,
            _socket_home: socket_home,
        }
    }
}

#[tokio::test]
async fn production_bridges_forward_signed_frames_bidirectionally_without_storage() {
    let a = Machine::boot().await;
    let b = Machine::boot().await;
    a.gateway
        .add_peer(b.gateway.peer_spec().parse().unwrap())
        .await
        .unwrap();
    b.gateway
        .add_peer(a.gateway.peer_spec().parse().unwrap())
        .await
        .unwrap();
    let addr = b
        .gateway
        .listen_lan("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    a.gateway
        .connect_lan(addr, b.gateway.peer_id())
        .await
        .unwrap();
    let alice = (PeerId::new(), PeerKeypair::generate());
    let bob = (PeerId::new(), PeerKeypair::generate());
    for machine in [&a, &b] {
        for (peer, key) in [&alice, &bob] {
            machine
                .gateway
                .add_peer(airc_lib::PeerSpec {
                    peer_id: *peer,
                    pubkey: key.public_bytes(),
                })
                .await
                .unwrap();
        }
        for (peer, host) in [(alice.0, a.state.peer_id), (bob.0, b.state.peer_id)] {
            airc_trust::set_endpoints_json(machine.home.path(), peer, None, 1, Some(host))
                .await
                .unwrap();
        }
    }
    let mut alice_inbox = a
        .client
        .attach_addressed(alice.0, a.state.peer_id, &alice.1, 0)
        .await
        .unwrap();
    let mut bob_inbox = b
        .client
        .attach_addressed(bob.0, b.state.peer_id, &bob.1, 0)
        .await
        .unwrap();
    // Opening identities and enrolling peers may emit lifecycle events. Capture
    // all channels before sending so those records cannot mask new persistence.
    let mut before = Vec::new();
    for machine in [&a, &b] {
        let store =
            airc_store::SqliteEventStore::open_path(&machine.home.path().join("events.sqlite"))
                .await
                .unwrap();
        let snapshot = store.page_recent(None, 100_000).await.unwrap();
        assert!(snapshot.len() < 100_000, "baseline scan truncated");
        before.push(snapshot);
    }
    for (sender, recipient, client, inbox) in [
        (&alice, bob.0, &a.client, &mut bob_inbox),
        (&bob, alice.0, &b.client, &mut alice_inbox),
    ] {
        let mut frame = Frame {
            kind: airc_protocol::FrameKind::Addressed,
            envelope: airc_protocol::Envelope {
                event_id: airc_core::EventId::new(),
                sender: sender.0,
                sender_client: airc_core::ClientId::new(),
                channel: airc_protocol::addressed::addressed_channel(),
                target: MentionTarget::Peer(recipient),
                lamport: 1,
                occurred_at_ms: 1,
                reply_to: None,
                headers: Default::default(),
                body: Some(airc_core::Body::Binary(vec![17; 64])),
                media: Vec::new(),
                signature: airc_protocol::Signature::Unsigned,
            },
        };
        airc_protocol::addressed_deadline::stamp(
            &mut frame.envelope.headers,
            std::time::SystemTime::now(),
            std::time::Duration::from_secs(5),
        )
        .unwrap();
        frame.envelope.signature = sender
            .1
            .sign_envelope(&frame.envelope, sender.0, 0)
            .unwrap();
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        client
            .send_addressed(frame.clone(), deadline)
            .await
            .unwrap();
        let received =
            tokio::time::timeout_at(deadline, airc_ipc::codec::read_frame::<_, Response>(inbox))
                .await
                .unwrap()
                .unwrap();
        assert_eq!(received, Some(Response::Addressed { frame }));
    }
    for (machine, baseline) in [&a, &b].into_iter().zip(before) {
        let store =
            airc_store::SqliteEventStore::open_path(&machine.home.path().join("events.sqlite"))
                .await
                .unwrap();
        let after = store.page_recent(None, 100_000).await.unwrap();
        assert!(after.len() < 100_000, "post-send scan truncated");
        assert_eq!(
            after, baseline,
            "addressed exchange changed the durable transcript"
        );
    }
    drop(alice_inbox);
    drop(bob_inbox);
    a.shutdown().await;
    b.shutdown().await;
}

#[tokio::test]
async fn attached_sdk_scopes_exchange_encrypted_command_and_result() {
    use airc_lib::addressed::{AddressedKind, AddressedSend};
    let a = Machine::boot().await;
    let b = Machine::boot().await;
    a.gateway
        .add_peer(b.gateway.peer_spec().parse().unwrap())
        .await
        .unwrap();
    b.gateway
        .add_peer(a.gateway.peer_spec().parse().unwrap())
        .await
        .unwrap();
    let addr = b
        .gateway
        .listen_lan("127.0.0.1:0".parse().unwrap())
        .await
        .unwrap();
    a.gateway
        .connect_lan(addr, b.gateway.peer_id())
        .await
        .unwrap();
    let alice =
        airc_lib::Airc::attach_as(a.home.path(), "alice", a._socket_home.path().join("a.sock"))
            .await
            .unwrap();
    let bob = airc_lib::Airc::attach_as(b.home.path(), "bob", b._socket_home.path().join("a.sock"))
        .await
        .unwrap();
    for gateway in [&a.gateway, &b.gateway, &alice, &bob] {
        for scope in [&alice, &bob] {
            gateway
                .add_peer(scope.peer_spec().parse().unwrap())
                .await
                .unwrap();
        }
    }
    for machine in [&a, &b] {
        for (scope, host) in [(&alice, a.state.peer_id), (&bob, b.state.peer_id)] {
            airc_trust::set_endpoints_json(
                machine.home.path(),
                scope.peer_id(),
                None,
                1,
                Some(host),
            )
            .await
            .unwrap();
        }
    }
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut alice_inbox = alice.addressed_inbox(deadline).await.unwrap();
    let mut bob_inbox = bob.addressed_inbox(deadline).await.unwrap();
    let mut before = Vec::new();
    for machine in [&a, &b] {
        let store =
            airc_store::SqliteEventStore::open_path(&machine.home.path().join("events.sqlite"))
                .await
                .unwrap();
        let snapshot = store.page_recent(None, 100_000).await.unwrap();
        assert!(snapshot.len() < 100_000);
        before.push(snapshot);
    }
    let correlation = uuid::Uuid::new_v4();
    for (sender, recipient, inbox, kind, body) in [
        (
            &alice,
            &bob,
            &mut bob_inbox,
            AddressedKind::Command,
            "private daemon request",
        ),
        (
            &bob,
            &alice,
            &mut alice_inbox,
            AddressedKind::CommandResult,
            "private daemon result",
        ),
    ] {
        sender
            .send_addressed(
                AddressedSend {
                    to: recipient.peer_id(),
                    kind,
                    correlation,
                    headers: Default::default(),
                    body: airc_core::Body::text(body),
                },
                deadline,
            )
            .await
            .unwrap();
        let received = tokio::time::timeout_at(deadline, inbox.next())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(received.from(), sender.peer_id());
        assert_eq!(received.to(), recipient.peer_id());
        assert_eq!(received.kind(), kind);
        assert_eq!(received.correlation(), correlation);
        assert_eq!(received.body().as_text(), Some(body));
    }
    for (machine, baseline) in [&a, &b].into_iter().zip(before) {
        let store =
            airc_store::SqliteEventStore::open_path(&machine.home.path().join("events.sqlite"))
                .await
                .unwrap();
        let after = store.page_recent(None, 100_000).await.unwrap();
        assert!(after.len() < 100_000);
        assert_eq!(
            after, baseline,
            "encrypted command exchange persisted transcript data"
        );
    }
    drop(alice_inbox);
    drop(bob_inbox);
    drop(alice);
    drop(bob);
    a.shutdown().await;
    b.shutdown().await;
}
