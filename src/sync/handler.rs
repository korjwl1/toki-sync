use std::io::Read;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpStream;
use tokio::sync::Semaphore;

use crate::auth::JwtManager;
use crate::db::DatabaseRepo;
use crate::events::{EventStore, ServerEvent};
use super::protocol::*;

const MAX_DECOMPRESSED_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

/// Handle a single TCP client connection.
pub async fn handle_connection(
    stream: TcpStream,
    db: Arc<dyn DatabaseRepo>,
    jwt: Arc<JwtManager>,
    events: Arc<dyn EventStore>,
    batch_semaphore: Arc<Semaphore>,
) -> Result<()> {
    let (r, w) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(r);
    let mut writer = tokio::io::BufWriter::new(w);

    // ── AUTH ────────────────────────────────────────────────────────────────
    let (msg_type, payload) = read_frame(&mut reader).await?;
    if msg_type != MsgType::Auth {
        return Err(anyhow::anyhow!("expected AUTH, got {msg_type:?}"));
    }

    let auth: AuthPayload = bincode::deserialize(&payload)?;

    // Protocol version check — reject unknown versions immediately
    if auth.protocol_version != PROTOCOL_VERSION {
        let err = AuthErrPayload {
            reason: format!(
                "unsupported protocol version: client={}, server={}",
                auth.protocol_version, PROTOCOL_VERSION
            ),
            reset_required: false,
        };
        write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
        return Ok(());
    }

    // JWT verification first — we need user_id to scope any device operations
    let claims = match jwt.verify_access(&auth.jwt) {
        Ok(c) => c,
        Err(e) => {
            let err = AuthErrPayload {
                reason: format!("JWT invalid: {e}"),
                reset_required: false,
            };
            write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
            return Ok(());
        }
    };

    let user_id  = claims.sub.clone();
    let provider = auth.provider.clone();

    // Find or create device using the stable device_key UUID
    // Truncate device_name to 64 chars (hostname can be long)
    let device_name = if auth.device_name.len() > 64 {
        auth.device_name.chars().take(64).collect::<String>()
    } else {
        auth.device_name.clone()
    };
    let device_id = find_or_create_device(&*db, &user_id, &device_name, &auth.device_key).await?;

    // Schema version guard — delete this device's events and reset cursor
    if auth.schema_version != SCHEMA_VERSION {
        if let Err(e) = events.delete_device_events(&device_id).await {
            tracing::warn!("failed to delete events for device {device_id}: {e}");
        }
        // Reset server cursor so client re-syncs all data
        if let Err(e) = db.reset_cursor(&device_id, &provider).await {
            tracing::warn!("failed to reset cursor for device {device_id}: {e}");
        }

        let err = AuthErrPayload {
            reason: format!(
                "schema version mismatch: client={}, server={}",
                auth.schema_version, SCHEMA_VERSION
            ),
            reset_required: true,
        };
        write_frame(&mut writer, MsgType::AuthErr, &bincode::serialize(&err)?).await?;
        return Ok(());
    }

    // Ensure cursor row for this (device, provider)
    db.ensure_cursor(&device_id, &provider).await?;

    // AUTH_OK
    let ok = AuthOkPayload { device_id: device_id.clone() };
    write_frame(&mut writer, MsgType::AuthOk, &bincode::serialize(&ok)?).await?;

    tracing::debug!("sync auth ok: user={user_id} device={device_id} provider={provider}");

    // ── Main loop ────────────────────────────────────────────────────────────
    // Read timeout: 2 missed PING cycles (client sends every 60s) → disconnect.
    const READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

    loop {
        let (msg_type, payload) = match tokio::time::timeout(READ_TIMEOUT, read_frame(&mut reader)).await {
            Err(_elapsed) => {
                tracing::warn!("TCP read timeout ({READ_TIMEOUT:?}), dropping connection");
                break;
            }
            Ok(Ok(f)) => f,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Ok(Err(e)) if e.kind() == std::io::ErrorKind::InvalidData => {
                tracing::warn!("dropping TCP connection: {e}");
                break;
            }
            Ok(Err(e)) => return Err(e.into()),
        };

        match msg_type {
            MsgType::GetLastTs => {
                let get_ts: GetLastTsPayload = bincode::deserialize(&payload)?;
                let ts = db.get_last_ts(&device_id, &get_ts.provider).await?;
                let p = LastTsPayload { ts_ms: ts };
                write_frame(&mut writer, MsgType::LastTs, &bincode::serialize(&p)?).await?;
            }

            MsgType::SyncBatch | MsgType::SyncBatchZstd => {
                let raw = if msg_type == MsgType::SyncBatchZstd {
                    let decoder = zstd::stream::Decoder::new(payload.as_slice())
                        .map_err(|e| anyhow::anyhow!("zstd decoder init failed: {e}"))?;
                    let mut buf = Vec::new();
                    decoder.take(MAX_DECOMPRESSED_SIZE as u64 + 1).read_to_end(&mut buf)
                        .map_err(|e| anyhow::anyhow!("zstd decompress failed: {e}"))?;
                    if buf.len() > MAX_DECOMPRESSED_SIZE {
                        anyhow::bail!("decompressed payload exceeds {MAX_DECOMPRESSED_SIZE} bytes");
                    }
                    buf
                } else {
                    payload
                };
                let batch: SyncBatchPayload = bincode::deserialize(&raw)?;
                // Ensure cursor exists for this batch's provider (may differ from auth provider)
                db.ensure_cursor(&device_id, &batch.provider).await?;
                match handle_sync_batch(&batch, &user_id, &device_id, &batch.provider, &*db, &*events, &batch_semaphore).await {
                    Ok(last_ts) => {
                        let ack = SyncAckPayload { last_ts_ms: last_ts };
                        write_frame(&mut writer, MsgType::SyncAck, &bincode::serialize(&ack)?).await?;
                    }
                    Err(e) => {
                        tracing::warn!("sync_batch error for device={device_id}: {e}");
                        let err = SyncErrPayload { reason: e.to_string() };
                        write_frame(&mut writer, MsgType::SyncErr, &bincode::serialize(&err)?).await?;
                    }
                }
            }

            MsgType::Ping => {
                write_empty_frame(&mut writer, MsgType::Pong).await?;
            }

            other => {
                tracing::warn!("unexpected msg_type in main loop: {other:?}");
            }
        }
    }

    Ok(())
}

// ─── DB helpers ──────────────────────────────────────────────────────────────

async fn find_or_create_device(
    db: &dyn DatabaseRepo,
    user_id: &str,
    device_name: &str,
    device_key: &str,
) -> Result<String> {
    // Use client's device_key as the device ID directly.
    // This ensures the same physical device always has the same ID,
    // even after disable/re-enable or server DB rebuild.
    if let Some(id) = db.find_device_by_key_and_user(device_key, user_id).await? {
        db.update_device_seen(&id, device_name).await?;
        return Ok(id);
    }

    // New device: use device_key as ID (not a random UUID)
    db.create_device(device_key, user_id, device_name, device_key).await?;

    tracing::info!("registered device '{device_name}' (id={device_key}) for user={user_id}");
    Ok(device_key.to_string())
}

async fn handle_sync_batch(
    batch: &SyncBatchPayload,
    user_id: &str,
    device_id: &str,
    provider: &str,
    db: &dyn DatabaseRepo,
    events: &dyn EventStore,
    batch_semaphore: &Semaphore,
) -> Result<i64> {
    if batch.items.is_empty() {
        let current = db.get_last_ts(device_id, provider).await?;
        return Ok(current);
    }

    // Convert SyncItems to ServerEvents (resolve dict IDs to strings)
    let server_events: Vec<ServerEvent> = batch.items.iter().map(|item| {
        let model = batch.dict.get(&item.event.model_id).cloned().unwrap_or_else(|| {
            tracing::warn!("missing dict ID {} for model in device {}", item.event.model_id, device_id);
            String::new()
        });
        let project = batch.dict.get(&item.event.project_name_id)
            .filter(|s| !s.is_empty())
            .cloned()
            .unwrap_or_else(|| {
                if item.event.project_name_id != 0 {
                    tracing::warn!("missing dict ID {} for project in device {}", item.event.project_name_id, device_id);
                }
                String::new()
            });
        let bare_msg_id = item.message_id.split(':').next().unwrap_or(&item.message_id);

        // Map token columns by name (supports different providers)
        let mut se = ServerEvent {
            device_id: device_id.to_string(),
            user_id: user_id.to_string(),
            msg_id: bare_msg_id.to_string(),
            ts_ms: item.ts_ms,
            provider: provider.to_string(),
            model,
            project,
            input_tokens: 0,
            output_tokens: 0,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
            usage_total: 0,
        };

        for (i, col) in batch.token_columns.iter().enumerate() {
            if i >= item.event.tokens.len() { break; }
            match col.as_str() {
                "input" => se.input_tokens = item.event.tokens[i],
                "output" => se.output_tokens = item.event.tokens[i],
                "cache_create" => se.cache_creation_input_tokens = item.event.tokens[i],
                "cache_read" => se.cache_read_input_tokens = item.event.tokens[i],
                // Codex subsets: store in the corresponding fields but they're
                // already excluded from usage_total by the daemon
                "cached_input" => se.cache_read_input_tokens = item.event.tokens[i],
                "reasoning_output" => se.cache_creation_input_tokens = item.event.tokens[i],
                _ => {}
            }
        }

        se.usage_total = item.usage_total;

        se
    }).collect();

    // Acquire permit
    let _permit = batch_semaphore.acquire().await
        .map_err(|_| anyhow::anyhow!("batch semaphore closed"))?;

    // Write to EventStore — dedup by (device_id, msg_id) is handled internally
    events.upsert_events(&server_events).await?;

    drop(_permit);

    // Advance cursor to max ts in this batch
    let max_ts = batch.items.iter().map(|i| i.ts_ms).max().unwrap_or(0);
    db.advance_cursor(device_id, provider, max_ts).await?;

    Ok(max_ts)
}

