use anyhow::Result;
use super::{EventStore, ServerEvent, UserFilter};

/// ClickHouse-backed event store (future implementation).
///
/// Uses ReplacingMergeTree(ts_ms) ORDER BY (device_id, msg_id) for automatic dedup.
/// Queries use FINAL keyword to get deduplicated results.
///
/// Table schema:
/// ```sql
/// CREATE TABLE toki_events (
///     device_id String,
///     user_id String,
///     msg_id String,
///     ts_ms Int64,
///     provider String,
///     model String,
///     project String,
///     input_tokens UInt64,
///     output_tokens UInt64,
///     cache_creation_input_tokens UInt64,
///     cache_read_input_tokens UInt64
/// ) ENGINE = ReplacingMergeTree(ts_ms)
/// ORDER BY (device_id, msg_id)
/// ```
pub struct ClickHouseEventStore {
    _url: String,
}

impl ClickHouseEventStore {
    pub fn new(url: &str) -> Self {
        ClickHouseEventStore { _url: url.to_string() }
    }
}

#[async_trait::async_trait]
impl EventStore for ClickHouseEventStore {
    async fn upsert_events(&self, _events: &[ServerEvent]) -> Result<()> {
        anyhow::bail!("ClickHouse backend not yet implemented")
    }

    async fn query_events(
        &self,
        _since_ms: i64,
        _until_ms: i64,
        _filter: UserFilter,
    ) -> Result<Vec<ServerEvent>> {
        anyhow::bail!("ClickHouse backend not yet implemented")
    }

    async fn delete_device_events(&self, _device_id: &str) -> Result<()> {
        anyhow::bail!("ClickHouse backend not yet implemented")
    }
}
