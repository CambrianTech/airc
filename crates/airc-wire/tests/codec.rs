//! Acceptance suite for the airc wire codec. Each test gates one property
//! of the encoding called out in the crate's design brief.

use bytes::Bytes;
use uuid::Uuid;

use airc_bus::envelope::{DeliveryClass, Envelope, Kind, Seq, Target};
use airc_core::{ClientId, EventId, PeerId, RoomId};
use airc_wire::{decode, encode, WireEnvelope};

/// Build a fully-populated envelope at a deterministic position so tests can
/// assert exact field-for-field identity. Multi-entry headers, every
/// optional set, an explicit `seq` and `occurred_at_ms`.
fn full_envelope(kind: Kind, delivery: DeliveryClass, target: Target, payload: Bytes) -> Envelope {
    let mut e = Envelope::new(
        RoomId::from_u128(0xc0ffee),
        (PeerId::from_u128(0xa1), ClientId::from_u128(0xc1)),
        kind,
        delivery,
        payload,
    )
    .with_event_id(EventId::from_u128(0x550e8400_e29b_41d4_a716_446655440000))
    .with_target(target)
    .with_correlation_id(Uuid::from_u128(0x1234_5678))
    .with_coalesce_key("typing:peer-7")
    // BTreeMap ordering is exercised by inserting out of key order.
    .with_header("zeta", "last")
    .with_header("alpha", "first")
    .with_header("content-type", "application/x-pose");
    e.seq = Seq::new(3, 42);
    e.occurred_at_ms = 1_700_000_000_123;
    e
}

// ---------------------------------------------------------------------------
// 1. Round-trip per class — field-for-field equality incl. headers, every
//    Target variant, correlation_id Some+None, coalesce_key Some+None.
// ---------------------------------------------------------------------------

#[test]
fn round_trip_message_durable() {
    let e = full_envelope(
        Kind::Message,
        DeliveryClass::Durable,
        Target::Peer(PeerId::from_u128(0x99)),
        Bytes::from_static(b"hello world"),
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(got, e, "Message/Durable round-trips field-for-field");
}

#[test]
fn round_trip_event_ephemeral_latest() {
    let e = full_envelope(
        Kind::Event,
        DeliveryClass::EphemeralLatest,
        Target::All,
        Bytes::from_static(b"{typing}"),
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(got, e, "Event/EphemeralLatest round-trips field-for-field");
}

#[test]
fn round_trip_stream_chunk() {
    let e = full_envelope(
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Target::Endpoint("grid://render/0".to_string()),
        Bytes::from_static(&[0x00, 0x3f, 0x80, 0x00, 0x40, 0x49, 0x0f, 0xdb]),
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(
        got, e,
        "StreamChunk/StreamChunk round-trips field-for-field"
    );
}

#[test]
fn round_trip_command_request_response() {
    let e = full_envelope(
        Kind::Command,
        DeliveryClass::RequestResponse,
        Target::Capability("inference:gpu".to_string()),
        Bytes::from_static(b"screenshot"),
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(
        got, e,
        "Command/RequestResponse round-trips field-for-field"
    );
}

#[test]
fn round_trip_control() {
    let e = full_envelope(
        Kind::Control,
        DeliveryClass::EphemeralWindow,
        Target::Reply(Uuid::from_u128(0xdead_beef)),
        Bytes::from_static(b"cancel"),
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(got, e, "Control round-trips field-for-field");
}

#[test]
fn round_trip_command_result_and_signal_kinds() {
    // The remaining Kind variants not covered above, so every Kind is
    // exercised through the codec at least once.
    for kind in [Kind::CommandResult, Kind::Signal] {
        let e = full_envelope(
            kind,
            DeliveryClass::Durable,
            Target::All,
            Bytes::from_static(b"k"),
        );
        let got = decode(encode(&e)).expect("decode");
        assert_eq!(got, e, "{kind:?} round-trips");
    }
}

#[test]
fn round_trip_every_target_variant() {
    let targets = [
        Target::All,
        Target::Endpoint("env://lobby".to_string()),
        Target::Peer(PeerId::from_u128(0x7777)),
        Target::Reply(Uuid::from_u128(0x4242_4242_4242)),
        Target::Capability("audio:transcode".to_string()),
    ];
    for t in targets {
        let e = full_envelope(
            Kind::Message,
            DeliveryClass::Durable,
            t.clone(),
            Bytes::from_static(b"x"),
        );
        let got = decode(encode(&e)).expect("decode");
        assert_eq!(got.target, t, "Target::{t:?} round-trips exactly");
        assert_eq!(got, e, "whole envelope round-trips for Target::{t:?}");
    }
}

#[test]
fn round_trip_optionals_none() {
    // correlation_id None + coalesce_key None + empty headers — the bare
    // sender-authored shape with no optionals set.
    let e = Envelope::new(
        RoomId::from_u128(1),
        (PeerId::from_u128(2), ClientId::from_u128(3)),
        Kind::Message,
        DeliveryClass::Durable,
        Bytes::from_static(b"bare"),
    );
    assert!(e.correlation_id.is_none());
    assert!(e.coalesce_key.is_none());
    assert!(e.headers.is_empty());

    let got = decode(encode(&e)).expect("decode");
    assert_eq!(got, e, "None optionals + empty headers round-trip");
    assert!(got.correlation_id.is_none());
    assert!(got.coalesce_key.is_none());
    assert!(got.headers.is_empty());
}

#[test]
fn round_trip_multi_entry_headers_preserves_all_pairs() {
    let e = full_envelope(
        Kind::Message,
        DeliveryClass::Durable,
        Target::All,
        Bytes::from_static(b"h"),
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(got.headers.len(), 3);
    assert_eq!(got.headers.get("alpha").map(String::as_str), Some("first"));
    assert_eq!(got.headers.get("zeta").map(String::as_str), Some("last"));
    assert_eq!(
        got.headers.get("content-type").map(String::as_str),
        Some("application/x-pose")
    );
    assert_eq!(got.headers, e.headers, "every header pair preserved");
}

// ---------------------------------------------------------------------------
// 2. Opaque payload byte-identical — empty, binary (0x00/0xFF), and 1 MiB.
// ---------------------------------------------------------------------------

#[test]
fn payload_empty_round_trips_byte_identical() {
    let e = full_envelope(
        Kind::Message,
        DeliveryClass::Durable,
        Target::All,
        Bytes::new(),
    );
    let got = decode(encode(&e)).expect("decode");
    assert!(got.payload.is_empty(), "empty payload stays empty");
    assert_eq!(got.payload, e.payload);
}

#[test]
fn payload_binary_zero_and_ff_round_trips_byte_identical() {
    let raw: Vec<u8> = (0u16..=255).map(|b| b as u8).collect();
    let mut raw = raw;
    raw.extend_from_slice(&[0x00, 0x00, 0x00, 0xff, 0xff, 0xff]);
    let payload = Bytes::from(raw.clone());
    let e = full_envelope(
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Target::All,
        payload,
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(
        got.payload.as_ref(),
        raw.as_slice(),
        "binary payload incl. 0x00 and 0xFF bytes byte-identical"
    );
}

#[test]
fn payload_one_mib_round_trips_byte_identical() {
    let raw: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    let payload = Bytes::from(raw.clone());
    let e = full_envelope(
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Target::All,
        payload,
    );
    let got = decode(encode(&e)).expect("decode");
    assert_eq!(got.payload.len(), 1024 * 1024);
    assert_eq!(
        got.payload.as_ref(),
        raw.as_slice(),
        "1 MiB payload byte-for-byte"
    );
}

// ---------------------------------------------------------------------------
// 3. ZERO-COPY PAYLOAD — the decoded payload is a slice WITHIN the decoded
//    buffer's allocation (proves slice_ref, not byte-equality).
// ---------------------------------------------------------------------------

/// The half-open pointer range `[start, end)` covered by a byte slice.
fn ptr_range(bytes: &[u8]) -> (usize, usize) {
    let start = bytes.as_ptr() as usize;
    (start, start + bytes.len())
}

#[test]
fn decoded_payload_is_a_slice_within_the_buffer() {
    // A payload distinctive enough that an accidental copy elsewhere would
    // not happen to share an address.
    let raw: Vec<u8> = (0..4096).map(|i| (i % 97) as u8).collect();
    let e = full_envelope(
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Target::All,
        Bytes::from(raw),
    );

    let buf = encode(&e);
    let (buf_lo, buf_hi) = ptr_range(&buf);

    // decode consumes a clone of `buf`; because `Bytes::clone` shares the
    // same heap allocation, the decoded payload — if truly zero-copy — must
    // point INTO that same allocation (same as `buf`'s range).
    let got = decode(buf.clone()).expect("decode");
    let (pay_lo, pay_hi) = ptr_range(&got.payload);

    assert!(
        pay_lo >= buf_lo && pay_hi <= buf_hi,
        "payload pointer range [{pay_lo:#x}, {pay_hi:#x}) must lie within \
         buffer range [{buf_lo:#x}, {buf_hi:#x}) — proves slice_ref, no copy"
    );
    // And it is strictly inside (a FlatBuffer has a header before the
    // payload vector), not coincidentally aliasing the buffer start.
    assert!(pay_lo > buf_lo, "payload sits after the FlatBuffer header");
    assert_eq!(got.payload.len(), 4096);
}

#[test]
fn decoded_payload_shares_allocation_under_strong_count() {
    // Independent corroboration that no copy happened: holding the decoded
    // payload keeps the original buffer's allocation alive. We drop our
    // `buf` handle and the payload still reads correctly because it shares
    // the refcounted allocation.
    let raw: Vec<u8> = (0..2048).map(|i| (i % 13) as u8).collect();
    let e = full_envelope(
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Target::All,
        Bytes::from(raw.clone()),
    );
    let buf = encode(&e);
    let (buf_lo, buf_hi) = ptr_range(&buf);
    let got = decode(buf).expect("decode"); // move buf in; only payload keeps it alive
    let (pay_lo, pay_hi) = ptr_range(&got.payload);
    assert!(pay_lo >= buf_lo && pay_hi <= buf_hi);
    assert_eq!(&got.payload[..16], &raw[..16], "shared bytes still valid");
}

// ---------------------------------------------------------------------------
// 4. Zero-copy read view — WireEnvelope::payload() is a slice within the
//    buffer; routing reads don't allocate the whole Envelope.
// ---------------------------------------------------------------------------

#[test]
fn wire_view_payload_is_slice_within_buffer() {
    let raw: Vec<u8> = (0..512).map(|i| (i % 7) as u8).collect();
    let e = full_envelope(
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Target::Peer(PeerId::from_u128(0x5)),
        Bytes::from(raw),
    );
    let buf = encode(&e);
    let (buf_lo, buf_hi) = ptr_range(&buf);

    let view = WireEnvelope::read(&buf).expect("read view");
    let payload = view.payload().expect("payload accessor").expect("present");
    let (pay_lo, pay_hi) = ptr_range(payload);
    assert!(
        pay_lo >= buf_lo && pay_hi <= buf_hi,
        "WireEnvelope::payload() returns a slice within the buffer"
    );
    assert_eq!(payload.len(), 512);
}

#[test]
fn wire_view_reads_routing_fields_in_place() {
    let e = full_envelope(
        Kind::Event,
        DeliveryClass::EphemeralLatest,
        Target::Capability("inference:gpu".to_string()),
        Bytes::from_static(b"pose"),
    );
    let buf = encode(&e);
    let view = WireEnvelope::read(&buf).expect("read view");

    assert_eq!(view.kind().expect("kind"), Kind::Event);
    assert_eq!(
        view.delivery().expect("delivery"),
        DeliveryClass::EphemeralLatest
    );
    assert_eq!(view.channel().expect("channel"), e.channel);
    assert_eq!(view.event_id().expect("event_id"), e.event_id);
    assert_eq!(view.seq().expect("seq"), e.seq);
    assert_eq!(view.occurred_at_ms().expect("occurred"), e.occurred_at_ms);
    assert_eq!(view.correlation_id().expect("corr"), e.correlation_id);
    assert_eq!(view.coalesce_key().expect("ck"), e.coalesce_key.as_deref());
    assert_eq!(view.target().expect("target"), e.target);
}

#[test]
fn wire_view_headers_iterate_in_place() {
    let e = full_envelope(
        Kind::Message,
        DeliveryClass::Durable,
        Target::All,
        Bytes::from_static(b"h"),
    );
    let buf = encode(&e);
    let view = WireEnvelope::read(&buf).expect("read view");

    let mut pairs: Vec<(String, String)> = Vec::new();
    for entry in view.headers() {
        let (k, v) = entry.expect("header entry");
        // Each k/v is a borrow INTO the buffer (a &str slice of `buf`).
        pairs.push((k.to_string(), v.to_string()));
    }
    pairs.sort();
    assert_eq!(
        pairs,
        vec![
            ("alpha".to_string(), "first".to_string()),
            ("content-type".to_string(), "application/x-pose".to_string()),
            ("zeta".to_string(), "last".to_string()),
        ]
    );
}

// ---------------------------------------------------------------------------
// 5. Lean-header guardrail — a 16-byte-payload envelope encodes small so
//    per-packet overhead stays tiny for high-rate small packets.
// ---------------------------------------------------------------------------

/// Documented ceiling for a minimal (no headers, no optionals, default
/// target) envelope carrying a 16-byte payload. The four ids are 16 bytes
/// each (64 bytes), plus three u64 scalars and the FlatBuffer vtable/header.
/// 256 bytes leaves comfortable headroom while still being a hard guard
/// against accidental bloat (a verbose framing would blow past this).
const LEAN_CEILING: usize = 256;

#[test]
fn sixteen_byte_payload_envelope_stays_lean() {
    // A pose-style packet: a few floats. No headers, no optionals, default
    // target — the hot-path shape.
    let e = Envelope::new(
        RoomId::from_u128(0xabc),
        (PeerId::from_u128(0xa), ClientId::from_u128(0xb)),
        Kind::StreamChunk,
        DeliveryClass::StreamChunk,
        Bytes::from_static(&[0u8; 16]),
    );
    let buf = encode(&e);
    assert!(
        buf.len() <= LEAN_CEILING,
        "16-byte-payload envelope encoded to {} bytes, over the {} ceiling",
        buf.len(),
        LEAN_CEILING
    );
    // Sanity: it still round-trips.
    let got = decode(buf).expect("decode");
    assert_eq!(got, e);
}

// ---------------------------------------------------------------------------
// Misc robustness — malformed input is a typed error, not a panic.
// ---------------------------------------------------------------------------

#[test]
fn decode_garbage_is_typed_error_not_panic() {
    let garbage = Bytes::from_static(&[0xff, 0xff, 0xff, 0xff, 0x00, 0x01]);
    let err = decode(garbage).expect_err("garbage must not decode");
    // Any WireError is fine; the point is no panic / no silent success.
    let _ = format!("{err}");
}

#[test]
fn decode_empty_buffer_is_typed_error() {
    let err = decode(Bytes::new()).expect_err("empty buffer must not decode");
    let _ = format!("{err}");
}
