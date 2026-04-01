use anyhow::{Context, Result};
use super::{EventStore, ServerEvent, UserFilter};

/// ClickHouse-backed event store.
///
/// Uses ReplacingMergeTree(ts_ms) ORDER BY (device_id, msg_id) for automatic dedup.
/// Queries use FINAL keyword to get deduplicated results at read time.
///
/// ClickHouse merges happen asynchronously in the background, but FINAL forces
/// dedup at query time regardless of merge state.
pub struct ClickHouseEventStore {
    url: String,
    client: ureq::Agent,
}

impl ClickHouseEventStore {
    pub fn new(url: &str) -> Result<Self> {
        let client = ureq::AgentBuilder::new()
            .timeout(std::time::Duration::from_secs(30))
            .build();
        let store = ClickHouseEventStore {
            url: url.trim_end_matches('/').to_string(),
            client,
        };
        // Create table on startup (idempotent)
        store.create_table()?;
        Ok(store)
    }

    fn create_table(&self) -> Result<()> {
        let ddl = "
            CREATE TABLE IF NOT EXISTS toki_events (
                device_id String,
                user_id String,
                msg_id String,
                ts_ms Int64,
                provider String,
                model String,
                project String,
                input_tokens UInt64,
                output_tokens UInt64,
                cache_creation_input_tokens UInt64,
                cache_read_input_tokens UInt64,
                usage_total UInt64
            ) ENGINE = ReplacingMergeTree(ts_ms)
            ORDER BY (device_id, provider, msg_id)
        ";
        self.execute(ddl).context("create toki_events table")?;
        Ok(())
    }

    fn execute(&self, query: &str) -> Result<String> {
        let resp = self.client.post(&self.url)
            .set("Content-Type", "text/plain")
            .send_string(query)
            .map_err(|e| anyhow::anyhow!("ClickHouse query failed: {e}"))?;
        let body = resp.into_string().context("read ClickHouse response")?;
        Ok(body)
    }

    fn escape(s: &str) -> String {
        s.replace('\\', "\\\\")
         .replace('\'', "\\'")
         .replace('\0', "")
         .replace('\n', "\\n")
         .replace('\r', "\\r")
         .replace('\t', "\\t")
    }
}

#[async_trait::async_trait]
impl EventStore for ClickHouseEventStore {
    async fn upsert_events(&self, events: &[ServerEvent]) -> Result<()> {
        if events.is_empty() { return Ok(()); }

        // Build INSERT with VALUES — ClickHouse ReplacingMergeTree handles dedup
        let mut sql = String::from(
            "INSERT INTO toki_events (device_id, user_id, msg_id, ts_ms, provider, model, project, \
             input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, usage_total) VALUES "
        );

        for (i, e) in events.iter().enumerate() {
            if i > 0 { sql.push(','); }
            sql.push_str(&format!(
                "('{}','{}','{}',{},'{}','{}','{}',{},{},{},{},{})",
                Self::escape(&e.device_id),
                Self::escape(&e.user_id),
                Self::escape(&e.msg_id),
                e.ts_ms,
                Self::escape(&e.provider),
                Self::escape(&e.model),
                Self::escape(&e.project),
                e.input_tokens,
                e.output_tokens,
                e.cache_creation_input_tokens,
                e.cache_read_input_tokens,
                e.usage_total,
            ));
        }

        let client_url = self.url.clone();
        let client = self.client.clone();

        tokio::task::spawn_blocking(move || {
            client.post(&client_url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse INSERT failed: {e}"))?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn query_events(
        &self,
        since_ms: i64,
        until_ms: i64,
        filter: UserFilter,
    ) -> Result<Vec<ServerEvent>> {
        let user_clause = match &filter {
            UserFilter::Single(uid) => format!("AND user_id = '{}'", Self::escape(uid)),
            UserFilter::Multiple(uids) => {
                let list: Vec<String> = uids.iter().map(|u| format!("'{}'", Self::escape(u))).collect();
                format!("AND user_id IN ({})", list.join(","))
            }
            UserFilter::All => String::new(),
        };

        let sql = format!(
            "SELECT device_id, user_id, msg_id, ts_ms, provider, model, project, \
             input_tokens, output_tokens, cache_creation_input_tokens, cache_read_input_tokens, usage_total \
             FROM toki_events FINAL \
             WHERE ts_ms >= {since_ms} AND ts_ms < {until_ms} {user_clause} \
             ORDER BY ts_ms \
             FORMAT JSONEachRow"
        );

        let url = self.url.clone();
        let client = self.client.clone();

        tokio::task::spawn_blocking(move || {
            let resp = client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse SELECT failed: {e}"))?;
            let body = resp.into_string().context("read ClickHouse response")?;

            let mut events = Vec::new();
            for line in body.lines() {
                if line.is_empty() { continue; }
                let e: ServerEvent = serde_json::from_str(line)
                    .with_context(|| format!("parse ClickHouse row: {}", &line[..line.len().min(100)]))?;
                events.push(e);
            }
            Ok(events)
        })
        .await
        .context("spawn_blocking panicked")?
    }

    async fn cleanup_old_dedup(&self, _device_id: &str, _cutoff_ms: i64) -> Result<()> {
        // No-op: ClickHouse handles dedup via ReplacingMergeTree, no idx_msg to clean up.
        Ok(())
    }

    async fn delete_device_events(&self, device_id: &str) -> Result<()> {
        let sql = format!(
            "ALTER TABLE toki_events DELETE WHERE device_id = '{}'",
            Self::escape(device_id)
        );

        let url = self.url.clone();
        let client = self.client.clone();

        tokio::task::spawn_blocking(move || {
            client.post(&url)
                .set("Content-Type", "text/plain")
                .send_string(&sql)
                .map_err(|e| anyhow::anyhow!("ClickHouse DELETE failed: {e}"))?;
            Ok(())
        })
        .await
        .context("spawn_blocking panicked")?
    }
}
