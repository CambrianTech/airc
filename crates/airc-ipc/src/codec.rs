//! Length-framed IPC codec for daemon RPC and attach streams.
//!
//! Local IPC is a byte stream on Unix sockets and Windows named pipes.
//! Newline-delimited JSON is not a protocol: any string field may
//! contain a newline, and a slow reader has no declared frame length.
//! This codec uses a fixed 4-byte big-endian length prefix followed by
//! a CBOR payload for the typed request/response enums.

use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum IPC frame payload. AIRC daemon requests are local control
/// messages, not blob transport; media stays content-addressed.
pub const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;

pub async fn write_frame<W, T>(writer: &mut W, value: &T) -> std::io::Result<()>
where
    W: AsyncWrite + Unpin,
    T: Serialize,
{
    let mut payload = Vec::new();
    ciborium::into_writer(value, &mut payload).map_err(invalid_data)?;
    let len = u32::try_from(payload.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "ipc frame too large: {} bytes exceeds {}",
                payload.len(),
                MAX_FRAME_BYTES
            ),
        )
    })?;
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ipc frame too large: {len} bytes exceeds {MAX_FRAME_BYTES}"),
        ));
    }

    writer.write_all(&len.to_be_bytes()).await?;
    writer.write_all(&payload).await?;
    writer.flush().await
}

pub async fn read_frame<R, T>(reader: &mut R) -> std::io::Result<Option<T>>
where
    R: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let Some(payload) = read_payload(reader).await? else {
        return Ok(None);
    };
    ciborium::from_reader(payload.as_slice())
        .map(Some)
        .map_err(invalid_data)
}

/// Decode ordinary responses unchanged; avoid Serde's per-byte tagged-enum
/// buffer only for the exact canonical event shape emitted by our writer.
pub async fn read_response_frame<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> std::io::Result<Option<crate::Response>> {
    let Some(payload) = read_payload(reader).await? else {
        return Ok(None);
    };
    decode_response(&payload).map(Some)
}

fn decode_response(payload: &[u8]) -> std::io::Result<crate::Response> {
    if let Some(envelope) = canonical_event(payload) {
        return Ok(crate::Response::Event { envelope });
    }
    // Never normalize the input before fallback: field order, tags, duplicate
    // fields and noncanonical representations retain existing acceptance rules.
    ciborium::from_reader(payload).map_err(invalid_data)
}

fn canonical_event(payload: &[u8]) -> Option<Vec<u8>> {
    let mut rest = payload.strip_prefix(b"\xa2\x64kind\x65event\x68envelope")?;
    let (&head, after) = rest.split_first()?;
    rest = after;
    let count = match head {
        0x80..=0x97 => usize::from(head - 0x80),
        0x98 => {
            let (&n, after) = rest.split_first()?;
            rest = after;
            if n < 24 {
                return None;
            }
            usize::from(n)
        }
        0x99 => {
            let bytes = rest.get(..2)?;
            let n = u16::from_be_bytes(bytes.try_into().ok()?);
            rest = &rest[2..];
            if n < 256 {
                return None;
            }
            usize::from(n)
        }
        0x9a => {
            let bytes = rest.get(..4)?;
            let n = u32::from_be_bytes(bytes.try_into().ok()?);
            rest = &rest[4..];
            if n < 65536 {
                return None;
            }
            usize::try_from(n).ok()?
        }
        _ => return None,
    };
    // Every u8 needs at least one input byte. Bound allocation by the validated
    // frame itself, not an untrusted declared array length.
    if count > rest.len() {
        return None;
    }
    let mut bytes = Vec::with_capacity(count);
    for _ in 0..count {
        let (&n, after) = rest.split_first()?;
        rest = after;
        match n {
            0..=23 => bytes.push(n),
            0x18 => {
                let (&n, after) = rest.split_first()?;
                if n < 24 {
                    return None;
                }
                rest = after;
                bytes.push(n);
            }
            _ => return None,
        }
    }
    if !rest.is_empty() {
        return None;
    }
    Some(bytes)
}

async fn read_payload<R: AsyncRead + Unpin>(reader: &mut R) -> std::io::Result<Option<Vec<u8>>> {
    let mut len_bytes = [0_u8; 4];
    match reader.read_exact(&mut len_bytes).await {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }

    let len = u32::from_be_bytes(len_bytes);
    if len > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("ipc frame too large: {len} bytes exceeds {MAX_FRAME_BYTES}"),
        ));
    }

    let mut payload = vec![0_u8; len as usize];
    reader.read_exact(&mut payload).await?;
    Ok(Some(payload))
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Request;

    #[test]
    fn canonical_event_matches_legacy_at_boundaries() {
        for len in [0, 1, 23, 24, 255, 256, 65535, 65536] {
            let envelope: Vec<u8> = (0..len).map(|n| (n % 256) as u8).collect();
            let mut payload = Vec::new();
            ciborium::into_writer(&crate::Response::event_ref(&envelope), &mut payload).unwrap();
            assert_eq!(canonical_event(&payload), Some(envelope.clone()));
            assert_eq!(
                decode_response(&payload).unwrap(),
                crate::Response::Event { envelope }
            );
            for end in [0, 1, payload.len() / 2, payload.len() - 1] {
                assert_eq!(
                    decode_response(&payload[..end]).is_ok(),
                    ciborium::from_reader::<crate::Response, _>(&payload[..end]).is_ok()
                );
            }
        }
    }

    #[test]
    fn altered_event_frames_preserve_legacy_decoder() {
        let mut original = Vec::new();
        ciborium::into_writer(&crate::Response::event_ref(&[0, 24, 255]), &mut original).unwrap();
        let compare = |bytes: &[u8]| {
            let old = ciborium::from_reader::<crate::Response, _>(bytes);
            let new = decode_response(bytes);
            assert_eq!(old.is_ok(), new.is_ok(), "{bytes:?}");
            if let (Ok(old), Ok(new)) = (old, new) {
                assert_eq!(old, new);
            }
        };
        for i in 0..original.len() {
            for b in 0..=255 {
                let mut bytes = original.clone();
                bytes[i] = b;
                compare(&bytes);
            }
        }
        for bytes in [
            b"\xa2\x68envelope\x80\x64kind\x65event".as_slice(),
            b"\xa2\x64kind\x7f\x62ev\x63ent\xff\x68envelope\x80".as_slice(),
            b"\xa2\x64kind\x65event\x68envelope\x81\xd8\x2a\x07".as_slice(),
            b"\xa2\x64kind\x65event\x68envelope\x40".as_slice(),
            b"\xa1\x64kind\x62ok".as_slice(),
        ] {
            assert!(canonical_event(bytes).is_none());
            compare(bytes);
        }
    }

    /// Event-only decoder experiment, not a replacement for Response.
    /// Same frames, serial means including allocations; not a CPU profile.
    #[tokio::test]
    #[ignore = "manual same-frame decoder comparison"]
    async fn bench_event_decoder_comparison() {
        use crate::response::Response;
        use std::{hint::black_box, time::Instant};
        #[derive(serde::Deserialize)]
        struct EventFields {
            kind: String,
            envelope: Vec<u8>,
        }
        const N: u128 = 512;
        for size in [384, 65_664] {
            let payload = vec![0x42; size];
            let mut frame = Vec::new();
            write_frame(&mut frame, &Response::event_ref(&payload))
                .await
                .unwrap();
            let direct: EventFields = read_frame(&mut frame.as_slice()).await.unwrap().unwrap();
            assert_eq!(direct.kind, "event");
            assert_eq!(direct.envelope, payload);
            assert_eq!(
                read_frame::<_, Response>(&mut frame.as_slice())
                    .await
                    .unwrap()
                    .unwrap(),
                Response::Event { envelope: payload }
            );
            let start = Instant::now();
            for _ in 0..N {
                black_box(
                    read_frame::<_, Response>(&mut black_box(frame.as_slice()))
                        .await
                        .unwrap()
                        .unwrap(),
                );
            }
            let tagged = start.elapsed();
            let start = Instant::now();
            for _ in 0..N {
                let direct = read_frame::<_, EventFields>(&mut black_box(frame.as_slice()))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(direct.kind, "event");
                black_box(direct);
            }
            eprintln!("same_frame envelope_bytes={size} framed_bytes={} tagged_mean_ns={} direct_fields_mean_ns={}",frame.len(),tagged.as_nanos()/N,start.elapsed().as_nanos()/N);
        }
    }

    // what this catches: replacing the existing byte-array encoding with CBOR
    // byte strings must first prove that already-installed Vec decoders accept it.
    #[tokio::test]
    async fn opaque_event_byte_string_compatibility_and_size() {
        struct ByteString<'a>(&'a [u8]);
        impl Serialize for ByteString<'_> {
            fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_bytes(self.0)
            }
        }
        #[derive(Serialize)]
        #[serde(tag = "kind", rename_all = "snake_case")]
        enum BytesResponse<'a> {
            Event { envelope: ByteString<'a> },
        }
        for size in [0, 23, 24, 255, 256, 16 * 1024, 1024 * 1024] {
            let payload: Vec<u8> = (0..size).map(|n| (n % 256) as u8).collect();
            let old = crate::response::Response::Event {
                envelope: payload.clone(),
            };
            let candidate = BytesResponse::Event {
                envelope: ByteString(&payload),
            };
            let mut old_frame = Vec::new();
            write_frame(&mut old_frame, &old).await.unwrap();
            let mut bytes_frame = Vec::new();
            write_frame(&mut bytes_frame, &candidate).await.unwrap();
            let old_decode =
                read_frame::<_, crate::response::Response>(&mut bytes_frame.as_slice()).await;
            eprintln!("opaque IPC payload={size} old_frame={} byte_string_frame={} old_decoder_accepts={}", old_frame.len(), bytes_frame.len(), old_decode.is_ok());
            assert!(bytes_frame.len() <= old_frame.len());
            let error = old_decode.expect_err("installed Vec readers reject CBOR byte strings");
            assert!(error.to_string().contains("expected a sequence"));

            // Borrowing the existing sequence representation removes the source
            // Vec allocation/copy while preserving every wire byte and JSON field.
            let borrowed = crate::response::Response::event_ref(&payload);
            let mut borrowed_frame = Vec::new();
            write_frame(&mut borrowed_frame, &borrowed).await.unwrap();
            assert_eq!(borrowed_frame, old_frame);
            assert_eq!(
                serde_json::to_vec(&borrowed).unwrap(),
                serde_json::to_vec(&old).unwrap()
            );
            let decoded: crate::response::Response = read_frame(&mut borrowed_frame.as_slice())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(decoded, old);
        }
    }

    #[tokio::test]
    async fn frame_round_trips_newline_bearing_payload() {
        let mut bytes = Vec::new();
        let request = Request::Send(crate::request::SendRequest {
            channel: uuid::Uuid::nil(),
            from_peer: uuid::Uuid::from_u128(0x1),
            from_client: uuid::Uuid::from_u128(0x2),
            text: "first line\nsecond line".to_string(),
            headers: airc_core::Headers::new(),
        });

        write_frame(&mut bytes, &request).await.unwrap();
        assert!(!bytes.ends_with(b"\n"));
        let decoded: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();

        assert_eq!(decoded, request);
    }

    #[tokio::test]
    async fn empty_stream_returns_none() {
        let decoded: Option<Request> = read_frame(&mut [].as_slice()).await.unwrap();

        assert!(decoded.is_none());
    }

    #[tokio::test]
    async fn oversized_frame_fails_before_allocating_payload() {
        let bytes = (MAX_FRAME_BYTES + 1).to_be_bytes().to_vec();

        let error = read_frame::<_, Request>(&mut bytes.as_slice())
            .await
            .unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("ipc frame too large"));
    }

    // ---------------------------------------------------------------
    // Card c6ea5d70 — IPC frame round-trip perf benchmarks.
    //
    // Every `airc` CLI command that hits the daemon pays
    // (encode + write + read + decode) on each side at least once.
    // The realistic shape: a CLI command → daemon Request, daemon
    // → Response. Both sides do one encode + one decode. The
    // headers/projection audit (#1077 + #1078) found the substrate
    // already fast at the pure-data layer; this is the boundary
    // layer where actual user-visible latency might live.
    // ---------------------------------------------------------------

    fn realistic_ping() -> Request {
        Request::Ping
    }

    fn realistic_status() -> Request {
        Request::Status
    }

    fn realistic_send() -> Request {
        // Modal CLI shape: a Send with a few-hundred-char body and
        // realistic header set. Mirrors what `airc msg "..."` lands
        // on the daemon for every chat publish.
        let mut headers = airc_core::Headers::new();
        headers.insert("airc.task.request".to_string(), "P0".to_string());
        headers.insert("continuum.widget".to_string(), "video-room".to_string());
        headers.insert("x-correlation".to_string(), "req-7e88c34d-1234".to_string());
        Request::Send(crate::request::SendRequest {
            channel: uuid::Uuid::from_u128(0xc0ffee),
            from_peer: uuid::Uuid::from_u128(0xa1),
            from_client: uuid::Uuid::from_u128(0xc1),
            text: "session update: shipped #1077 (headers bench), #1078 (projection bench), \
                   #1079 (UDS frame bench). Three perf audits, three 'substrate already \
                   fast' confirmations. The real perf gaps are at the boundary, not the \
                   core. Carded follow-ups: SQLite query batching, gh CLI batching."
                .to_string(),
            headers,
        })
    }

    #[tokio::test]
    async fn bench_ipc_frame_round_trip_ping() {
        // Smallest possible variant — pure framing + enum-tag CBOR.
        // Establishes the irreducible per-frame cost (~hundreds of ns).
        let request = realistic_ping();

        // Warmup.
        for _ in 0..1_000 {
            let mut bytes = Vec::with_capacity(64);
            write_frame(&mut bytes, &request).await.unwrap();
            let _: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();
        }

        const ITERS: u64 = 50_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..ITERS {
            let mut bytes = Vec::with_capacity(64);
            write_frame(&mut bytes, &request).await.unwrap();
            let decoded: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();
            sink = sink.wrapping_add(matches!(decoded, Request::Ping) as u64);
        }
        let elapsed = start.elapsed();
        let ns_per_op = elapsed.as_nanos() as u64 / ITERS;
        eprintln!(
            "card c6ea5d70: IPC frame round-trip Ping — {ITERS} iters in {elapsed:?}, \
             {ns_per_op} ns/op, sink={sink}"
        );

        // Floor: the empty-payload round-trip stays under 100μs.
        // M2 release measures ~hundreds of ns; the floor catches a
        // catastrophic regression.
        assert!(
            ns_per_op < 100_000,
            "Ping round-trip regressed to {ns_per_op} ns/op"
        );
    }

    #[tokio::test]
    async fn bench_ipc_frame_round_trip_send_with_headers() {
        // The realistic chat-publish round-trip: a Send Request
        // carrying a few-hundred-char body + 3 headers. Lands on
        // every `airc msg` invocation. The shape continuum's bridge
        // also pays when forwarding events between scopes.
        let request = realistic_send();

        for _ in 0..1_000 {
            let mut bytes = Vec::with_capacity(1024);
            write_frame(&mut bytes, &request).await.unwrap();
            let _: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();
        }

        const ITERS: u64 = 10_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..ITERS {
            let mut bytes = Vec::with_capacity(1024);
            write_frame(&mut bytes, &request).await.unwrap();
            let decoded: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();
            sink = sink.wrapping_add(matches!(decoded, Request::Send(_)) as u64);
        }
        let elapsed = start.elapsed();
        let ns_per_op = elapsed.as_nanos() as u64 / ITERS;
        eprintln!(
            "card c6ea5d70: IPC frame round-trip Send + 3 headers + 300-char body — \
             {ITERS} iters in {elapsed:?}, {ns_per_op} ns/op, sink={sink}"
        );

        assert!(
            ns_per_op < 100_000,
            "Send round-trip regressed to {ns_per_op} ns/op"
        );
    }

    #[tokio::test]
    async fn bench_ipc_frame_throughput_simulating_burst() {
        // What does a busy CLI burst look like — 1000 Ping-shaped
        // pings on a single round trip. Represents continuum's
        // bridge fanning rapid status queries during a busy room.
        let request = realistic_status();

        for _ in 0..1_000 {
            let mut bytes = Vec::with_capacity(64);
            write_frame(&mut bytes, &request).await.unwrap();
            let _: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();
        }

        const ITERS: u64 = 50_000;
        let start = std::time::Instant::now();
        let mut sink = 0u64;
        for _ in 0..ITERS {
            let mut bytes = Vec::with_capacity(64);
            write_frame(&mut bytes, &request).await.unwrap();
            let decoded: Request = read_frame(&mut bytes.as_slice()).await.unwrap().unwrap();
            sink = sink.wrapping_add(matches!(decoded, Request::Status) as u64);
        }
        let elapsed = start.elapsed();
        let ns_per_op = elapsed.as_nanos() as u64 / ITERS;
        let ops_per_sec = 1_000_000_000 / ns_per_op.max(1);
        eprintln!(
            "card c6ea5d70: IPC frame round-trip Status (throughput) — {ITERS} iters in {elapsed:?}, \
             {ns_per_op} ns/op, {ops_per_sec} ops/sec, sink={sink}"
        );

        assert!(
            ns_per_op < 100_000,
            "Status round-trip regressed to {ns_per_op} ns/op"
        );
    }
}
