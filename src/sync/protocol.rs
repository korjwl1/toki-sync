/// toki-sync server-side binary protocol.
///
/// Wire types are defined in the shared `toki-sync-protocol` crate.
/// This module provides asynchronous (tokio::io) frame I/O.
use std::io;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

// Re-export all shared types so existing imports continue to work.
// Re-export all shared types. Some may only be used transitively by downstream
// modules, so suppress unused-import warnings.
#[allow(unused_imports)]
pub use toki_sync_protocol::{
    AuthErrPayload, AuthOkPayload, AuthPayload, GetLastTsPayload, LastTsPayload, MsgType,
    StoredEvent, SyncAckPayload, SyncBatchPayload, SyncErrPayload, SyncItem, MAX_PAYLOAD_SIZE,
    PROTOCOL_VERSION, SCHEMA_VERSION,
};

// ─── Async frame I/O ───────────────────────────────────────────────────────

/// Write a frame: [msg_type: u32 LE][payload_len: u32 LE][payload]
pub async fn write_frame<W>(w: &mut W, msg_type: MsgType, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    if payload.len() > MAX_PAYLOAD_SIZE as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "payload too large: {} bytes (max {MAX_PAYLOAD_SIZE})",
                payload.len()
            ),
        ));
    }
    let mut header = [0u8; 8];
    header[..4].copy_from_slice(&(msg_type as u32).to_le_bytes());
    header[4..].copy_from_slice(&(payload.len() as u32).to_le_bytes());
    w.write_all(&header).await?;
    w.write_all(payload).await?;
    w.flush().await?;
    Ok(())
}

pub async fn write_empty_frame<W>(w: &mut W, msg_type: MsgType) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
    write_frame(w, msg_type, &[]).await
}

/// Read a frame with a caller-supplied payload cap. Use this wherever the
/// peer is not yet authenticated: the length is attacker-controlled and is
/// allocated BEFORE the body arrives, so a pre-auth cap is the difference
/// between a few KiB and MAX_PAYLOAD_SIZE per connection.
pub async fn read_frame_limited<R>(r: &mut R, max_payload: u32) -> io::Result<(MsgType, Vec<u8>)>
where
    R: AsyncReadExt + Unpin,
{
    read_frame_inner(r, max_payload.min(MAX_PAYLOAD_SIZE)).await
}

/// Read a frame. Returns Err(InvalidData) if payload > MAX_PAYLOAD_SIZE.
pub async fn read_frame<R>(r: &mut R) -> io::Result<(MsgType, Vec<u8>)>
where
    R: AsyncReadExt + Unpin,
{
    read_frame_inner(r, MAX_PAYLOAD_SIZE).await
}

async fn read_frame_inner<R>(r: &mut R, max_payload: u32) -> io::Result<(MsgType, Vec<u8>)>
where
    R: AsyncReadExt + Unpin,
{
    let mut header = [0u8; 8];
    r.read_exact(&mut header).await?;

    let type_u32 = u32::from_le_bytes(header[..4].try_into().unwrap());
    let len = u32::from_le_bytes(header[4..].try_into().unwrap());

    let msg_type = MsgType::from_u32(type_u32).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unknown msg_type: {type_u32}"),
        )
    })?;

    if len > max_payload {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("payload too large: {len} bytes (max {max_payload})"),
        ));
    }

    // Chunked read: never pre-allocate the declared length. A peer that
    // announces a large frame and then stalls now holds only the bytes it
    // actually sent, not its claim.
    const CHUNK: usize = 64 * 1024;
    let mut payload = Vec::with_capacity((len as usize).min(CHUNK));
    let mut remaining = len as usize;
    while remaining > 0 {
        let take = remaining.min(CHUNK);
        let start = payload.len();
        payload.resize(start + take, 0);
        r.read_exact(&mut payload[start..]).await?;
        remaining -= take;
    }
    Ok((msg_type, payload))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Craft a raw frame with the given msg_type_u32 and payload_len_u32.
    fn make_raw_frame(msg_type: u32, payload_len: u32) -> Vec<u8> {
        let mut v = Vec::with_capacity(8);
        v.extend_from_slice(&msg_type.to_le_bytes());
        v.extend_from_slice(&payload_len.to_le_bytes());
        v
    }

    #[tokio::test]
    async fn test_read_frame_max_payload_rejected() {
        let oversized = MAX_PAYLOAD_SIZE + 1;
        let raw = make_raw_frame(MsgType::Ping as u32, oversized);
        let mut cursor = std::io::Cursor::new(raw);
        let result = read_frame(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_read_frame_exactly_max_payload_accepted() {
        let raw = make_raw_frame(MsgType::Ping as u32, MAX_PAYLOAD_SIZE);
        let mut cursor = std::io::Cursor::new(raw);
        let result = read_frame(&mut cursor).await;
        assert!(result.is_err());
        assert_ne!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_read_frame_unknown_msg_type_rejected() {
        let raw = make_raw_frame(999, 0);
        let mut cursor = std::io::Cursor::new(raw);
        let result = read_frame(&mut cursor).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidData);
    }

    #[tokio::test]
    async fn test_write_then_read_frame_roundtrip() {
        let mut buf = Vec::new();
        let payload = b"hello world";
        write_frame(&mut buf, MsgType::Ping, payload).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let (msg, data) = read_frame(&mut cursor).await.unwrap();
        assert_eq!(msg, MsgType::Ping);
        assert_eq!(data, payload);
    }
}
