pub mod fjall_store;
pub mod clickhouse;

use anyhow::Result;

/// A fully-resolved event stored on the server.
///
/// Unlike the local daemon's StoredEvent (which uses dict-compressed u32 IDs),
/// this stores resolved strings because dict IDs are session-scoped and would
/// conflict across devices.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ServerEvent {
    pub device_id: String,
    pub user_id: String,
    /// Bare message ID (without timestamp suffix). Used as dedup key.
    pub msg_id: String,
    pub ts_ms: i64,
    pub provider: String,
    pub model: String,
    pub project: String,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_creation_input_tokens: u64,
    pub cache_read_input_tokens: u64,
    /// Pre-computed usage total. For Claude: all 4 token types.
    /// For Codex: input + output only (cached_input ⊂ input, reasoning_output ⊂ output).
    #[serde(default)]
    pub usage_total: u64,
}

/// Scope filter for queries.
pub enum UserFilter {
    /// Single user (scope=self)
    Single(String),
    /// Multiple users (scope=team)
    Multiple(Vec<String>),
    /// All users (scope=all, admin only)
    All,
}

/// Event storage backend.
///
/// Two implementations:
/// - `FjallEventStore`: embedded LSM-tree, standalone mode.
///   Dedup via idx_msg secondary index (same pattern as local daemon).
/// - `ClickHouseEventStore`: external columnar DB.
///   Dedup via ReplacingMergeTree(ts_ms) ORDER BY (device_id, provider, msg_id).
///
/// Backend is chosen at startup via config (`events.backend`) and cannot
/// be switched at runtime. Data is NOT migrated between backends.
///
/// Key invariant: upsert_events is idempotent by (device_id, provider, msg_id).
/// Re-sending the same event (e.g., after crash recovery) produces the same result.
#[async_trait::async_trait]
pub trait EventStore: Send + Sync + 'static {
    /// Insert or update events. Deduplicates by (device_id, provider, msg_id):
    /// if an event with the same key already exists, it is replaced.
    async fn upsert_events(&self, events: &[ServerEvent]) -> Result<()>;

    /// Query events in [since_ms, until_ms) matching the user filter.
    async fn query_events(
        &self,
        since_ms: i64,
        until_ms: i64,
        filter: UserFilter,
    ) -> Result<Vec<ServerEvent>>;

    /// Delete all events for a specific device (used on schema mismatch reset).
    async fn delete_device_events(&self, device_id: &str) -> Result<()>;

    /// Clean up old dedup index entries for a device.
    /// Removes idx_msg entries whose event timestamp is older than cutoff_ms.
    async fn cleanup_old_dedup(&self, device_id: &str, cutoff_ms: i64) -> Result<()>;
}
