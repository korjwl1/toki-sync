/// toki-sync server-side binary protocol.
///
/// Frame format (all integers little-endian):
///   [4B msg_type: u32][4B payload_len: u32][payload bytes]
///
/// Must stay binary-compatible with toki/src/sync/protocol.rs.
use std::collections::HashMap;
use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Schema version the server expects. Clients must match.
pub const SCHEMA_VERSION: u32 = 2;

/// Max payload: 16 MiB
pub const MAX_PAYLOAD_SIZE: u32 = 16 * 1024 * 1024;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MsgType {
    Auth      = 1,
    AuthOk    = 2,
    AuthErr   = 3,
    GetLastTs = 4,
    LastTs    = 5,
    SyncBatch = 6,
    SyncAck   = 7,
    SyncErr   = 8,
    Ping      = 9,
    Pong      = 10,
}

impl MsgType {
    pub fn from_u32(v: u32) -> Option<Self> {
        match v {
            1  => Some(Self::Auth),
            2  => Some(Self::AuthOk),
            3  => Some(Self::AuthErr),
            4  => Some(Self::GetLastTs),
            5  => Some(Self::LastTs),
            6  => Some(Self::SyncBatch),
            7  => Some(Self::SyncAck),
            8  => Some(Self::SyncErr),
            9  => Some(Self::Ping),
            10 => Some(Self::Pong),
            _  => None,
        }
    }
}

// ─── Payload types (must match toki client bincode layout exactly) ─────────────

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthPayload {
    pub jwt: String,
    pub device_name: String,
    pub schema_version: u32,
    pub provider: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthOkPayload {
    pub device_id: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct AuthErrPayload {
    pub reason: String,
    pub reset_required: bool,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct LastTsPayload {
    pub ts_ms: i64,
}

/// Must match toki::common::types::StoredEvent field layout and types exactly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoredEvent {
    pub model_id: u32,
    pub session_id: u32,
    pub source_file_id: u32,
    pub project_name_id: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncItem {
    pub ts_ms: i64,
    pub message_id: String,
    pub event: StoredEvent,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncBatchPayload {
    pub items: Vec<SyncItem>,
    pub dict: HashMap<u32, String>,
    pub provider: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncAckPayload {
    pub last_ts_ms: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SyncErrPayload {
    pub reason: String,
}

// ─── Async frame I/O ─────────────────────────────────────────────────────────

/// Write a frame: [msg_type: u32 LE][payload_len: u32 LE][payload]
pub async fn write_frame<W>(w: &mut W, msg_type: MsgType, payload: &[u8]) -> io::Result<()>
where
    W: AsyncWriteExt + Unpin,
{
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

/// Read a frame. Returns Err(InvalidData) if payload > MAX_PAYLOAD_SIZE.
pub async fn read_frame<R>(r: &mut R) -> io::Result<(MsgType, Vec<u8>)>
where
    R: AsyncReadExt + Unpin,
{
    let mut header = [0u8; 8];
    r.read_exact(&mut header).await?;

    let type_u32 = u32::from_le_bytes(header[..4].try_into().unwrap());
    let len      = u32::from_le_bytes(header[4..].try_into().unwrap());

    let msg_type = MsgType::from_u32(type_u32).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, format!("unknown msg_type: {type_u32}"))
    })?;

    if len > MAX_PAYLOAD_SIZE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("payload too large: {len} bytes (max {MAX_PAYLOAD_SIZE})"),
        ));
    }

    let mut payload = vec![0u8; len as usize];
    if len > 0 {
        r.read_exact(&mut payload).await?;
    }
    Ok((msg_type, payload))
}
