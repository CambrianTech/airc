use super::*;

fn cursor(counter: u64) -> IpcCursor {
    IpcCursor {
        epoch: 1,
        counter,
        event_id: airc_core::EventId::from_u128(0xfeed),
    }
}

/// Card c0cb6cdc: every `AttachStart` survives the wire round-trip —
/// the legacy flag-pair encoding decodes back to the same typed start.
#[test]
fn attach_start_roundtrips_through_wire_encoding() {
    for start in [
        AttachStart::Live,
        AttachStart::After(cursor(7)),
        AttachStart::FromTranscriptStart,
    ] {
        let request = AttachRequest::new(airc_core::RoomId(Uuid::nil()), start);
        let encoded = serde_json::to_string(&request).unwrap();
        let decoded: AttachRequest = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.start(), start, "round-trip of {start:?}");
        assert_eq!(decoded, request);
    }
}

/// Card 13cae1db: the EXACT serialized bytes for each `AttachStart`
/// variant ARE the cross-version contract. The decode side is pinned
/// by the raw-JSON tests below; this pins the encode side, so a
/// SYMMETRIC serde rename (e.g. `from_now` → `start_now` on both the
/// field and these literals) still fails a test instead of silently
/// breaking old daemons that only speak the legacy flag pair.
#[test]
fn attach_start_wire_bytes_are_pinned_per_variant() {
    let channel = airc_core::RoomId(Uuid::nil());
    for (start, expected) in [
        (
            AttachStart::Live,
            r#"{"selection":{"select":"all"},"channel":"00000000-0000-0000-0000-000000000000","from_now":true,"headers":"any"}"#,
        ),
        (
            AttachStart::After(cursor(7)),
            r#"{"selection":{"select":"all"},"channel":"00000000-0000-0000-0000-000000000000","from":{"epoch":1,"counter":7,"event_id":"00000000-0000-0000-0000-00000000feed"},"headers":"any"}"#,
        ),
        (
            AttachStart::FromTranscriptStart,
            r#"{"selection":{"select":"all"},"channel":"00000000-0000-0000-0000-000000000000","headers":"any"}"#,
        ),
    ] {
        let encoded = serde_json::to_string(&AttachRequest::new(channel, start)).unwrap();
        assert_eq!(encoded, expected, "wire bytes of {start:?}");
    }
}

/// Card c0cb6cdc: a bare request from a pre-AttachStart client
/// (no `from`, no `from_now` on the wire) keeps its legacy meaning —
/// full transcript replay — so version-skewed peers interoperate.
#[test]
fn legacy_bare_attach_decodes_as_transcript_start() {
    let legacy = format!(
        r#"{{"selection":{{"select":"all"}},"channel":"{}"}}"#,
        Uuid::nil()
    );
    let decoded: AttachRequest = serde_json::from_str(&legacy).unwrap();
    assert_eq!(decoded.start(), AttachStart::FromTranscriptStart);
    assert!(!decoded.coalesces_backlog());
}

/// Card c0cb6cdc: `from_now` wins over a cursor on the wire — the
/// precedence exists in exactly one place (`AttachRequest::start`),
/// pinned here so it never silently drifts.
#[test]
fn from_now_takes_precedence_over_cursor_on_the_wire() {
    let skewed = format!(
        r#"{{"selection":{{"select":"all"}},"channel":"{}","from":{},"from_now":true}}"#,
        Uuid::nil(),
        serde_json::to_string(&cursor(3)).unwrap(),
    );
    let decoded: AttachRequest = serde_json::from_str(&skewed).unwrap();
    assert_eq!(decoded.start(), AttachStart::Live);
}

// what this catches: the channel-set attach's wire contract. (a) The
// set and each room's own cursor survive the round-trip. (b) A
// single-channel request still omits `channels` entirely, so an older
// daemon that knows nothing of sets sees byte-identical requests from
// a newer client that chose the legacy shape. (c) A set entry with no
// cursor starts LIVE, never at the transcript start — N rooms of full
// history on one socket is not a shape a subscriber ever asked for.
#[test]
fn a_channel_set_attach_carries_each_rooms_own_cursor_and_hides_from_old_daemons() {
    let a = airc_core::RoomId(Uuid::from_u128(1));
    let b = airc_core::RoomId(Uuid::from_u128(2));
    let request = AttachRequest::channel_set(vec![
        ChannelAttach {
            channel: a,
            from: None,
        },
        ChannelAttach {
            channel: b,
            from: Some(cursor(9)),
        },
    ]);
    let encoded = serde_json::to_string(&request).unwrap();
    let decoded: AttachRequest = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, request);
    let set = decoded.channels().expect("a set attach");
    assert_eq!(set[0].start(), AttachStart::Live);
    assert_eq!(set[1].start(), AttachStart::After(cursor(9)));
    assert!(
        decoded.channel().is_none(),
        "a set attach names no single channel"
    );
    let parts = decoded.into_parts();
    assert_eq!(parts.channels.as_ref().map(Vec::len), Some(2));

    let single = serde_json::to_string(&AttachRequest::live(a)).unwrap();
    assert!(
        !single.contains("channels"),
        "legacy shape must not mention the set: {single}"
    );
}

/// Card c0cb6cdc: the dangerous start must be named — the safe
/// default of the typed enum is `Live`.
#[test]
fn attach_start_default_is_live() {
    assert_eq!(AttachStart::default(), AttachStart::Live);
}

#[test]
fn into_parts_moves_filters_with_decoded_start() {
    let parts = AttachRequest::new(
        airc_core::RoomId(Uuid::nil()),
        AttachStart::After(cursor(9)),
    )
    .with_kinds(vec![IpcKind::Command])
    .with_coalesced_backlog()
    .with_backlog_tail(2)
    .into_parts();
    assert_eq!(parts.start, AttachStart::After(cursor(9)));
    assert!(parts.coalesce_backlog);
    assert_eq!(parts.backlog_tail, Some(2));
    assert_eq!(parts.kinds.as_deref(), Some(&[IpcKind::Command][..]));
}

/// Card 7d5b6a65 extension wire-compat: `backlog_tail` never rides
/// the wire when unset (old daemons see the exact pre-extension
/// bytes — pinned above), and roundtrips intact when set.
// what this catches: a serde attribute regression that would leak
// `backlog_tail: null` into every attach and break old daemons'
// strict decoders, or drop the value on the daemon side.
#[test]
fn backlog_tail_is_omitted_when_unset_and_roundtrips_when_set() {
    let channel = airc_core::RoomId(Uuid::nil());
    let unset = serde_json::to_string(&AttachRequest::new(channel, AttachStart::Live)).unwrap();
    assert!(
        !unset.contains("backlog_tail"),
        "unset backlog_tail must be absent from the wire: {unset}"
    );

    let set = AttachRequest::new(channel, AttachStart::FromTranscriptStart)
        .with_coalesced_backlog()
        .with_backlog_tail(25);
    let decoded: AttachRequest =
        serde_json::from_str(&serde_json::to_string(&set).unwrap()).unwrap();
    assert_eq!(decoded.backlog_tail(), Some(25));
    assert!(decoded.coalesces_backlog());
}

#[test]
fn ping_serializes_compactly() {
    let encoded = serde_json::to_string(&Request::Ping).unwrap();
    assert_eq!(encoded, r#"{"op":"ping"}"#);
    let decoded: Request = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, Request::Ping);
}

#[test]
fn remove_peer_roundtrips_with_peer_id() {
    let peer_id = PeerId::from_u128(0xabc);
    let encoded =
        serde_json::to_string(&Request::RemovePeer(RemovePeerRequest { peer_id })).unwrap();
    let decoded: Request = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, Request::RemovePeer(RemovePeerRequest { peer_id }));
}

#[test]
fn send_roundtrips_with_typed_fields() {
    let original = Request::Send(SendRequest {
        class: airc_core::EventClass::Correspondence(airc_core::CorrespondenceKind::Message),
        channel: Uuid::nil(),
        from_peer: Uuid::from_u128(0x1),
        from_client: Uuid::from_u128(0x2),
        text: "hello".to_string(),
        headers: Headers::new(),
    });
    let encoded = serde_json::to_string(&original).unwrap();
    let decoded: Request = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn publish_roundtrips_with_typed_body_and_kind() {
    let original = Request::Publish(PublishRequest {
        class: airc_core::EventClass::Control(airc_core::ControlKind::Command),
        channel: Uuid::nil(),
        from_peer: Uuid::from_u128(0x1),
        from_client: Uuid::from_u128(0x2),
        kind: IpcKind::Command,
        delivery: IpcDelivery::RequestResponse,
        target: IpcTarget::Capability("inference:gpu".to_string()),
        correlation_id: Some(Uuid::from_u128(0xc0ffee)),
        coalesce_key: None,
        payload: vec![0xde, 0xad, 0xbe, 0xef],
        headers: Headers::new(),
    });
    let encoded = serde_json::to_string(&original).unwrap();
    let decoded: Request = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn publish_delivery_defaults_to_durable_when_absent() {
    // A client that omits `delivery` (simple chat publish) must
    // decode as Durable — the streaming classes are opt-in.
    let decoded: Request = serde_json::from_str(
        r#"{"op":"publish","channel":"00000000-0000-0000-0000-000000000000","from_peer":"00000000-0000-0000-0000-000000000001","from_client":"00000000-0000-0000-0000-000000000002","class":{"family":"correspondence","kind":"message"},"kind":"message","payload":[104,105]}"#,
    )
    .unwrap();
    match decoded {
        Request::Publish(p) => {
            assert_eq!(p.delivery, IpcDelivery::Durable);
            assert_eq!(p.payload, b"hi");
        }
        other => panic!("expected publish, got {other:?}"),
    }
}

#[test]
fn inbox_roundtrips_with_epoch_counter_cursor() {
    let original = Request::Inbox(InboxRequest {
        since: Some(IpcCursor {
            epoch: 3,
            counter: 17,
            event_id: airc_core::EventId::from_u128(0xdead_beef),
        }),
        channel: Some(airc_core::RoomId::from_u128(0x42)),
        limit: Some(64),
        kinds: None,
        before: None,
    });
    let encoded = serde_json::to_string(&original).unwrap();
    let decoded: Request = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, original);
}

/// Continuum #297: `kinds` wire contract, both halves.
// what this catches: a serde attribute regression that would leak
// `kinds: null` into every unfiltered inbox (breaking old daemons'
// strict decoders) or drop the kinds list on the daemon side —
// silently reverting the filter-before-limit fix to raw newest-N.
#[test]
fn inbox_kinds_omitted_when_none_and_roundtrips_when_set() {
    let unfiltered = Request::Inbox(InboxRequest {
        since: None,
        channel: Some(airc_core::RoomId::from_u128(0x42)),
        limit: Some(50),
        kinds: None,
        before: None,
    });
    let encoded = serde_json::to_string(&unfiltered).unwrap();
    assert!(
        !encoded.contains("kinds"),
        "unset kinds must be absent from the wire: {encoded}"
    );

    let filtered = Request::Inbox(InboxRequest {
        since: None,
        channel: Some(airc_core::RoomId::from_u128(0x42)),
        limit: Some(50),
        kinds: Some(vec![IpcKind::Message, IpcKind::Event]),
        before: None,
    });
    let decoded: Request =
        serde_json::from_str(&serde_json::to_string(&filtered).unwrap()).unwrap();
    assert_eq!(decoded, filtered);
}

/// Card a1562dbc: the EXACT wire bytes of `RoomTip` are the
/// cross-version contract — `op` tag and `channel` field name
/// pinned as literal strings. A serde rename (even a symmetric
/// one) must fail here, not silently strand old daemons.
#[test]
fn room_tip_wire_bytes_are_pinned() {
    let request = Request::RoomTip(RoomTipRequest {
        channel: airc_core::RoomId(Uuid::nil()),
    });
    let encoded = serde_json::to_string(&request).unwrap();
    assert_eq!(
        encoded,
        r#"{"op":"room_tip","channel":"00000000-0000-0000-0000-000000000000"}"#
    );
}

/// Card a1562dbc: decode-side pin — the literal wire JSON an
/// already-shipped client would send must decode to the typed
/// variant. Pins the decode half independently of the encode half
/// so a symmetric rename cannot pass both.
#[test]
fn room_tip_literal_wire_json_decodes() {
    let decoded: Request = serde_json::from_str(
        r#"{"op":"room_tip","channel":"00000000-0000-0000-0000-000000000042"}"#,
    )
    .unwrap();
    assert_eq!(
        decoded,
        Request::RoomTip(RoomTipRequest {
            channel: airc_core::RoomId::from_u128(0x42),
        })
    );
}

/// Card a1562dbc: adding the `RoomTip` variant must not perturb the
/// decoding of pre-existing ops — the literal pre-RoomTip wire
/// bytes of a neighbouring op still decode unchanged.
#[test]
fn room_tip_addition_keeps_existing_inbox_wire_compat() {
    let decoded: Request = serde_json::from_str(
        r#"{"op":"inbox","channel":"00000000-0000-0000-0000-000000000042","limit":1}"#,
    )
    .unwrap();
    assert_eq!(
        decoded,
        Request::Inbox(InboxRequest {
            since: None,
            channel: Some(airc_core::RoomId::from_u128(0x42)),
            limit: Some(1),
            kinds: None,
            before: None,
        })
    );
}

// what this catches: the `peer_identity_card` op tag + `peer_id`
// field are the cross-version wire contract — a symmetric serde
// rename (which a round-trip test would miss) breaks an attached
// client talking to an older/newer daemon. Pinned as a literal on
// BOTH sides, independently.
#[test]
fn peer_identity_card_wire_bytes_are_pinned() {
    let request = Request::PeerIdentityCard(PeerIdentityCardRequest {
        peer_id: airc_core::PeerId::from_u128(0x7),
    });
    let encoded = serde_json::to_string(&request).unwrap();
    assert_eq!(
        encoded,
        r#"{"op":"peer_identity_card","peer_id":"00000000-0000-0000-0000-000000000007"}"#
    );
    let decoded: Request = serde_json::from_str(
        r#"{"op":"peer_identity_card","peer_id":"00000000-0000-0000-0000-000000000007"}"#,
    )
    .unwrap();
    assert_eq!(decoded, request);
}

/// Card 4b6a0ffa (#33): the EXACT wire bytes of `RouteEndpoints`
/// are the cross-version contract — the `op` tag pinned as a
/// literal on both the encode and decode side, so a symmetric
/// serde rename cannot pass.
#[test]
fn route_endpoints_wire_bytes_are_pinned() {
    let encoded = serde_json::to_string(&Request::RouteEndpoints).unwrap();
    assert_eq!(encoded, r#"{"op":"route_endpoints"}"#);
    let decoded: Request = serde_json::from_str(r#"{"op":"route_endpoints"}"#).unwrap();
    assert_eq!(decoded, Request::RouteEndpoints);
}

#[test]
fn stop_serializes_compactly() {
    assert_eq!(
        serde_json::to_string(&Request::Stop).unwrap(),
        r#"{"op":"stop"}"#
    );
}

#[test]
fn missing_or_unknown_semantics_are_rejected() {
    let request = Request::Publish(PublishRequest {
        class: airc_core::EventClass::Activity(airc_core::ActivityKind::WorkReceipt),
        channel: Uuid::nil(),
        from_peer: Uuid::nil(),
        from_client: Uuid::nil(),
        kind: IpcKind::Event,
        delivery: IpcDelivery::Durable,
        target: IpcTarget::All,
        correlation_id: None,
        coalesce_key: None,
        payload: vec![0xff, 0, 0x80],
        headers: Headers::new(),
    });
    let mut value = serde_json::to_value(request).unwrap();
    value.as_object_mut().unwrap().remove("class");
    assert!(serde_json::from_value::<Request>(value.clone()).is_err());
    value["class"] = serde_json::json!({"family":"activity", "kind":"unknown"});
    assert!(serde_json::from_value::<Request>(value).is_err());
    let mut attach = serde_json::to_value(AttachRequest::live(airc_core::RoomId::new())).unwrap();
    attach.as_object_mut().unwrap().remove("selection");
    assert!(serde_json::from_value::<AttachRequest>(attach).is_err());
}

#[test]
fn attach_selection_roundtrips_and_reaches_serving_parts() {
    let request = AttachRequest::live(airc_core::RoomId::new())
        .with_selection(airc_core::EventSelection::Correspondence);
    let decoded: AttachRequest =
        serde_json::from_slice(&serde_json::to_vec(&request).unwrap()).unwrap();
    assert_eq!(
        decoded.into_parts().selection,
        airc_core::EventSelection::Correspondence
    );
}
