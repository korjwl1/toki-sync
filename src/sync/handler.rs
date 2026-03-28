use std::io::Read;
use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpStream;

use crate::auth::JwtManager;
use crate::db::DatabaseRepo;
use crate::metrics::backend::MetricPoint;
use crate::metrics::backend::MetricsBackend;
use crate::metrics::VictoriaMetrics;
use super::protocol::*;

const MAX_DECOMPRESSED_SIZE: usize = 64 * 1024 * 1024; // 64 MiB

/// Handle a single TCP client connection.
pub async fn handle_connection(
    stream: TcpStream,
    db: Arc<dyn DatabaseRepo>,
    jwt: Arc<JwtManager>,
    vm: Arc<VictoriaMetrics>,
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
    let device_id = find_or_create_device(&*db, &user_id, &auth.device_name, &auth.device_key).await?;

    // Schema version guard — delete only this device's series, not all devices for this provider
    if auth.schema_version != SCHEMA_VERSION {
        if let Err(e) = vm.delete_device_series(&device_id).await {
            tracing::warn!("failed to delete VM series for device {device_id}: {e}");
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
                match handle_sync_batch(&batch, &user_id, &device_id, &batch.provider, &*db, &vm).await {
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
    // Look up existing device by stable device_key UUID, scoped to user
    if let Some(id) = db.find_device_by_key_and_user(device_key, user_id).await? {
        db.update_device_seen(&id, device_name).await?;
        return Ok(id);
    }

    // Create new device
    let id = uuid::Uuid::new_v4().to_string();
    db.create_device(&id, user_id, device_name, device_key).await?;

    tracing::info!("registered new device '{device_name}' (key={device_key}) for user={user_id} id={id}");
    Ok(id)
}

async fn handle_sync_batch(
    batch: &SyncBatchPayload,
    user_id: &str,
    device_id: &str,
    provider: &str,
    db: &dyn DatabaseRepo,
    vm: &VictoriaMetrics,
) -> Result<i64> {
    if batch.items.is_empty() {
        let current = db.get_last_ts(device_id, provider).await?;
        return Ok(current);
    }

    // Build VM metric points from batch
    let points = build_metric_points(batch, user_id, device_id, provider);

    // Write to VM first — cursor MUST NOT advance on failure
    vm.write_batch(&points).await?;

    // VM write succeeded → advance cursor to max ts in this batch
    let max_ts = batch.items.iter().map(|i| i.ts_ms).max().unwrap_or(0);
    db.advance_cursor(device_id, provider, max_ts).await?;

    Ok(max_ts)
}

// ─── Prometheus value escaping ────────────────────────────────────────────────

fn escape_prom_value(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            c => out.push(c),
        }
    }
    out
}

// ─── Metric point builder ─────────────────────────────────────────────────────

fn build_metric_points(
    batch: &SyncBatchPayload,
    user_id: &str,
    device_id: &str,
    provider: &str,
) -> Vec<MetricPoint> {
    let empty = String::new();
    let mut points = Vec::with_capacity(batch.items.len() * 4);

    // Pre-escape values that are constant across the entire batch
    let esc_provider  = escape_prom_value(provider);
    let esc_user_id   = escape_prom_value(user_id);
    let esc_device_id = escape_prom_value(device_id);

    for item in &batch.items {
        let model   = batch.dict.get(&item.event.model_id).unwrap_or(&empty);
        let session = batch.dict.get(&item.event.session_id).unwrap_or(&empty);
        let project = batch.dict
            .get(&item.event.project_name_id)
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .unwrap_or("");

        let base: Vec<(String, String)> = vec![
            ("model".into(),    escape_prom_value(model)),
            ("session".into(),  escape_prom_value(session)),
            ("provider".into(), esc_provider.clone()),
            ("user".into(),     esc_user_id.clone()),
            ("device".into(),   esc_device_id.clone()),
            ("project".into(),  escape_prom_value(project)),
        ];

        let ts = item.ts_ms;

        let token_types: [(u64, &str); 4] = [
            (item.event.input_tokens,                  "input"),
            (item.event.output_tokens,                 "output"),
            (item.event.cache_creation_input_tokens,   "cache_create"),
            (item.event.cache_read_input_tokens,       "cache_read"),
        ];

        for (count, type_label) in &token_types {
            if *count == 0 { continue; }
            let mut labels = base.clone();
            // type_label is static known-safe ASCII — no escaping needed
            labels.push(("type".into(), type_label.to_string()));
            points.push(MetricPoint {
                name: "toki_tokens_total".to_string(),
                labels,
                value: *count as f64,
                timestamp_ms: ts,
            });
        }
    }

    points
}

// ─── Unit tests ──────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use super::*;

    fn make_batch(items: Vec<SyncItem>) -> SyncBatchPayload {
        let mut dict = HashMap::new();
        dict.insert(1u32, "claude-opus-4-6".to_string());
        dict.insert(2u32, "sess-abc".to_string());
        dict.insert(3u32, "/path/to/file.jsonl".to_string());
        dict.insert(4u32, "my-project".to_string());
        SyncBatchPayload { items, dict, provider: "claude_code".to_string() }
    }

    #[test]
    fn test_build_metric_points_basic() {
        let item = SyncItem {
            ts_ms: 1_700_000_000_000,
            event: StoredEvent {
                model_id:   1,
                session_id: 2,
                source_file_id:  3,
                project_name_id: 4,
                input_tokens:  100,
                output_tokens: 50,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens:     0,
            },
        };
        let batch  = make_batch(vec![item]);
        let points = build_metric_points(&batch, "user-1", "device-1", "claude_code");

        assert_eq!(points.len(), 2, "input + output only (cache=0 skipped)");

        let input_pt = points.iter().find(|p| p.labels.iter().any(|(k,v)| k == "type" && v == "input")).unwrap();
        assert_eq!(input_pt.value, 100.0);
        assert_eq!(input_pt.timestamp_ms, 1_700_000_000_000);

        let output_pt = points.iter().find(|p| p.labels.iter().any(|(k,v)| k == "type" && v == "output")).unwrap();
        assert_eq!(output_pt.value, 50.0);
    }

    #[test]
    fn test_build_metric_points_all_types() {
        let item = SyncItem {
            ts_ms: 1_000,
            event: StoredEvent {
                model_id:   1,
                session_id: 2,
                source_file_id:  3,
                project_name_id: 4,
                input_tokens:  10,
                output_tokens: 20,
                cache_creation_input_tokens: 5,
                cache_read_input_tokens:     3,
            },
        };
        let batch  = make_batch(vec![item]);
        let points = build_metric_points(&batch, "user-1", "device-1", "claude_code");
        assert_eq!(points.len(), 4);
    }

    #[test]
    fn test_build_metric_points_user_label_present() {
        let item = SyncItem {
            ts_ms: 1_000,
            event: StoredEvent {
                model_id:   1,
                session_id: 2,
                source_file_id:  3,
                project_name_id: 4,
                input_tokens:  1,
                output_tokens: 0,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
            },
        };
        let batch  = make_batch(vec![item]);
        let points = build_metric_points(&batch, "user-xyz", "device-1", "claude_code");
        assert_eq!(points.len(), 1);
        let pt = &points[0];
        let user_label = pt.labels.iter().find(|(k, _)| k == "user").unwrap();
        assert_eq!(user_label.1, "user-xyz");
        let device_label = pt.labels.iter().find(|(k, _)| k == "device").unwrap();
        assert_eq!(device_label.1, "device-1");
    }

    #[test]
    fn test_build_metric_points_empty_batch() {
        let batch  = make_batch(vec![]);
        let points = build_metric_points(&batch, "user-1", "device-1", "claude_code");
        assert!(points.is_empty());
    }
}
