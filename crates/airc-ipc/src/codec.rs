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
    ciborium::from_reader(payload.as_slice())
        .map(Some)
        .map_err(invalid_data)
}

fn invalid_data(error: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::request::Request;

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
