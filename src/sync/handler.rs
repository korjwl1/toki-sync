use std::sync::Arc;

use anyhow::Result;
use tokio::net::TcpStream;

use crate::auth::JwtManager;
use crate::db::Database;
use crate::metrics::backend::MetricPoint;
use crate::metrics::backend::MetricsBackend;
use crate::metrics::VictoriaMetrics;
use super::protocol::*;

/// Handle a single TCP client connection.
pub async fn handle_connection(
    stream: TcpStream,
    db: Arc<Database>,
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

    // Schema version guard
    if auth.schema_version != SCHEMA_VERSION {
        // Best-effort: delete VM series so client can re-sync clean
        let _ = vm.delete_user_series(&auth.provider).await;

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

    // JWT verification
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

    // Find or create device
    let device_id = find_or_create_device(&db, &user_id, &auth.device_name).await?;

    // Ensure cursor row for this (device, provider)
    ensure_cursor(&db, &device_id, &provider).await?;

    // AUTH_OK
    let ok = AuthOkPayload { device_id: device_id.clone() };
    write_frame(&mut writer, MsgType::AuthOk, &bincode::serialize(&ok)?).await?;

    tracing::debug!("sync auth ok: user={user_id} device={device_id} provider={provider}");

    // ── Main loop ────────────────────────────────────────────────────────────
    loop {
        let (msg_type, payload) = match read_frame(&mut reader).await {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) if e.kind() == std::io::ErrorKind::InvalidData => {
                // MAX_PAYLOAD_SIZE exceeded or unknown msg_type — drop connection immediately
                tracing::warn!("dropping TCP connection: {e}");
                break;
            }
            Err(e) => return Err(e.into()),
        };

        match msg_type {
            MsgType::GetLastTs => {
                let ts = get_last_ts(&db, &device_id, &provider).await?;
                let p = LastTsPayload { ts_ms: ts };
                write_frame(&mut writer, MsgType::LastTs, &bincode::serialize(&p)?).await?;
            }

            MsgType::SyncBatch => {
                let batch: SyncBatchPayload = bincode::deserialize(&payload)?;
                match handle_sync_batch(&batch, &user_id, &device_id, &provider, &db, &vm).await {
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

async fn find_or_create_device(db: &Database, user_id: &str, device_name: &str) -> Result<String> {
    // Look up existing device
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM devices WHERE user_id = ? AND name = ?",
    )
    .bind(user_id)
    .bind(device_name)
    .fetch_optional(&db.pool)
    .await?;

    if let Some(id) = existing {
        let now = chrono::Utc::now().timestamp();
        sqlx::query("UPDATE devices SET last_seen_at = ? WHERE id = ?")
            .bind(now)
            .bind(&id)
            .execute(&db.pool)
            .await?;
        return Ok(id);
    }

    // Create new device
    let id  = uuid::Uuid::new_v4().to_string();
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT INTO devices (id, user_id, name, created_at, last_seen_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&id)
    .bind(user_id)
    .bind(device_name)
    .bind(now)
    .bind(now)
    .execute(&db.pool)
    .await?;

    tracing::info!("registered new device '{device_name}' for user={user_id} id={id}");
    Ok(id)
}

async fn ensure_cursor(db: &Database, device_id: &str, provider: &str) -> Result<()> {
    let now = chrono::Utc::now().timestamp();
    sqlx::query(
        "INSERT OR IGNORE INTO cursors (device_id, provider, last_ts, updated_at) VALUES (?, ?, 0, ?)",
    )
    .bind(device_id)
    .bind(provider)
    .bind(now)
    .execute(&db.pool)
    .await?;
    Ok(())
}

async fn get_last_ts(db: &Database, device_id: &str, provider: &str) -> Result<i64> {
    let ts: Option<i64> = sqlx::query_scalar(
        "SELECT last_ts FROM cursors WHERE device_id = ? AND provider = ?",
    )
    .bind(device_id)
    .bind(provider)
    .fetch_optional(&db.pool)
    .await?;
    Ok(ts.unwrap_or(0))
}

async fn handle_sync_batch(
    batch: &SyncBatchPayload,
    user_id: &str,
    device_id: &str,
    provider: &str,
    db: &Database,
    vm: &VictoriaMetrics,
) -> Result<i64> {
    if batch.items.is_empty() {
        return Ok(0);
    }

    // Build VM metric points from batch
    let points = build_metric_points(batch, user_id, provider);

    // Write to VM first — cursor MUST NOT advance on failure
    vm.write_batch(&points).await?;

    // VM write succeeded → advance cursor to max ts in this batch
    let max_ts = batch.items.iter().map(|i| i.ts_ms).max().unwrap_or(0);
    let now    = chrono::Utc::now().timestamp();

    sqlx::query(
        "UPDATE cursors SET last_ts = MAX(last_ts, ?), updated_at = ?
         WHERE device_id = ? AND provider = ?",
    )
    .bind(max_ts)
    .bind(now)
    .bind(device_id)
    .bind(provider)
    .execute(&db.pool)
    .await?;

    Ok(max_ts)
}

// ─── Metric point builder ─────────────────────────────────────────────────────

fn build_metric_points(batch: &SyncBatchPayload, user_id: &str, provider: &str) -> Vec<MetricPoint> {
    let empty = String::new();
    let mut points = Vec::with_capacity(batch.items.len() * 4);

    for item in &batch.items {
        let model   = batch.dict.get(&item.event.model_id).unwrap_or(&empty);
        let session = batch.dict.get(&item.event.session_id).unwrap_or(&empty);
        let project = batch.dict
            .get(&item.event.project_name_id)
            .filter(|s| !s.is_empty())
            .map(String::as_str)
            .unwrap_or("");

        let base: Vec<(String, String)> = vec![
            ("model".into(),    model.clone()),
            ("session".into(),  session.clone()),
            ("provider".into(), provider.to_string()),
            ("user_id".into(),  user_id.to_string()),
            ("project".into(),  project.to_string()),
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
            labels.push(("type".into(), (*type_label).to_string()));
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
            message_id: "msg-1".to_string(),
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
        let points = build_metric_points(&batch, "user-1", "claude_code");

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
            message_id: "msg-2".to_string(),
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
        let points = build_metric_points(&batch, "user-1", "claude_code");
        assert_eq!(points.len(), 4);
    }

    #[test]
    fn test_build_metric_points_user_label_present() {
        let item = SyncItem {
            ts_ms: 1_000,
            message_id: "msg-3".to_string(),
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
        let points = build_metric_points(&batch, "user-xyz", "claude_code");
        assert_eq!(points.len(), 1);
        let pt = &points[0];
        let user_label = pt.labels.iter().find(|(k, _)| k == "user_id").unwrap();
        assert_eq!(user_label.1, "user-xyz");
    }

    #[test]
    fn test_build_metric_points_empty_batch() {
        let batch  = make_batch(vec![]);
        let points = build_metric_points(&batch, "user-1", "claude_code");
        assert!(points.is_empty());
    }
}
