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
    use crate::ipc::request::Request;

    #[tokio::test]
    async fn frame_round_trips_newline_bearing_payload() {
        let mut bytes = Vec::new();
        let request = Request::Send(crate::ipc::request::SendRequest {
            wire: "/tmp/airc-wire".into(),
            channel: uuid::Uuid::nil(),
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
}
