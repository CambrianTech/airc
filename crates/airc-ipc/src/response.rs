//! Daemon → client responses. Symmetric to `request.rs` — typed
//! enum, wire-tagged by `kind`.
//!
//! Owner-core model: live events and inbox pages cross the IPC boundary
//! as **opaque airc-wire bytes** (`airc_wire::encode(&Envelope)`) — the
//! daemon encodes once, the client decodes once. The IPC layer stays
//! ignorant of the envelope's shape (no `airc-bus` dependency leaks
//! here, no per-hop re-serialize).

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use airc_core::{EventId, PeerId, RoomId};

use crate::request::IpcCursor;

/// One response to a `Request`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    /// Response to `Ping`.
    Pong,
    /// Response to `Status`.
    Status(StatusResponse),
    /// Response to `Inbox` — durable envelopes (airc-wire bytes) + a
    /// "newest cursor" the caller threads back on the next call to keep
    /// the stream consume-once.
    Inbox(InboxResponse),
    /// **Card a1562dbc.** Response to `RoomTip` — the cursor of the
    /// newest durable event on the requested channel, straight from the
    /// store's index. No envelope bytes ride along: the probe returns
    /// the cursor value only, never a copy of an event body.
    RoomTip(RoomTipResponse),
    /// Response to `PeerIdentityCard` — the peer's durable identity-index
    /// row (opaque `Identity` JSON + LWW `version`), or `None` when the peer
    /// has never published a card. The identity analog of `RoomTip`: the
    /// daemon answers from its owner-core `scoped_state` index, never by
    /// replaying the room.
    PeerIdentityCard(PeerIdentityCardResponse),
    /// One live event emitted by an `Attach` stream — the airc-wire
    /// encoding of the bus `Envelope`. The client decodes via
    /// `airc_wire::decode`.
    Event { envelope: Vec<u8> },
    /// **Card 7d5b6a65.** Emitted by an `Attach` stream when the
    /// client requested `coalesce_backlog: true` and the daemon had
    /// backlog to catch up on. ONE summary frame per attach catch-up,
    /// then the stream transitions to live tail; each subsequent live
    /// event arrives as its own `Event` frame.
    ///
    /// `skipped` is the count of historical events the daemon
    /// suppressed during catch-up. `advanced_to` is the cursor the
    /// daemon resumed from — the client may persist this and pass it
    /// as `from` on a future reconnect to skip the same backlog
    /// without `from_now`. When `skipped: 0`, the catch-up phase was
    /// empty (no backlog at attach time) and the frame is suppressed
    /// — the daemon emits this variant only when it actually omitted
    /// at least one event.
    AttachCursorAdvanced {
        skipped: u64,
        advanced_to: IpcCursor,
    },
    /// Response to `Publish` / `Send` — the owner-assigned receipt.
    Publish(PublishResponse),
    /// Response to `ListPeers` — the daemon's currently-enrolled
    /// peers (peer_id + URL-safe-no-padding base64 pubkey).
    Peers(PeersResponse),
    /// **Card 4b6a0ffa (#33).** Response to `RouteEndpoints` — the
    /// dialable endpoints this daemon currently advertises in its
    /// account-registry beacon. Short-lived CLI publishers (`airc
    /// registry sync`) read these back instead of advertising their
    /// own dead listener or overwriting the gist endpoint-less.
    RouteEndpoints(RouteEndpointsResponse),
    /// Generic success for ops that don't return data (`AddPeer`,
    /// `RemovePeer`, `Stop`, and the initial `Attach` ack).
    Ok,
    /// Failure — typed message so the client can render it.
    Error { message: String },
}

/// Daemon health/state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StatusResponse {
    /// Peer UUID as the hyphenated string form.
    pub peer_id: String,
    /// Seconds since daemon start.
    pub uptime_seconds: u64,
    /// IPC protocol version spoken by this daemon. Missing means the
    /// daemon predates status metadata and should be treated as stale
    /// by lifecycle code.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ipc_protocol_version: Option<u32>,
    /// Build commit baked into the daemon binary. Missing means
    /// unknown/old daemon.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_commit: Option<String>,
    /// Build branch baked into the daemon binary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_branch: Option<String>,
    /// Executable path of the daemon process. This is diagnostics
    /// only; lifecycle decisions use protocol + build metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable: Option<String>,
    /// Count of remote peers this daemon currently holds a LIVE LAN
    /// connection to — the set room broadcast actually fans out to
    /// (`RoutedForwarder` forwards only over `connected_peers()`),
    /// refreshed by the daemon's route-refresh loop. `0` while
    /// enrolled peers exist means a room send reaches NO remote peer:
    /// the signal `airc send` surfaces so a broken fan-out can't
    /// masquerade as success (the enrolled-peer "address book" count
    /// alone hid this). `#[serde(default)]`: a daemon predating this
    /// field decodes as `0` — treated as "unknown / none", which the
    /// receipt frames honestly rather than as confirmed reach.
    #[serde(default)]
    pub connected_lan_peers: usize,
}

/// One entry in the `Peers` response. Mirrors `peers_store::StoredPeer`
/// but lives in `ipc` so the client doesn't need to depend on the
/// daemon's storage module.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerEntry {
    pub peer_id: PeerId,
    pub pubkey_b64: String,
}

/// Snapshot of enrolled peers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeersResponse {
    pub peers: Vec<PeerEntry>,
}

/// Result of an `Inbox` pull: durable envelopes (airc-wire bytes) + the
/// cursor to feed back as `since` on the next call. Envelopes are in
/// total order `(epoch, counter, event_id)`, oldest → newest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InboxResponse {
    /// Up to `limit` envelopes matching the request, each an
    /// `airc_wire::encode(&Envelope)` buffer.
    pub envelopes: Vec<Vec<u8>>,
    /// Cursor of the newest envelope in `envelopes`. `None` when the
    /// page was empty — the caller's `since` stays authoritative.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newest: Option<IpcCursor>,
}

/// Result of a `RoomTip` probe (card a1562dbc): the durable tip of one
/// channel. Mirrors `InboxResponse.newest`'s wire shape (`tip` absent
/// when the room has no durable history) so the two cursors are
/// interchangeable as watermark inputs.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub struct RoomTipResponse {
    /// Cursor of the newest durable event on the channel. `None` (and
    /// absent on the wire) when the room has no durable events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tip: Option<IpcCursor>,
}

/// Result of a `PeerIdentityCard` resolve: the peer's durable
/// identity-index row, or `None` (absent on the wire) when the peer has
/// never published a card. The identity analog of [`RoomTipResponse`] —
/// answered from the daemon's owner-core `scoped_state` index.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PeerIdentityCardResponse {
    /// The stored identity-index row, or `None` when no card exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card: Option<IpcIdentityCard>,
}

/// One peer's durable identity-index row as it crosses IPC: the opaque
/// serialized `Identity` JSON plus its LWW `version` (the card's
/// `emitted_at_ms`). Mirrors `airc_store::StoredScopedState`'s
/// `(value_json, version)` without leaking the store type across the
/// boundary — this crate sits below airc-lib/airc-store's consumers,
/// exactly like `IpcRouteEndpoint` mirrors `RouteEndpoint`. The client
/// (`airc-lib`) reconstructs the typed `PeerIdentityCard` from these two
/// fields.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IpcIdentityCard {
    /// Serialized `airc_core::identity::Identity` JSON. Opaque here.
    pub value_json: String,
    /// LWW version — the card's `emitted_at_ms`, recorded verbatim.
    pub version: i64,
}

/// One dialable endpoint advertised by the daemon (card 4b6a0ffa /
/// #33). Mirrors `airc_lib::RouteEndpoint` — same `kind`-tagged
/// snake_case wire shape — without leaking the lib type across the
/// IPC boundary (this crate sits below `airc-lib` in the dependency
/// graph, exactly like `IpcDelivery` mirrors the bus class). The
/// conversion lives in `airc-cli`, the only crate that sees both.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IpcRouteEndpoint {
    LanTcp {
        #[serde(with = "socket_addr_string")]
        addr: SocketAddr,
    },
    TailscaleTcp {
        #[serde(with = "socket_addr_string")]
        addr: SocketAddr,
    },
    Udp {
        #[serde(with = "socket_addr_string")]
        addr: SocketAddr,
    },
    Relay {
        url: String,
    },
    Reticulum {
        destination: String,
    },
    WebRtcSignaling {
        url: String,
    },
}

/// `SocketAddr` as its `Display`/`FromStr` string on the wire,
/// EXPLICITLY. std's serde impl picks string vs. struct form off the
/// format's `is_human_readable`, which does not round-trip through
/// the CBOR frame codec — and an implicit format-dependent shape is
/// not a contract. One spelled-out shape (`"10.0.0.2:7717"`), pinned
/// by the literal-JSON tests below, identical in JSON and CBOR.
mod socket_addr_string {
    use std::net::SocketAddr;

    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(addr: &SocketAddr, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(addr)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<SocketAddr, D::Error> {
        let raw = String::deserialize(deserializer)?;
        raw.parse().map_err(serde::de::Error::custom)
    }
}

/// Result of a `RouteEndpoints` probe: every endpoint the daemon's
/// registry glue currently advertises. Empty means the daemon is up
/// but not dialable (no LAN listener bound, no relay) — the caller
/// must treat that exactly like "no daemon" for publish decisions.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RouteEndpointsResponse {
    pub endpoints: Vec<IpcRouteEndpoint>,
}

/// Owner-assigned receipt returned by `Send` / `Publish`. The
/// `(epoch, counter)` seq IS the authoritative total order; wall-clock
/// `occurred_at` lives on the envelope itself (decode from inbox/attach
/// bytes if a client needs it), so it isn't duplicated here.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PublishResponse {
    pub event_id: EventId,
    /// Generational epoch of the assigned `(epoch, counter)` seq.
    pub epoch: u64,
    /// Monotonic counter within the epoch.
    pub counter: u64,
    /// Owner-stamped wall-clock at publish (informational; the
    /// authoritative order is `(epoch, counter)`).
    pub occurred_at_ms: u64,
    pub channel_id: RoomId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pong_serializes_compactly() {
        assert_eq!(
            serde_json::to_string(&Response::Pong).unwrap(),
            r#"{"kind":"pong"}"#
        );
    }

    #[test]
    fn status_roundtrips() {
        let original = Response::Status(StatusResponse {
            peer_id: "07e7ad58-ba56-4535-b4e5-a161a110e487".to_string(),
            uptime_seconds: 42,
            ipc_protocol_version: Some(3),
            build_commit: Some("abc123".to_string()),
            build_branch: Some("rust-rewrite".to_string()),
            executable: Some("/tmp/airc".to_string()),
            connected_lan_peers: 2,
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn status_accepts_pre_metadata_daemon_response() {
        let decoded: Response = serde_json::from_str(
            r#"{"kind":"status","peer_id":"07e7ad58-ba56-4535-b4e5-a161a110e487","uptime_seconds":42}"#,
        )
        .unwrap();
        assert_eq!(
            decoded,
            Response::Status(StatusResponse {
                peer_id: "07e7ad58-ba56-4535-b4e5-a161a110e487".to_string(),
                uptime_seconds: 42,
                ipc_protocol_version: None,
                build_commit: None,
                build_branch: None,
                executable: None,
                connected_lan_peers: 0,
            })
        );
    }

    /// A daemon predating the `connected_lan_peers` field (it carries
    /// the metadata fields but not the connection count) must decode as
    /// `0` — "unknown / none", never a crash. Pins the `#[serde(default)]`
    /// back-compat contract so a newer CLI can read an older daemon.
    #[test]
    fn status_defaults_connected_lan_peers_for_pre_field_daemon() {
        let decoded: Response = serde_json::from_str(
            r#"{"kind":"status","peer_id":"07e7ad58-ba56-4535-b4e5-a161a110e487","uptime_seconds":42,"ipc_protocol_version":3}"#,
        )
        .unwrap();
        let Response::Status(status) = decoded else {
            panic!("expected status response");
        };
        assert_eq!(
            status.connected_lan_peers, 0,
            "a daemon without the field must decode as 0 connected peers, not error"
        );
    }

    #[test]
    fn error_carries_message() {
        let error = Response::Error {
            message: "boom".to_string(),
        };
        let encoded = serde_json::to_string(&error).unwrap();
        assert!(encoded.contains("boom"));
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, error);
    }

    #[test]
    fn publish_response_roundtrips_with_epoch_counter() {
        let original = Response::Publish(PublishResponse {
            event_id: EventId::from_u128(1),
            epoch: 2,
            counter: 9,
            occurred_at_ms: 1_700_000_000_000,
            channel_id: RoomId::from_u128(4),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    #[test]
    fn inbox_response_roundtrips_with_wire_bytes() {
        let original = Response::Inbox(InboxResponse {
            envelopes: vec![vec![1, 2, 3], vec![4, 5, 6, 7]],
            newest: Some(IpcCursor {
                epoch: 1,
                counter: 2,
                event_id: EventId::from_u128(3),
            }),
        });
        let encoded = serde_json::to_string(&original).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, original);
    }

    /// Card a1562dbc: the EXACT wire bytes of `RoomTip` are pinned per
    /// shape — `kind` tag, `tip` field name, nested cursor field names
    /// all literal. Both shapes covered: a populated tip, and the
    /// empty-room shape where `tip` is ABSENT (not `null`), mirroring
    /// `InboxResponse.newest`.
    #[test]
    fn room_tip_response_wire_bytes_are_pinned_per_shape() {
        for (response, expected) in [
            (
                Response::RoomTip(RoomTipResponse {
                    tip: Some(IpcCursor {
                        epoch: 3,
                        counter: 17,
                        event_id: EventId::from_u128(0xfeed),
                    }),
                }),
                r#"{"kind":"room_tip","tip":{"epoch":3,"counter":17,"event_id":"00000000-0000-0000-0000-00000000feed"}}"#,
            ),
            (
                Response::RoomTip(RoomTipResponse { tip: None }),
                r#"{"kind":"room_tip"}"#,
            ),
        ] {
            let encoded = serde_json::to_string(&response).unwrap();
            assert_eq!(encoded, expected, "wire bytes of {response:?}");
            let decoded: Response = serde_json::from_str(expected).unwrap();
            assert_eq!(decoded, response, "decode of pinned literal");
        }
    }

    // what this catches: the EXACT wire bytes of `PeerIdentityCard` per
    // shape — `kind` tag, `card` field, nested `value_json`/`version`
    // field names all literal. Both shapes covered: a populated card,
    // and the never-published shape where `card` is ABSENT (not `null`),
    // mirroring `RoomTipResponse.tip`. A symmetric rename a round-trip
    // test would miss breaks an attached client ↔ daemon of another
    // version.
    #[test]
    fn peer_identity_card_response_wire_bytes_are_pinned_per_shape() {
        for (response, expected) in [
            (
                Response::PeerIdentityCard(PeerIdentityCardResponse {
                    card: Some(IpcIdentityCard {
                        value_json: r#"{"name":"Claude"}"#.to_string(),
                        version: 1_700_000_000_000,
                    }),
                }),
                r#"{"kind":"peer_identity_card","card":{"value_json":"{\"name\":\"Claude\"}","version":1700000000000}}"#,
            ),
            (
                Response::PeerIdentityCard(PeerIdentityCardResponse { card: None }),
                r#"{"kind":"peer_identity_card"}"#,
            ),
        ] {
            let encoded = serde_json::to_string(&response).unwrap();
            assert_eq!(encoded, expected, "wire bytes of {response:?}");
            let decoded: Response = serde_json::from_str(expected).unwrap();
            assert_eq!(decoded, response, "decode of pinned literal");
        }
    }

    /// Card 4b6a0ffa (#33): the EXACT wire bytes of `RouteEndpoints`
    /// are the cross-version contract — outer `kind` tag, `endpoints`
    /// field, and every endpoint variant's tag + field names pinned as
    /// literals. The endpoint shape deliberately matches
    /// `airc_lib::RouteEndpoint`'s serde shape (`kind`-tagged,
    /// snake_case) so the two sides of the CLI conversion can never
    /// drift apart silently: a rename on either side fails here.
    #[test]
    fn route_endpoints_response_wire_bytes_are_pinned_per_variant() {
        for (response, expected) in [
            (
                Response::RouteEndpoints(RouteEndpointsResponse {
                    endpoints: vec![
                        IpcRouteEndpoint::LanTcp {
                            addr: "10.0.0.2:7717".parse().expect("valid socket addr"),
                        },
                        IpcRouteEndpoint::TailscaleTcp {
                            addr: "100.64.0.7:7717".parse().expect("valid socket addr"),
                        },
                        IpcRouteEndpoint::Udp {
                            addr: "10.0.0.2:7718".parse().expect("valid socket addr"),
                        },
                        IpcRouteEndpoint::Relay {
                            url: "https://relay.example.test".to_string(),
                        },
                        IpcRouteEndpoint::Reticulum {
                            destination: "abcdef0123456789".to_string(),
                        },
                        IpcRouteEndpoint::WebRtcSignaling {
                            url: "wss://signal.example.test".to_string(),
                        },
                    ],
                }),
                r#"{"kind":"route_endpoints","endpoints":[{"kind":"lan_tcp","addr":"10.0.0.2:7717"},{"kind":"tailscale_tcp","addr":"100.64.0.7:7717"},{"kind":"udp","addr":"10.0.0.2:7718"},{"kind":"relay","url":"https://relay.example.test"},{"kind":"reticulum","destination":"abcdef0123456789"},{"kind":"web_rtc_signaling","url":"wss://signal.example.test"}]}"#,
            ),
            (
                Response::RouteEndpoints(RouteEndpointsResponse {
                    endpoints: Vec::new(),
                }),
                r#"{"kind":"route_endpoints","endpoints":[]}"#,
            ),
        ] {
            let encoded = serde_json::to_string(&response).expect("encode");
            assert_eq!(encoded, expected, "wire bytes of {response:?}");
            let decoded: Response = serde_json::from_str(expected).expect("decode");
            assert_eq!(decoded, response, "decode of pinned literal");
        }
    }

    #[test]
    fn event_response_carries_opaque_wire_bytes() {
        let response = Response::Event {
            envelope: vec![0xa, 0xb, 0xc],
        };
        let encoded = serde_json::to_string(&response).unwrap();
        let decoded: Response = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, response);
    }
}
